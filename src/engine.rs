use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::TreatAsUnresolved;
use log::{debug, info, trace};

use crate::apply;
use crate::capture;
use crate::diff;
use crate::manifest::Manifest;
use crate::merge;
use crate::resolver::ConflictResolver;
use crate::scan;
use crate::store::PorchettaStore;
use crate::ui;

pub struct PorchettaEngine {
    store: PorchettaStore,
    home: Option<Utf8PathBuf>,
    resolver: Box<dyn ConflictResolver>,
}

impl PorchettaEngine {
    pub fn new(store: PorchettaStore, resolver: impl ConflictResolver + 'static) -> Self {
        Self {
            store,
            home: None,
            resolver: Box::new(resolver),
        }
    }

    /// Creates an engine bound to a specific home directory (useful for tests).
    pub fn with_home(
        store: PorchettaStore,
        home: Utf8PathBuf,
        resolver: impl ConflictResolver + 'static,
    ) -> Self {
        Self {
            store,
            home: Some(home),
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
    pub fn sync(&mut self, verbose: bool, dry_run: bool, offline: bool) -> Result<()> {
        debug!("Starting sync operation");
        let home = self.resolve_home()?;
        let mut manifest = Manifest::load(&self.store.read_manifest()?)?;
        info!("Loaded manifest with {} topics", manifest.topics.len());

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");

        let has_origin = !offline && self.store.has_origin()?;

        if has_origin {
            self.store.git_fetch()?;
            if let Some(remote_oid) = self.store.get_remote_manifest_head("origin")? {
                self.maybe_fast_forward_manifest(remote_oid, &mut manifest)?;
            }
        }

        let mut refs_to_push: Vec<String> = Vec::new();
        for (name, info) in &manifest.topics {
            let topic_refs = self.sync_topic(
                name, info, &home, &hostname, &manifest.lua, dry_run, verbose, has_origin,
            )?;
            refs_to_push.extend(topic_refs);
        }

        if !dry_run && has_origin && !refs_to_push.is_empty() {
            if verbose {
                ui::info(&format!("Pushing {} ref(s) to origin", refs_to_push.len()));
            }
            self.store.git_push(&refs_to_push)?;
        }

        debug!("Sync operation completed");
        Ok(())
    }

    fn resolve_home(&self) -> Result<Utf8PathBuf> {
        match &self.home {
            Some(h) => Ok(h.clone()),
            None => Utf8PathBuf::try_from(
                dirs::home_dir().context("Could not determine home directory")?,
            )
            .map_err(|e| anyhow::anyhow!("home directory is not valid UTF-8: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn sync_topic(
        &mut self,
        name: &str,
        info: &crate::manifest::Topic,
        home: &Utf8Path,
        hostname: &str,
        lua: &mlua::Lua,
        dry_run: bool,
        verbose: bool,
        has_origin: bool,
    ) -> Result<Vec<String>> {
        debug!("Syncing topic '{name}'");
        let mut refs_to_push = Vec::new();

        let topic_base = match &info.root {
            Some(root) if !root.as_str().is_empty() => home.join(root),
            _ => home.to_path_buf(),
        };

        for p in &info.paths {
            ensure!(
                !p.components().any(|c| c == Utf8Component::ParentDir),
                "Topic '{name}' path '{p}' contains '..' which is not allowed"
            );
        }

        if has_origin
            && let Some(remote_oid) = self.store.get_remote_topic_head("origin", name)?
        {
            self.maybe_fast_forward_topic(name, remote_oid)?;
        }

        let topic_files = scan::scan_topic_files(&topic_base, &info.paths, |rel| {
            match &info.should_include {
                Some(key) => crate::hooks::run_should_include(lua, name, key, rel),
                None => Ok(true),
            }
        })?;

        let file_count = topic_files.len();
        debug!("Topic '{name}' has {file_count} files to sync");
        if verbose {
            ui::info(&format!("Syncing topic '{name}'"));
            ui::bullet(&format!("{file_count} files scanned"));
        }

        let topic_files_vec: Vec<_> = topic_files.iter().cloned().collect();
        let snapshot = capture::capture_files(&topic_base, &topic_files_vec, |rel, content| {
            match &info.to_repo {
                Some(key) => crate::hooks::run_hook(lua, name, "to_repo", key, rel, content),
                None => Ok(content.to_vec()),
            }
        })?;
        let our_tree_oid = capture::write_snapshot(&self.store, &snapshot)?;
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
                current: Some(
                    gix::bstr::BString::from(format!("{name} (on system)")).as_bstr(),
                ),
                other: Some(gix::bstr::BString::from(format!("{name} (in repo)")).as_bstr()),
            },
            self.store.tree_merge_options()?,
        )?;

        let has_unresolved = merge_outcome.conflicts.iter().any(|c| {
            c.is_unresolved(TreatAsUnresolved::default())
        });

        if dry_run && has_unresolved {
            ui::bullet(&format!("{name} — would require conflict resolution"));
            for conflict in &merge_outcome.conflicts {
                if !conflict.is_unresolved(TreatAsUnresolved::default()) {
                    continue;
                }
                let (ours_change, theirs_change) = conflict.changes_in_resolution();
                let location_description =
                    merge::conflict_location_description(ours_change, theirs_change);
                ui::info(&format!("  unresolved conflict at {location_description}"));
            }
            return Ok(refs_to_push);
        }

        if !dry_run {
            merge::resolve_conflicts(&self.store, self.resolver.as_ref(), &mut merge_outcome)?;
        }

        let merged_tree_oid = merge_outcome.tree.write()?;
        trace!("Merged tree: {merged_tree_oid}");

        let pushed = merged_tree_oid != their_tree_oid;
        let pulled = merged_tree_oid != our_tree_oid;
        let old_topic_head = self.store.get_topic_head(name)?;

        if pushed {
            if dry_run {
                ui::bullet(&format!("would push topic '{name}' to repo"));
            } else {
                let commit_oid = self.store.commit_topic_tree(
                    name,
                    hostname,
                    merged_tree_oid,
                    format!("Sync topic '{name}'"),
                )?;
                if verbose {
                    ui::bullet(&format!("pushed to repo ({commit_oid})"));
                }
                debug!("Created commit: {commit_oid}");
                self.store.update_topic_head(name, commit_oid)?;
                if old_topic_head != Some(commit_oid) {
                    refs_to_push.push(format!("refs/heads/topic/{name}"));
                }
            }
        } else {
            debug!("Topic '{name}' has no changes from repo");
        }

        if pulled {
            let our_tree = self.store.find_tree(our_tree_oid)?;
            let merged_tree = self.store.find_tree(merged_tree_oid)?;
            let operations = diff::collect_apply_operations(&our_tree, &merged_tree)
                .with_context(|| format!("Failed to compute apply operations for topic '{name}'"))?;

            if dry_run {
                ui::bullet(&format!(
                    "would apply {} change(s) to system",
                    operations.len()
                ));
                for op in &operations {
                    match op {
                        diff::ApplyOperation::Upsert { relative_path, .. } => {
                            ui::info(&format!("  would upsert {relative_path}"));
                        }
                        diff::ApplyOperation::Delete { relative_path } => {
                            ui::info(&format!("  would delete {relative_path}"));
                        }
                    }
                }
            } else {
                if verbose {
                    ui::bullet(&format!(
                        "applied {} change(s) to system",
                        operations.len()
                    ));
                }

                apply::preflight(&topic_base, name, &operations)
                    .with_context(|| format!("Pre-flight checks failed for topic '{name}'"))?;

                apply::apply(
                    &topic_base,
                    name,
                    operations,
                    |rel, content| match &info.to_system {
                        Some(key) => crate::hooks::run_hook(
                            lua, name, "to_system", key, rel, content,
                        ),
                        None => Ok(content.to_vec()),
                    },
                    |oid| self.store.find_blob(oid).map(|b| b.data.clone()),
                )
                .with_context(|| format!("Failed to apply changes for topic '{name}'"))?;
            }
        }

        let status = Self::topic_sync_status(dry_run, pushed, pulled);
        ui::bullet(&format!("{name} — {status}"));

        if !dry_run {
            let topic_head = self.store
                .get_topic_head(name)?
                .context("Missing topic head for existing topic")?;
            let old_system_head = self.store.get_topic_hostname_head(name, hostname)?;
            self.store.update_topic_hostname_head(name, hostname, topic_head)?;
            if old_system_head != Some(topic_head) {
                refs_to_push.push(format!("refs/heads/system/{hostname}/{name}"));
            }
        }

        info!("Synchronized topic '{name}'");
        Ok(refs_to_push)
    }

    fn topic_sync_status(dry_run: bool, pushed: bool, pulled: bool) -> &'static str {
        match (dry_run, pushed, pulled) {
            (true, true, true) => "would sync",
            (true, true, false) => "would push",
            (true, false, true) => "would apply",
            (_, false, false) => "unchanged",
            (false, true, true) => "synced",
            (false, true, false) => "pushed",
            (false, false, true) => "applied",
        }
    }

    fn maybe_fast_forward_manifest(
        &mut self,
        remote_oid: gix::ObjectId,
        manifest: &mut Manifest,
    ) -> Result<()> {
        use crate::store::FastForwardOutcome;
        match self.store.fast_forward_branch("manifest", remote_oid)? {
            FastForwardOutcome::Diverged => {
                bail!("manifest branch has diverged between local and remote");
            }
            FastForwardOutcome::FastForwarded => {
                *manifest = Manifest::load(&self.store.read_manifest()?)?;
            }
            FastForwardOutcome::UpToDate => {}
        }
        Ok(())
    }

    fn maybe_fast_forward_topic(&self, name: &str, remote_oid: gix::ObjectId) -> Result<()> {
        use crate::store::FastForwardOutcome;
        match self.store.fast_forward_branch(&format!("topic/{name}"), remote_oid)? {
            FastForwardOutcome::Diverged => {
                bail!("topic '{name}' has diverged between local and remote");
            }
            _ => Ok(()),
        }
    }
}
