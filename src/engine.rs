pub mod apply;
pub mod capture;
pub mod diff;
pub mod merge;
pub mod path_util;
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

struct SyncTopicResult {
    status: SyncTopicStatus,
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
    fn head(&self) -> Option<gix::ObjectId> {
        match self {
            Self::Head(head) => Some(*head),
            Self::NoHistory | Self::Conflict => None,
        }
    }
}

#[derive(Clone, Copy)]
enum SyncTopicStatus {
    Unchanged,
    WouldCapture,
    WouldApply,
    WouldCaptureAndApply,
    Conflict,
    Captured,
    Applied,
    CapturedAndApplied,
}

impl SyncTopicStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::WouldCapture => "would capture",
            Self::WouldApply => "would apply",
            Self::WouldCaptureAndApply => "would capture and apply",
            Self::Conflict => "conflict",
            Self::Captured => "captured",
            Self::Applied => "applied",
            Self::CapturedAndApplied => "captured and applied",
        }
    }
}

#[derive(Default)]
struct SyncSummary {
    total: usize,
    unchanged: usize,
    changed: usize,
    conflicts: usize,
}

impl SyncSummary {
    fn record(&mut self, status: SyncTopicStatus) {
        self.total += 1;
        match status {
            SyncTopicStatus::Unchanged => self.unchanged += 1,
            SyncTopicStatus::Conflict => self.conflicts += 1,
            SyncTopicStatus::WouldCapture
            | SyncTopicStatus::WouldApply
            | SyncTopicStatus::WouldCaptureAndApply
            | SyncTopicStatus::Captured
            | SyncTopicStatus::Applied
            | SyncTopicStatus::CapturedAndApplied => self.changed += 1,
        }
    }

    fn print(self) {
        ui::info(&format!(
            "Summary: {} topic(s) checked, {} clean, {} changed, {} conflict(s)",
            self.total, self.unchanged, self.changed, self.conflicts
        ));
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
        // Validate the edited manifest before writing
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
        debug!("Starting sync operation");

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

        let local_manifest = self.store.head(&Branch::Manifest)?;
        let remote_manifest_heads = self.remote_heads(&remotes, &Branch::Manifest)?;
        let manifest_head = match self.reconcile_head(
            "manifest",
            local_manifest,
            remote_manifest_heads,
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

        let mut reconciled_topics = BTreeMap::new();
        for topic in &manifest.topics {
            let branch = Branch::topic(&topic.name);
            let local_topic_head = self.store.head(&branch)?;
            let remote_heads = self.remote_heads(&remotes, &branch)?;
            let state = self.reconcile_head(
                &format!("topic/{}", topic.name),
                local_topic_head,
                remote_heads,
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

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");

        let enabled_topics: Vec<_> = manifest
            .topics
            .iter()
            .filter(|topic| topic.enabled)
            .collect();

        let mut summary = SyncSummary::default();
        for info in &enabled_topics {
            let state = reconciled_topics
                .get(&info.name)
                .copied()
                .with_context(|| format!("Topic '{}' was not reconciled", info.name))?;
            if matches!(state, Reconciled::Conflict) {
                summary.record(SyncTopicStatus::Conflict);
                continue;
            }
            let result = self.sync_topic(&manifest, info, &hostname, dry_run, state)?;
            summary.record(result.status);
        }
        summary.print();

        if !dry_run {
            let topic_names: Vec<_> = manifest
                .topics
                .iter()
                .map(|topic| topic.name.as_str())
                .collect();
            self.publish(&remotes, &topic_names)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    fn publish(&self, remotes: &[Remote], topics: &[&str]) -> Result<()> {
        let mut failures = Vec::new();
        for remote in remotes {
            match self.store.push_topics(remote, topics) {
                Ok(()) => ui::info(&format!(
                    "Published synchronized refs to '{}'",
                    remote.name()
                )),
                Err(error) => {
                    ui::error(&format!(
                        "Failed to publish to '{}': {error}",
                        remote.name()
                    ));
                    failures.push(format!("{}: {error}", remote.name()));
                }
            }
        }
        if !failures.is_empty() {
            bail!(
                "Local synchronization succeeded, but publication failed for: {}",
                failures.join("; ")
            );
        }
        Ok(())
    }

    fn sync_topic(
        &mut self,
        manifest: &crate::manifest::Manifest,
        info: &crate::manifest::Topic,
        hostname: &str,
        dry_run: bool,
        reconciled: Reconciled,
    ) -> Result<SyncTopicResult> {
        let name = &info.name;
        debug!("Syncing topic '{name}'");
        let topic_base = if let Some(root) = &info.root {
            self.home.join(root)
        } else {
            self.home.clone()
        };

        let Some(trees) =
            self.merge_topic_trees(manifest, info, hostname, &topic_base, reconciled, dry_run)?
        else {
            return Ok(SyncTopicResult {
                status: SyncTopicStatus::Conflict,
            });
        };

        let changed_wrt_repo =
            matches!(reconciled, Reconciled::NoHistory) || trees.merged != trees.theirs;
        let changed_wrt_system = trees.merged != trees.ours;
        let status = Self::topic_sync_status(dry_run, changed_wrt_repo, changed_wrt_system);
        ui::bullet(&format!("{name} — {}", ui::color_status(status.label())));

        self.commit_repo_change(name, hostname, changed_wrt_repo, trees.merged, dry_run)?;
        self.apply_system_change(
            info,
            &topic_base,
            changed_wrt_system,
            trees.ours,
            trees.merged,
            dry_run,
        )?;

        if !dry_run {
            self.store.update_topic_hostname_head(name, hostname)?;
        }

        info!("Synchronized topic '{name}'");
        Ok(SyncTopicResult { status })
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
        let ours = self::capture::snapshot_topic(
            self.store.repo(),
            topic_base,
            &info.paths,
            |rel| {
                Ok(manifest
                    .should_include(rel)
                    .with_context(|| format!("manifest-level should_include failed for '{rel}'"))?
                    && info.should_include(rel).with_context(|| {
                        format!("topic-level should_include failed for '{rel}'")
                    })?)
            },
            |rel, content| info.to_repo(rel, content),
        )?;
        let theirs = self.tree_oid_at(reconciled.head())?;
        let system_head = self.store.head(&Branch::system(hostname, name))?;
        let base = self.tree_oid_at(system_head)?;
        let mut merge_outcome =
            self::merge::merge_trees(self.store.repo(), base, ours, theirs, name)?;

        if dry_run && merge_outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
            ui::bullet(&format!("{name} — {}", ui::color_status("conflict")));
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

    /// Commits the merged tree to the topic branch when it differs from the repo state.
    fn commit_repo_change(
        &self,
        name: &str,
        hostname: &str,
        changed: bool,
        merged: gix::ObjectId,
        dry_run: bool,
    ) -> Result<()> {
        if !changed {
            debug!("Topic '{name}' has no changes from repo");
            return Ok(());
        }
        if dry_run {
            ui::bullet(&format!("  repo: would update topic tree to {merged}"));
            return Ok(());
        }
        self.store
            .commit_topic_tree(name, hostname, merged, format!("Sync topic '{name}'"))?;
        Ok(())
    }

    /// Applies the merged tree to the filesystem when it differs from the system state.
    fn apply_system_change(
        &self,
        info: &crate::manifest::Topic,
        topic_base: &camino::Utf8Path,
        changed: bool,
        ours: gix::ObjectId,
        merged: gix::ObjectId,
        dry_run: bool,
    ) -> Result<()> {
        if !changed {
            return Ok(());
        }
        let name = &info.name;
        let our_tree = self.store.repo().find_tree(ours)?;
        let merged_tree = self.store.repo().find_tree(merged)?;
        let operations = self::diff::collect_apply_operations(&our_tree, &merged_tree)
            .with_context(|| format!("Failed to compute apply operations for topic '{name}'"))?;

        if dry_run {
            ui::info(&format!(
                "  system: would apply {} change(s) under {topic_base}",
                operations.len()
            ));
            for op in &operations {
                ui::muted(&format!("    {op}"));
            }
            return Ok(());
        }

        self::apply::apply(
            topic_base,
            name,
            operations,
            |rel, content| info.to_system(rel, content),
            |oid| {
                self.store
                    .repo()
                    .find_blob(oid)
                    .map(|b| b.data.clone())
                    .map_err(Into::into)
            },
        )
        .with_context(|| format!("Failed to apply changes for topic '{name}'"))
    }

    fn tree_oid_at(&self, head: Option<gix::ObjectId>) -> Result<gix::ObjectId> {
        match head {
            Some(head) => Ok(self
                .store
                .repo()
                .find_object(head)?
                .peel_to_tree()?
                .id()
                .into()),
            None => Ok(self.store.repo().empty_tree().id().into()),
        }
    }

    fn topic_sync_status(
        dry_run: bool,
        changed_wrt_repo: bool,
        changed_wrt_system: bool,
    ) -> SyncTopicStatus {
        match (dry_run, changed_wrt_repo, changed_wrt_system) {
            (true, true, true) => SyncTopicStatus::WouldCaptureAndApply,
            (true, true, false) => SyncTopicStatus::WouldCapture,
            (true, false, true) => SyncTopicStatus::WouldApply,
            (_, false, false) => SyncTopicStatus::Unchanged,
            (false, true, true) => SyncTopicStatus::CapturedAndApplied,
            (false, true, false) => SyncTopicStatus::Captured,
            (false, false, true) => SyncTopicStatus::Applied,
        }
    }

    /// Returns the fetched head of `branch` on each remote that has one.
    fn remote_heads(&self, remotes: &[Remote], branch: &Branch) -> Result<Vec<gix::ObjectId>> {
        remotes
            .iter()
            .filter_map(|remote| self.store.remote_head(remote, branch).transpose())
            .collect()
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
        dry_run: bool,
    ) -> Result<Reconciled> {
        // A head that is an ancestor of another head is already contained in it.
        let mut heads: BTreeSet<_> = local.into_iter().chain(remote_heads).collect();
        let all: Vec<_> = heads.iter().copied().collect();
        let mut contained = BTreeSet::new();
        for candidate in &all {
            for other in &all {
                if candidate != other
                    && self::merge::is_ancestor(self.store.repo(), *candidate, *other)?
                {
                    contained.insert(*candidate);
                    break;
                }
            }
        }
        for head in contained {
            heads.remove(&head);
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
            integrated =
                self.store
                    .commit_tree(tree, [integrated, theirs], format!("Reconcile {name}"))?;
        }
        Ok(Reconciled::Head(integrated))
    }
}
