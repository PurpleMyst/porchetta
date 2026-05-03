pub mod apply;
pub mod capture;
pub mod diff;
pub mod merge;
pub mod path_util;
pub mod resolver;
pub mod scan;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
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
struct DryRunSummary {
    total: usize,
    unchanged: usize,
    changed: usize,
    conflicts: usize,
}

impl DryRunSummary {
    fn record(&mut self, status: SyncTopicStatus) {
        self.total += 1;
        match status {
            SyncTopicStatus::Unchanged => self.unchanged += 1,
            SyncTopicStatus::Conflict => self.conflicts += 1,
            SyncTopicStatus::WouldCapture
            | SyncTopicStatus::WouldApply
            | SyncTopicStatus::WouldCaptureAndApply => self.changed += 1,
            SyncTopicStatus::Captured
            | SyncTopicStatus::Applied
            | SyncTopicStatus::CapturedAndApplied => {}
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
            self.pull_manifest_ref()?;
        }

        let manifest = Manifest::load(&self.store.read_manifest()?)?;
        info!("Loaded manifest with {} topics", manifest.topics.len());

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");

        if has_origin {
            self.pull_topic_refs(&manifest)?;
        }

        let mut dry_run_summary = DryRunSummary::default();
        for info in &manifest.topics {
            let result = self.sync_topic(&manifest, info, &hostname, dry_run)?;
            if dry_run {
                dry_run_summary.record(result.status);
            }
        }

        if dry_run {
            dry_run_summary.print();
        }

        if !dry_run && has_origin {
            self.store.git_push_all(&hostname)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn sync_topic(
        &mut self,
        manifest: &crate::manifest::Manifest,
        info: &crate::manifest::Topic,
        hostname: &str,
        dry_run: bool,
    ) -> Result<SyncTopicResult> {
        let name = &info.name;
        debug!("Syncing topic '{name}'");
        let topic_base = if let Some(root) = &info.root
            && !root.as_str().is_empty()
        {
            self.home.join(root)
        } else {
            self.home.clone()
        };

        let topic_files = self::scan::scan_topic_files(&topic_base, &info.paths, |rel| {
            let manifest_ok = manifest
                .should_include(rel)
                .with_context(|| format!("manifest-level should_include failed for '{rel}'"))?;
            let topic_ok = info.should_include(rel)?;
            Ok(manifest_ok && topic_ok)
        })?;

        let file_count = topic_files.len();
        debug!("Topic '{name}' has {file_count} files to sync");

        let topic_files_vec: Vec<_> = topic_files.iter().cloned().collect();
        let our_tree_oid = self::capture::snapshot_topic(
            &self.store,
            &topic_base,
            &topic_files_vec,
            |rel, content| info.to_repo(rel, content),
        )?;
        trace!("Built our tree: {our_tree_oid}");

        let their_tree_oid = self.store.get_topic_tree_oid(name)?;
        trace!("Found their tree: {their_tree_oid}");

        let base_tree_oid = self.store.get_topic_hostname_tree_oid(name, hostname)?;
        trace!("Found base tree: {base_tree_oid}");

        let mut merge_outcome = self.store.merge_trees(
            base_tree_oid,
            our_tree_oid,
            their_tree_oid,
            Labels {
                ancestor: Some(
                    gix::bstr::BString::from(format!("{name} (last applied)")).as_bstr(),
                ),
                current: Some(gix::bstr::BString::from(format!("{name} (on system)")).as_bstr()),
                other: Some(gix::bstr::BString::from(format!("{name} (in repo)")).as_bstr()),
            },
        )?;

        if dry_run && merge_outcome.has_unresolved_conflicts(TreatAsUnresolved::default()) {
            ui::bullet(&format!("{name} — {}", ui::status("conflict")));
            for conflict in &merge_outcome.conflicts {
                if !conflict.is_unresolved(TreatAsUnresolved::default()) {
                    continue;
                }
                let (ours_change, theirs_change) = conflict.changes_in_resolution();
                let location_description =
                    self::merge::conflict_location_description(ours_change, theirs_change);
                ui::info(&format!(
                    "  {location_description} changed both locally and in repo"
                ));
            }
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
        if dry_run {
            ui::bullet(&format!("{name} — {}", ui::status(status.label())));
        }
        if changed_wrt_repo {
            if dry_run {
                ui::info("  repo: would commit updated topic state");
            } else {
                let commit_oid = self.store.commit_topic_tree(
                    name,
                    hostname,
                    merged_tree_oid,
                    format!("Sync topic '{name}'"),
                )?;
                debug!("Created commit: {commit_oid}");
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
                    match op {
                        self::diff::ApplyOperation::Upsert { relative_path, .. } => {
                            ui::muted(&format!("    upsert {relative_path}"));
                        }
                        self::diff::ApplyOperation::Delete { relative_path } => {
                            ui::muted(&format!("    delete {relative_path}"));
                        }
                    }
                }
            } else {
                self::apply::preflight(&topic_base, name, &operations)
                    .with_context(|| format!("Pre-flight checks failed for topic '{name}'"))?;

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
            ui::bullet(&format!("{name} — {}", ui::status(status.label())));
        }

        if !dry_run {
            let topic_head = self
                .store
                .get_topic_head(name)?
                .context("Missing topic head for existing topic")?;
            self.store
                .update_topic_hostname_head(name, hostname, topic_head)?;
        }

        info!("Synchronized topic '{name}'");
        Ok(SyncTopicResult { status })
    }

    fn topic_sync_status(dry_run: bool, captured: bool, applied: bool) -> SyncTopicStatus {
        match (dry_run, captured, applied) {
            (true, true, true) => SyncTopicStatus::WouldCaptureAndApply,
            (true, true, false) => SyncTopicStatus::WouldCapture,
            (true, false, true) => SyncTopicStatus::WouldApply,
            (_, false, false) => SyncTopicStatus::Unchanged,
            (false, true, true) => SyncTopicStatus::CapturedAndApplied,
            (false, true, false) => SyncTopicStatus::Captured,
            (false, false, true) => SyncTopicStatus::Applied,
        }
    }

    fn pull_manifest_ref(&self) -> Result<()> {
        self.store.git_fetch()?;
        if let Some(remote_oid) = self.store.get_remote_manifest_head("origin")? {
            self.store.fast_forward_manifest(remote_oid)?;
        }
        Ok(())
    }

    fn pull_topic_refs(&self, manifest: &Manifest) -> Result<()> {
        for topic in &manifest.topics {
            let name = &topic.name;
            if let Some(remote_oid) = self.store.get_remote_topic_head("origin", name)? {
                self.store.fast_forward_topic(name, remote_oid)?;
            }
        }
        Ok(())
    }
}
