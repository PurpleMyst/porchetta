pub mod apply;
pub mod capture;
pub mod diff;
pub mod merge;
pub mod path_util;
pub mod resolver;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use gix::merge::tree::TreatAsUnresolved;
use log::{debug, info, trace};

use self::resolver::ConflictResolver;
use crate::manifest::Manifest;
use crate::store::PorchettaStore;
use crate::ui;

pub struct PorchettaEngine {
    store: PorchettaStore,
    home: Utf8PathBuf,
    resolver: Box<dyn ConflictResolver>,
}

struct SyncTopicResult {
    status: SyncTopicStatus,
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

    /// Edits the manifest using the provided editor function.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or writing the manifest fails.
    pub fn edit_manifest(&mut self, editor: impl FnOnce(&[u8]) -> Result<Vec<u8>>) -> Result<()> {
        debug!("Starting manifest edit");
        let manifest_content = self.store.read_manifest()?;
        let new_manifest_content = editor(&manifest_content)?;
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

        let has_origin = !offline && self.store.has_origin()?;
        if has_origin {
            self.pull_manifest()?;
        }

        let manifest = Manifest::load_with_home(&self.store.read_manifest()?, Some(&self.home))?;
        info!("Loaded manifest with {} topics", manifest.topics.len());

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");

        if has_origin {
            self.pull_topics(&manifest)?;
        }

        let mut summary = SyncSummary::default();
        for info in &manifest.topics {
            let result = self.sync_topic(&manifest, info, &hostname, dry_run)?;
            summary.record(result.status);
        }
        summary.print();

        if !dry_run && has_origin {
            self.store.push_all(&hostname)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    fn sync_topic(
        &mut self,
        manifest: &crate::manifest::Manifest,
        info: &crate::manifest::Topic,
        hostname: &str,
        dry_run: bool,
    ) -> Result<SyncTopicResult> {
        let name = &info.name;
        debug!("Syncing topic '{name}'");
        let topic_base = if let Some(root) = &info.root {
            self.home.join(root)
        } else {
            self.home.clone()
        };

        let our_tree_oid = self::capture::snapshot_topic(
            &self.store,
            &topic_base,
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
        let their_tree_oid = self.store.get_topic_tree_oid(name)?;
        let base_tree_oid = self.store.get_topic_hostname_tree_oid(name, hostname)?;
        let mut merge_outcome =
            self.store
                .merge_trees(base_tree_oid, our_tree_oid, their_tree_oid, name)?;

        if dry_run && merge_outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
            ui::bullet(&format!("{name} — {}", ui::color_status("conflict")));
            self::merge::show_conflicts(&merge_outcome);
            ui::muted("  run `porchetta sync` to resolve interactively");
            return Ok(SyncTopicResult {
                status: SyncTopicStatus::Conflict,
            });
        }

        if !dry_run {
            self::merge::resolve_conflicts(
                &self.store,
                self.resolver.as_ref(),
                &mut merge_outcome,
            )?;
        }

        let merged_tree_oid = merge_outcome.tree.write()?;
        trace!("Merged tree: {merged_tree_oid}");

        let changed_wrt_repo = merged_tree_oid != their_tree_oid;
        let changed_wrt_system = merged_tree_oid != our_tree_oid;
        let status = Self::topic_sync_status(dry_run, changed_wrt_repo, changed_wrt_system);
        ui::bullet(&format!("{name} — {}", ui::color_status(status.label())));
        if changed_wrt_repo {
            if dry_run {
                ui::bullet(&format!(
                    "  repo: would update topic tree to {merged_tree_oid}"
                ));
            } else {
                self.store.commit_topic_tree(
                    name,
                    hostname,
                    merged_tree_oid,
                    format!("Sync topic '{name}'"),
                )?;
            }
        } else {
            debug!("Topic '{name}' has no changes from repo");
        }

        if changed_wrt_system {
            let our_tree = self.store.find_tree(our_tree_oid)?;
            let merged_tree = self.store.find_tree(merged_tree_oid)?;
            let operations = self::diff::collect_apply_operations(&our_tree, &merged_tree)
                .with_context(|| {
                    format!("Failed to compute apply operations for topic '{name}'")
                })?;

            if dry_run {
                ui::info(&format!(
                    "  system: would apply {} change(s) under {topic_base}",
                    operations.len()
                ));
                for op in &operations {
                    ui::muted(&format!("    {op}"));
                }
            } else {
                self::apply::apply(
                    &topic_base,
                    name,
                    operations,
                    |rel, content| info.to_system(rel, content),
                    |oid| self.store.find_blob(oid).map(|b| b.data.clone()),
                )
                .with_context(|| format!("Failed to apply changes for topic '{name}'"))?;
            }
        }

        if !dry_run {
            self.store.update_topic_hostname_head(name, hostname)?;
        }

        info!("Synchronized topic '{name}'");
        Ok(SyncTopicResult { status })
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

    fn pull_manifest(&self) -> Result<()> {
        self.store.git_fetch()?;
        if let Some(remote_oid) = self.store.get_remote_manifest_head("origin")? {
            self.store.fast_forward_manifest(remote_oid)?;
        }
        Ok(())
    }

    fn pull_topics(&self, manifest: &Manifest) -> Result<()> {
        for topic in &manifest.topics {
            let name = &topic.name;
            if let Some(remote_oid) = self.store.get_remote_topic_head("origin", name)? {
                self.store.fast_forward_topic(name, remote_oid)?;
            }
        }
        Ok(())
    }
}
