mod apply;
mod capture;
mod diff;
pub mod merge;
pub mod resolver;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use gix::merge::tree::TreatAsUnresolved;
use log::{debug, info, trace};

use self::resolver::{ConflictResolver, SymlinkConflictResolution};
use crate::manifest::{Manifest, Symlink, Topic};
use crate::store::{Branch, PorchettaStore, Remote};
use crate::ui;

/// A branch's reconciled state in the ODB for this sync.
#[derive(Clone, Copy)]
enum Reconciled {
    /// Local and remote heads were integrated into this head.
    Head(gix::ObjectId),
    /// No head exists locally or on any remote; sync must create the first
    /// commit, even for an empty tree, so the system branch has a head to mirror.
    NoHistory,
    /// Unresolved reconciliation conflicts when no interactive resolver is active.
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

/// Pure ODB plan for a single topic before ref or filesystem mutations.
enum TopicPlan {
    Conflict,
    Ready {
        merged: gix::ObjectId,
        commit_to_repo: bool,
        store_changes: Vec<self::diff::FileChange>,
        machine_changes: Vec<self::diff::FileChange>,
        symlink_changes: Vec<Symlink>,
    },
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

impl PorchettaStore {
    /// Synchronizes all topics between the user's home directory and the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined or synchronization fails.
    pub fn sync(
        &self,
        dry_run: bool,
        offline: bool,
        resolver: &dyn ConflictResolver,
    ) -> Result<()> {
        self.sync_topics(dry_run, offline, &[], resolver)
    }

    /// Synchronizes only `topics` (or all topics when empty) between the user's home directory and the store.
    ///
    /// # Errors
    ///
    /// Returns an error if a named topic is unknown or synchronization fails.
    pub fn sync_topics(
        &self,
        dry_run: bool,
        offline: bool,
        topics: &[String],
        resolver: &dyn ConflictResolver,
    ) -> Result<()> {
        let home = crate::util::home_dir()?;
        self.sync_at(&home, dry_run, offline, topics, resolver)
    }

    /// Synchronizes `topics` (or all topics when empty) against a specific `home` directory.
    ///
    /// # Errors
    ///
    /// Returns an error if a named topic is unknown or synchronization fails.
    pub fn sync_at(
        &self,
        home: &Utf8Path,
        dry_run: bool,
        offline: bool,
        topics: &[String],
        resolver: &dyn ConflictResolver,
    ) -> Result<()> {
        debug!("Starting sync operation");
        let selected: Vec<&str> = topics
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let remotes = if offline { Vec::new() } else { self.remotes()? };
        self.fetch_remotes(&remotes)?;

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

        let active_resolver = (!dry_run).then_some(resolver);
        let (manifest_head, manifest) =
            self.reconcile_manifest(home, &remotes, &hostname, active_resolver)?;
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

        let topics: Vec<_> = manifest
            .topics
            .iter()
            .filter(|topic| selected.is_empty() || selected.contains(&topic.name.as_str()))
            .collect();

        let reconciled_topics =
            self.reconcile_topics(&topics, &remotes, &hostname, active_resolver)?;
        if !dry_run {
            let mut updates = vec![(Branch::Manifest, manifest_head)];
            for (topic, state) in &reconciled_topics {
                if let Reconciled::Head(head) = state {
                    updates.push((Branch::topic(topic), *head));
                }
            }
            self.update_heads(&updates)?;
        }

        let mut summary = SyncSummary::default();
        for info in topics.iter().copied().filter(|t| t.enabled) {
            let state = reconciled_topics
                .get(&info.name)
                .copied()
                .with_context(|| format!("Topic '{}' was not reconciled", info.name))?;
            let plan = self.plan_topic(home, &manifest, info, &hostname, state, active_resolver)?;
            if !dry_run {
                self.execute_topic_plan(home, &manifest, info, &hostname, &plan)?;
            }
            Self::report_topic_plan(&manifest, info, dry_run, &plan, &mut summary);
        }

        summary.print(dry_run);
        if !dry_run {
            let topic_names: Vec<_> = topics.iter().map(|t| t.name.as_str()).collect();
            self.publish(&remotes, &topic_names)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    fn reconcile_manifest(
        &self,
        home: &Utf8Path,
        remotes: &[Remote],
        hostname: &str,
        resolver: Option<&dyn ConflictResolver>,
    ) -> Result<(gix::ObjectId, Manifest)> {
        let local_manifest = self.head(&Branch::Manifest)?;
        let remote_manifest_heads = self.remote_heads(remotes, &Branch::Manifest)?;
        let manifest_head = match self.reconcile_head(
            "manifest",
            local_manifest,
            remote_manifest_heads,
            hostname,
            resolver,
        )? {
            Reconciled::Head(head) => head,
            Reconciled::NoHistory => bail!("Manifest branch has no head"),
            Reconciled::Conflict => bail!("Interactive sync is required to reconcile manifest"),
        };
        let manifest_bytes = self.read_manifest_at(manifest_head)?;
        let manifest = Manifest::load_with_home(&manifest_bytes, Some(home))
            .context("Reconciled manifest is invalid")?;
        info!("Loaded manifest with {} topics", manifest.topics.len());
        Ok((manifest_head, manifest))
    }

    fn reconcile_topics(
        &self,
        topics: &[&Topic],
        remotes: &[Remote],
        hostname: &str,
        resolver: Option<&dyn ConflictResolver>,
    ) -> Result<BTreeMap<String, Reconciled>> {
        let mut reconciled_topics = BTreeMap::new();
        for topic in topics {
            let branch = Branch::topic(&topic.name);
            let local_topic_head = self.head(&branch)?;
            let remote_heads = self.remote_heads(remotes, &branch)?;
            let state = self.reconcile_head(
                &format!("topic/{}", topic.name),
                local_topic_head,
                remote_heads,
                hostname,
                resolver,
            )?;
            reconciled_topics.insert(topic.name.clone(), state);
        }
        Ok(reconciled_topics)
    }

    fn publish(&self, remotes: &[Remote], topics: &[&str]) -> Result<()> {
        if remotes.is_empty() {
            return Ok(());
        }
        ui::header("Publication");
        let mut failures = Vec::new();
        for (remote, result) in remotes
            .iter()
            .zip(self.push_topics_to_remotes(remotes, topics))
        {
            match result {
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

    /// Snapshots `info` into the ODB, merges against the reconciled head and host base,
    /// and computes file diffs without moving any refs or modifying `$HOME`.
    fn plan_topic(
        &self,
        home: &Utf8Path,
        manifest: &Manifest,
        info: &Topic,
        hostname: &str,
        reconciled: Reconciled,
        resolver: Option<&dyn ConflictResolver>,
    ) -> Result<TopicPlan> {
        let name = &info.name;
        if matches!(reconciled, Reconciled::Conflict) {
            Self::report_topic(
                manifest,
                name,
                "conflict",
                "History conflict · needs resolution",
            );
            return Ok(TopicPlan::Conflict);
        }

        debug!("Planning topic '{name}'");
        let topic_base = info.base_dir(home);
        let ours = self::capture::snapshot_topic(self.repo(), manifest, info, &topic_base)?;
        let theirs = self.tree_oid_at(reconciled.head())?;
        let base = self.tree_oid_at(self.head(&Branch::system(hostname, name))?)?;
        let merged: gix::ObjectId = if ours == theirs || theirs == base {
            ours
        } else if ours == base {
            theirs
        } else {
            let mut merge_outcome =
                self::merge::merge_trees(self.repo(), base, ours, theirs, name)?;
            if let Some(resolver) = resolver {
                self::merge::resolve_conflicts(self.repo(), resolver, &mut merge_outcome)?;
            } else if merge_outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
                Self::report_topic(manifest, name, "conflict", "Needs resolution");
                self::merge::show_conflicts(&merge_outcome);
                ui::muted("  run `porchetta sync` to resolve interactively");
                return Ok(TopicPlan::Conflict);
            }
            merge_outcome.tree.write()?.into()
        };
        trace!("Merged tree: {merged}");

        let commit_to_repo = matches!(reconciled, Reconciled::NoHistory) || merged != theirs;
        let merged_tree = self.repo().find_tree(merged)?;
        let store_changes =
            self::diff::collect_file_changes(&self.repo().find_tree(theirs)?, &merged_tree)?;
        let machine_changes =
            self::diff::collect_file_changes(&self.repo().find_tree(ours)?, &merged_tree)?;

        let mut symlink_changes = Vec::new();
        for symlink in &info.symlinks {
            let link_abs = topic_base.join(&symlink.link);
            match std::fs::symlink_metadata(&link_abs) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        let current = std::fs::read_link(&link_abs)
                            .with_context(|| format!("Failed to read symlink '{link_abs}'"))?;
                        let current_str = current.to_string_lossy();
                        if current_str != symlink.target.as_str() {
                            symlink_changes.push(symlink.clone());
                        }
                    } else if let Some(resolver) = resolver {
                        let kind = if meta.is_dir() { "directory" } else { "file" };
                        let prompt = format!(
                            "Path '{}' conflicts with existing {kind}. Replace with symlink to '{}'?",
                            symlink.link, symlink.target
                        );
                        match resolver.resolve_symlink_conflict(&prompt)? {
                            SymlinkConflictResolution::Replace => {
                                symlink_changes.push(symlink.clone());
                            }
                            SymlinkConflictResolution::KeepExisting => {}
                            SymlinkConflictResolution::Abort => {
                                bail!(
                                    "Sync aborted by user while resolving symlink conflict at '{}'",
                                    symlink.link
                                );
                            }
                        }
                    } else {
                        Self::report_topic(
                            manifest,
                            name,
                            "conflict",
                            &format!(
                                "Path '{}' conflicts with existing {}",
                                symlink.link,
                                if meta.is_dir() { "directory" } else { "file" }
                            ),
                        );
                        return Ok(TopicPlan::Conflict);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    symlink_changes.push(symlink.clone());
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("Failed to inspect path '{link_abs}' for topic '{name}'")
                    });
                }
            }
        }

        Ok(TopicPlan::Ready {
            merged,
            commit_to_repo,
            store_changes,
            machine_changes,
            symlink_changes,
        })
    }

    #[cfg(not(windows))]
    fn apply_symlink(
        _home: &Utf8Path,
        _manifest: &Manifest,
        link_path: &Utf8Path,
        target: &Utf8Path,
    ) -> Result<()> {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory '{parent}'"))?;
        }

        if let Ok(meta) = std::fs::symlink_metadata(link_path) {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                std::fs::remove_dir_all(link_path)
            } else {
                std::fs::remove_file(link_path)
            }
            .with_context(|| format!("Failed to remove existing '{link_path}'"))?;
        }

        std::os::unix::fs::symlink(target, link_path)
            .with_context(|| format!("Failed to create symlink '{link_path}' -> '{target}'"))?;

        Ok(())
    }

    #[cfg(windows)]
    fn apply_symlink(
        home: &Utf8Path,
        manifest: &Manifest,
        link_path: &Utf8Path,
        target: &Utf8Path,
    ) -> Result<()> {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory '{parent}'"))?;
        }

        if let Ok(meta) = std::fs::symlink_metadata(link_path) {
            if !link_path.is_dir() {
                std::fs::remove_file(link_path)
            } else if meta.file_type().is_symlink() {
                std::fs::remove_dir(link_path)
            } else {
                std::fs::remove_dir_all(link_path)
            }
            .with_context(|| format!("Failed to remove existing '{link_path}'"))?;
        }

        // Porchetta creates relative symlinks. On Windows, CreateSymbolicLinkW requires
        // knowing whether the relative target resolves to a file or a directory.
        let target_abs = match link_path.parent() {
            Some(parent) => parent.join(target),
            None => target.to_path_buf(),
        };
        let is_dir = target_abs.is_dir()
            || manifest
                .topics
                .iter()
                .any(|t| target_abs == t.base_dir(home));

        if is_dir {
            std::os::windows::fs::symlink_dir(target, link_path)
        } else {
            std::os::windows::fs::symlink_file(target, link_path)
        }
        .with_context(|| {
            format!(
                "Failed to create symlink '{link_path}' -> '{target}' (ensure Windows Developer Mode is enabled)"
            )
        })?;

        Ok(())
    }

    fn execute_topic_plan(
        &self,
        home: &Utf8Path,
        manifest: &Manifest,
        info: &Topic,
        hostname: &str,
        plan: &TopicPlan,
    ) -> Result<()> {
        let TopicPlan::Ready {
            merged,
            commit_to_repo,
            store_changes,
            machine_changes,
            symlink_changes,
        } = plan
        else {
            return Ok(());
        };
        let name = &info.name;
        let topic_base = info.base_dir(home);

        if *commit_to_repo {
            self.commit_topic_tree(
                name,
                hostname,
                *merged,
                format!("Sync topic '{name}' from '{hostname}'"),
            )?;
        }
        if !machine_changes.is_empty()
            && let Err(error) = self::apply::apply(self.repo(), info, &topic_base, machine_changes)
                .with_context(|| format!("Failed to apply changes for topic '{name}'"))
        {
            if *commit_to_repo {
                Self::report_topic(
                    manifest,
                    name,
                    "captured",
                    &format_change_count("Captured", store_changes.len(), "into store"),
                );
                Self::print_file_changes("Store", store_changes);
            }
            ui::error(&format!(
                "Topic '{name}' incomplete · machine changes may be partially applied"
            ));
            return Err(error);
        }

        for change in symlink_changes {
            let link_abs = topic_base.join(&change.link);
            Self::apply_symlink(home, manifest, &link_abs, &change.target).with_context(|| {
                format!(
                    "Failed to create symlink '{}' for topic '{name}'",
                    change.link
                )
            })?;
        }

        self.update_topic_hostname_head(name, hostname)?;
        info!("Synchronized topic '{name}'");
        Ok(())
    }

    fn report_topic_plan(
        manifest: &Manifest,
        info: &Topic,
        dry_run: bool,
        plan: &TopicPlan,
        summary: &mut SyncSummary,
    ) {
        let TopicPlan::Ready {
            commit_to_repo,
            store_changes,
            machine_changes,
            symlink_changes,
            ..
        } = plan
        else {
            summary.record_conflict();
            return;
        };

        let changed_wrt_repo = *commit_to_repo;
        let changed_wrt_system = !machine_changes.is_empty() || !symlink_changes.is_empty();
        let mut details = Vec::new();
        if changed_wrt_repo {
            let verb = if dry_run { "Would capture" } else { "Captured" };
            details.push(format_change_count(verb, store_changes.len(), "into store"));
        }
        if changed_wrt_system {
            let verb = if dry_run { "Would apply" } else { "Applied" };
            let total_system = machine_changes.len() + symlink_changes.len();
            details.push(format_change_count(verb, total_system, "to machine"));
        }
        if details.is_empty() {
            details.push("Unchanged".into());
        }
        let status = status_label(dry_run, changed_wrt_repo, changed_wrt_system);
        Self::report_topic(manifest, &info.name, status, &details.join(" · "));
        Self::print_file_changes("Store", store_changes);
        Self::print_file_changes("Machine", machine_changes);
        for symlink in symlink_changes {
            ui::file_change("Machine", "link", symlink.link.as_str());
        }
        summary.record_synced(
            store_changes.len(),
            machine_changes.len() + symlink_changes.len(),
            changed_wrt_repo || changed_wrt_system,
        );
    }

    fn print_file_changes(destination: &str, changes: &[self::diff::FileChange]) {
        for change in changes {
            ui::file_change(destination, change.action(), change.relative_path.as_str());
        }
    }

    /// Integrates the local and remote heads of one branch into a single head in the ODB.
    ///
    /// Heads already contained in another head's history are skipped; the rest are
    /// merged in sequence, local first. When `resolver` is `None`, stops at the first
    /// conflict and reports it instead of resolving interactively.
    fn reconcile_head(
        &self,
        name: &str,
        local: Option<gix::ObjectId>,
        remote_heads: Vec<gix::ObjectId>,
        hostname: &str,
        resolver: Option<&dyn ConflictResolver>,
    ) -> Result<Reconciled> {
        let mut heads: BTreeSet<_> = local.into_iter().chain(remote_heads).collect();
        if heads.len() > 1 {
            let all: Vec<_> = heads.iter().copied().collect();
            for candidate in &all {
                for other in &all {
                    if candidate != other
                        && heads.contains(other)
                        && self::merge::is_ancestor(self.repo(), *candidate, *other)?
                    {
                        heads.remove(candidate);
                        break;
                    }
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
            let mut outcome = self::merge::merge_commits(self.repo(), integrated, theirs, name)?;
            if let Some(resolver) = resolver {
                self::merge::resolve_conflicts(self.repo(), resolver, &mut outcome)?;
            } else if outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
                ui::error(&format!("Conflicts while reconciling {name}"));
                self::merge::show_conflicts(&outcome);
                ui::muted("  run `porchetta sync` to resolve interactively");
                return Ok(Reconciled::Conflict);
            }
            let tree = outcome.tree.write()?;
            integrated = self.commit_tree(
                tree,
                [integrated, theirs],
                format!("Reconcile {name} on '{hostname}'"),
            )?;
        }
        Ok(Reconciled::Head(integrated))
    }
}
