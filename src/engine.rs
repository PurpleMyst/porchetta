mod apply;
mod capture;
mod diff;
pub mod merge;
pub mod resolver;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;
use gix::merge::tree::TreatAsUnresolved;
use log::{debug, info, trace};

use self::resolver::ConflictResolver;
use crate::manifest::Manifest;
use crate::store::{Branch, PorchettaStore, Remote};
use crate::ui;

pub struct PorchettaEngine {
    store: PorchettaStore,
    home: Utf8PathBuf,
    resolver: Box<dyn ConflictResolver>,
}

/// The three trees involved in a topic sync: the filesystem snapshot, the
/// reconciled repo state, and the result of merging them against the base.
struct TopicTrees {
    ours: gix::ObjectId,
    theirs: gix::ObjectId,
    merged: gix::ObjectId,
}

/// A branch's reconciled state for this sync.
#[derive(Clone, Copy)]
enum Reconciled {
    /// Local and remote heads were integrated into this head.
    Head(gix::ObjectId),
    /// No head exists locally or on any remote; sync must create the first
    /// commit, even for an empty tree, so the system branch has a head to mirror.
    NoHistory,
    /// A dry run found reconciliation conflicts; the filesystem sync is skipped.
    Conflict,
}

impl Reconciled {
    fn head(self) -> Option<gix::ObjectId> {
        match self {
            Self::Head(head) => Some(head),
            Self::NoHistory | Self::Conflict => None,
        }
    }
}

const fn status_label(dry_run: bool, captured: bool, applied: bool) -> &'static str {
    match (dry_run, captured, applied) {
        (true, true, true) => "would capture and apply",
        (true, true, false) => "would capture",
        (true, false, true) => "would apply",
        (_, false, false) => "unchanged",
        (false, true, true) => "captured and applied",
        (false, true, false) => "captured",
        (false, false, true) => "applied",
    }
}

fn format_change_count(verb: &str, count: usize, target: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{verb} {count} file change{plural} {target}")
}

#[derive(Default)]
struct SyncSummary {
    total: usize,
    unchanged: usize,
    changed: usize,
    conflicts: usize,
    stored: usize,
    applied: usize,
}

impl SyncSummary {
    fn record_conflict(&mut self) {
        self.total += 1;
        self.conflicts += 1;
    }

    fn record_synced(&mut self, stored: usize, applied: usize, changed: bool) {
        self.total += 1;
        if changed {
            self.changed += 1;
        } else {
            self.unchanged += 1;
        }
        self.stored += stored;
        self.applied += applied;
    }

    fn print(&self, dry_run: bool) {
        if self.conflicts > 0 {
            ui::warning(&format!(
                "Preview incomplete · {} conflict(s) need resolution",
                self.conflicts
            ));
            ui::warning("Run `porchetta sync` to resolve interactively.");
        } else {
            ui::header(if dry_run {
                "Preview complete"
            } else {
                "Local sync complete"
            });
            ui::success(&format!(
                "{} topics checked · {} changed · {} unchanged",
                self.total, self.changed, self.unchanged
            ));
        }
        ui::info(&format!(
            "  Store: {} file changes · machine: {} file changes",
            self.stored, self.applied
        ));
        if dry_run {
            ui::muted("  No changes are published during this dry run.");
        }
    }
}

impl PorchettaEngine {
    /// Creates an engine with the user's home directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined or is not valid UTF-8.
    pub fn new(store: PorchettaStore, resolver: impl ConflictResolver + 'static) -> Result<Self> {
        let home =
            Utf8PathBuf::try_from(dirs::home_dir().context("Could not determine home directory")?)
                .map_err(|e| anyhow::anyhow!("home directory is not valid UTF-8: {e}"))?;
        Ok(Self {
            store,
            home,
            resolver: Box::new(resolver),
        })
    }

    /// Creates an engine bound to a specific home directory (useful for tests).
    pub fn with_home(
        store: PorchettaStore,
        home: Utf8PathBuf,
        resolver: impl ConflictResolver + 'static,
    ) -> Self {
        Self {
            store,
            home,
            resolver: Box::new(resolver),
        }
    }

    // Visible for integration tests; not public API.
    #[doc(hidden)]
    #[must_use]
    pub fn store(&self) -> &PorchettaStore {
        &self.store
    }

    /// Edits the manifest using the provided editor function.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or writing the manifest fails.
    pub fn edit_manifest(&mut self, editor: impl FnOnce(&[u8]) -> Result<Vec<u8>>) -> Result<()> {
        debug!("Starting manifest edit");
        let manifest_content = self.store.read_manifest()?;
        let new_manifest_content = editor(&manifest_content)?;
        Manifest::load_with_home(&new_manifest_content, Some(&self.home))
            .context("Edited manifest is invalid")?;
        self.store.write_manifest(&new_manifest_content)?;
        debug!("Manifest edit completed");
        Ok(())
    }

    /// Synchronizes all topics between the local filesystem and the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined, if the manifest
    /// cannot be loaded, if hostname cannot be obtained, or if any file system operation,
    /// git operation, or conflict resolution fails.
    pub fn sync(&mut self, dry_run: bool, offline: bool) -> Result<()> {
        self.sync_topics(dry_run, offline, &[])
    }

    /// Synchronizes only `topics`; an empty slice synchronizes every topic.
    ///
    /// # Errors
    ///
    /// Returns an error if a named topic is unknown or synchronization fails.
    pub fn sync_topics(&mut self, dry_run: bool, offline: bool, topics: &[String]) -> Result<()> {
        debug!("Starting sync operation");
        let selected: Vec<&str> = topics
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let remotes = if offline {
            Vec::new()
        } else {
            self.store.remotes()?
        };
        for remote in &remotes {
            self.store
                .fetch(remote)
                .with_context(|| format!("Failed to fetch remote '{}'", remote.name()))?;
        }

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");
        ui::sync_header(&hostname, dry_run, offline, remotes.len());
        if !selected.is_empty() {
            ui::info(&format!("  Topics selected: {}", selected.join(", ")));
        }
        ui::header("Topics");

        let (manifest_head, manifest) = self.reconcile_manifest(&remotes, &hostname, dry_run)?;
        let defined: Vec<&str> = manifest.topics.iter().map(|t| t.name.as_str()).collect();
        let unknown: Vec<&str> = selected
            .iter()
            .copied()
            .filter(|name| !defined.contains(name))
            .collect();
        if !unknown.is_empty() {
            bail!(
                "Unknown topic(s) {}. Manifest defines: {}",
                unknown.join(", "),
                defined.join(", ")
            );
        }

        let (selected_topics, filtered_topics): (Vec<_>, Vec<_>) = manifest
            .topics
            .iter()
            .partition(|topic| selected.is_empty() || selected.contains(&topic.name.as_str()));

        let reconciled_topics = self.reconcile_topics(
            manifest_head,
            &selected_topics,
            &remotes,
            &hostname,
            dry_run,
        )?;

        let mut summary = SyncSummary::default();
        for info in selected_topics.iter().copied().filter(|t| t.enabled) {
            let state = reconciled_topics
                .get(&info.name)
                .copied()
                .with_context(|| format!("Topic '{}' was not reconciled", info.name))?;
            if matches!(state, Reconciled::Conflict) {
                summary.record_conflict();
                Self::report_topic(
                    &manifest,
                    &info.name,
                    "conflict",
                    "History conflict · needs resolution",
                );
                continue;
            }
            self.sync_topic(&manifest, info, &hostname, dry_run, state, &mut summary)?;
        }
        if !filtered_topics.is_empty() {
            let names: Vec<_> = filtered_topics.iter().map(|t| t.name.as_str()).collect();
            ui::info(&format!(
                "  Filtered out {} topic(s): {}",
                names.len(),
                names.join(", ")
            ));
        }
        summary.print(dry_run);
        if !dry_run {
            let topic_names: Vec<_> = selected_topics.iter().map(|t| t.name.as_str()).collect();
            self.publish(&remotes, &topic_names)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    fn reconcile_manifest(
        &mut self,
        remotes: &[Remote],
        hostname: &str,
        dry_run: bool,
    ) -> Result<(gix::ObjectId, Manifest)> {
        let local_manifest = self.store.head(&Branch::Manifest)?;
        let remote_manifest_heads = self.store.remote_heads(remotes, &Branch::Manifest)?;
        let manifest_head = match self.reconcile_head(
            "manifest",
            local_manifest,
            remote_manifest_heads,
            hostname,
            dry_run,
        )? {
            Reconciled::Head(head) => head,
            Reconciled::NoHistory => bail!("Manifest branch has no head"),
            Reconciled::Conflict => bail!("Interactive sync is required to reconcile manifest"),
        };
        let manifest_bytes = self.store.read_manifest_at(manifest_head)?;
        let manifest = Manifest::load_with_home(&manifest_bytes, Some(&self.home))
            .context("Reconciled manifest is invalid")?;
        info!("Loaded manifest with {} topics", manifest.topics.len());
        Ok((manifest_head, manifest))
    }

    fn reconcile_topics(
        &mut self,
        manifest_head: gix::ObjectId,
        topics: &[&crate::manifest::Topic],
        remotes: &[Remote],
        hostname: &str,
        dry_run: bool,
    ) -> Result<BTreeMap<String, Reconciled>> {
        let mut reconciled_topics = BTreeMap::new();
        for topic in topics {
            let branch = Branch::topic(&topic.name);
            let local_topic_head = self.store.head(&branch)?;
            let remote_heads = self.store.remote_heads(remotes, &branch)?;
            let state = self.reconcile_head(
                &format!("topic/{}", topic.name),
                local_topic_head,
                remote_heads,
                hostname,
                dry_run,
            )?;
            reconciled_topics.insert(topic.name.clone(), state);
        }

        if !dry_run {
            let mut updates = vec![(Branch::Manifest, manifest_head)];
            for (topic, state) in &reconciled_topics {
                if let Reconciled::Head(head) = state {
                    updates.push((Branch::topic(topic), *head));
                }
            }
            self.store.update_heads(&updates)?;
        }

        Ok(reconciled_topics)
    }

    fn publish(&self, remotes: &[Remote], topics: &[&str]) -> Result<()> {
        if remotes.is_empty() {
            return Ok(());
        }
        ui::header("Publication");
        let mut failures = Vec::new();
        for remote in remotes {
            match self.store.push_topics(remote, topics) {
                Ok(()) => ui::publication(remote.name(), None),
                Err(error) => {
                    ui::publication(remote.name(), Some(&error.to_string()));
                    failures.push(format!("{}: {error}", remote.name()));
                }
            }
        }
        if !failures.is_empty() {
            ui::warning("Publication incomplete. Local changes are retained.");
            ui::warning("Run `porchetta sync` to retry publication.");
            bail!(
                "Local synchronization succeeded, but publication failed for: {}",
                failures.join("; ")
            );
        }
        Ok(())
    }

    fn report_topic(manifest: &Manifest, name: &str, status: &str, detail: &str) {
        let width = manifest
            .topics
            .iter()
            .filter(|topic| topic.enabled)
            .map(|topic| topic.name.chars().count())
            .max()
            .unwrap_or(0);
        ui::topic(&format!("{name:<width$}"), status, detail);
    }

    fn sync_topic(
        &mut self,
        manifest: &crate::manifest::Manifest,
        info: &crate::manifest::Topic,
        hostname: &str,
        dry_run: bool,
        reconciled: Reconciled,
        summary: &mut SyncSummary,
    ) -> Result<()> {
        let name = &info.name;
        debug!("Syncing topic '{name}'");
        let topic_base = info.base_dir(&self.home);

        let Some(trees) =
            self.merge_topic_trees(manifest, info, hostname, &topic_base, reconciled, dry_run)?
        else {
            summary.record_conflict();
            return Ok(());
        };

        let changed_wrt_repo =
            matches!(reconciled, Reconciled::NoHistory) || trees.merged != trees.theirs;
        let changed_wrt_system = trees.merged != trees.ours;
        let merged_tree = self.store.repo().find_tree(trees.merged)?;
        let store_changes = self::diff::collect_file_changes(
            &self.store.repo().find_tree(trees.theirs)?,
            &merged_tree,
        )?;
        let machine_changes = self::diff::collect_file_changes(
            &self.store.repo().find_tree(trees.ours)?,
            &merged_tree,
        )?;

        if changed_wrt_repo && !dry_run {
            self.store.commit_topic_tree(
                name,
                hostname,
                trees.merged,
                format!("Sync topic '{name}' from '{hostname}'"),
            )?;
        }
        if changed_wrt_system
            && !dry_run
            && let Err(error) =
                self::apply::apply(self.store.repo(), info, &topic_base, &machine_changes)
                    .with_context(|| format!("Failed to apply changes for topic '{name}'"))
        {
            if changed_wrt_repo {
                Self::report_topic(
                    manifest,
                    name,
                    "captured",
                    &format_change_count("Captured", store_changes.len(), "into store"),
                );
                self.print_file_changes(&store_changes, None);
            }
            ui::error(&format!(
                "Topic '{name}' incomplete · machine changes may be partially applied"
            ));
            return Err(error);
        }

        let mut details = Vec::new();
        if changed_wrt_repo {
            let verb = if dry_run { "Would capture" } else { "Captured" };
            details.push(format_change_count(verb, store_changes.len(), "into store"));
        }
        if changed_wrt_system {
            let verb = if dry_run { "Would apply" } else { "Applied" };
            details.push(format_change_count(
                verb,
                machine_changes.len(),
                "to machine",
            ));
        }
        if details.is_empty() {
            details.push("Unchanged".into());
        }
        let status = status_label(dry_run, changed_wrt_repo, changed_wrt_system);
        Self::report_topic(manifest, name, status, &details.join(" · "));
        self.print_file_changes(&store_changes, None);
        self.print_file_changes(&machine_changes, Some(&topic_base));

        if !dry_run {
            self.store.update_topic_hostname_head(name, hostname)?;
        }

        info!("Synchronized topic '{name}'");
        summary.record_synced(
            store_changes.len(),
            machine_changes.len(),
            changed_wrt_repo || changed_wrt_system,
        );
        Ok(())
    }

    fn print_file_changes(
        &self,
        changes: &[self::diff::FileChange],
        base: Option<&camino::Utf8Path>,
    ) {
        for change in changes {
            if let Some(base) = base {
                let path = base.join(&change.relative_path);
                let display = path
                    .strip_prefix(&self.home)
                    .map_or_else(|_| path.to_string(), |relative| format!("~/{relative}"));
                ui::file_change("Machine", change.action(), &display);
            } else {
                ui::file_change("Store", change.action(), change.relative_path.as_str());
            }
        }
    }

    /// Snapshots the topic's files and three-way merges them against the
    /// reconciled repo state and this host's last-applied base.
    ///
    /// Returns `None` when a dry run hits unresolved conflicts (already reported).
    fn merge_topic_trees(
        &mut self,
        manifest: &crate::manifest::Manifest,
        info: &crate::manifest::Topic,
        hostname: &str,
        topic_base: &camino::Utf8Path,
        reconciled: Reconciled,
        dry_run: bool,
    ) -> Result<Option<TopicTrees>> {
        let name = &info.name;
        let ours = self::capture::snapshot_topic(self.store.repo(), manifest, info, topic_base)?;
        let theirs = self.store.tree_oid_at(reconciled.head())?;
        let system_head = self.store.head(&Branch::system(hostname, name))?;
        let base = self.store.tree_oid_at(system_head)?;
        let mut merge_outcome =
            self::merge::merge_trees(self.store.repo(), base, ours, theirs, name)?;

        if dry_run && merge_outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
            Self::report_topic(manifest, name, "conflict", "Needs resolution");
            self::merge::show_conflicts(&merge_outcome);
            ui::muted("  run `porchetta sync` to resolve interactively");
            return Ok(None);
        }

        if !dry_run {
            self::merge::resolve_conflicts(
                self.store.repo(),
                self.resolver.as_ref(),
                &mut merge_outcome,
            )?;
        }

        let merged: gix::ObjectId = merge_outcome.tree.write()?.into();
        trace!("Merged tree: {merged}");
        Ok(Some(TopicTrees {
            ours,
            theirs,
            merged,
        }))
    }

    /// Integrates the local and remote heads of one branch into a single head.
    ///
    /// Heads already contained in another head's history are skipped; the rest are
    /// merged in sequence, local first. A dry run stops at the first conflict and
    /// reports it instead of resolving interactively.
    fn reconcile_head(
        &mut self,
        name: &str,
        local: Option<gix::ObjectId>,
        remote_heads: Vec<gix::ObjectId>,
        hostname: &str,
        dry_run: bool,
    ) -> Result<Reconciled> {
        let mut heads: BTreeSet<_> = local.into_iter().chain(remote_heads).collect();
        let all: Vec<_> = heads.iter().copied().collect();
        for candidate in &all {
            for other in &all {
                if candidate != other
                    && self::merge::is_ancestor(self.store.repo(), *candidate, *other)?
                {
                    heads.remove(candidate);
                    break;
                }
            }
        }

        // Integrate the local head first so merges read as local-vs-remote.
        let mut ordered = Vec::with_capacity(heads.len());
        if let Some(local) = local.filter(|head| heads.remove(head)) {
            ordered.push(local);
        }
        ordered.extend(heads);

        let Some(mut integrated) = ordered.first().copied() else {
            return Ok(Reconciled::NoHistory);
        };
        for theirs in ordered.into_iter().skip(1) {
            let mut outcome =
                self::merge::merge_commits(self.store.repo(), integrated, theirs, name)?;
            if dry_run && outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
                ui::error(&format!("Conflicts while reconciling {name}"));
                self::merge::show_conflicts(&outcome);
                ui::muted("  run `porchetta sync` to resolve interactively");
                return Ok(Reconciled::Conflict);
            }
            if !dry_run {
                self::merge::resolve_conflicts(
                    self.store.repo(),
                    self.resolver.as_ref(),
                    &mut outcome,
                )?;
            }
            let tree = outcome.tree.write()?;
            integrated = self.store.commit_tree(
                tree,
                [integrated, theirs],
                format!("Reconcile {name} on '{hostname}'"),
            )?;
        }
        Ok(Reconciled::Head(integrated))
    }
}
