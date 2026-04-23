use std::collections::{HashSet, VecDeque};
use std::ops::ControlFlow;

use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::merge::tree::TreatAsUnresolved;
use log::{debug, info, trace, warn};

use crate::manifest::Manifest;
use crate::store::PorchettaStore;
use crate::ui;

enum ApplyOperation {
    Upsert {
        relative_path: Utf8PathBuf,
        blob_oid: ObjectId,
    },
    Delete {
        relative_path: Utf8PathBuf,
    },
}

/// Convert a platform path to a forward-slash string for git tree storage.
fn to_tree_path(path: &Utf8Path) -> String {
    path.as_str().replace('\\', "/")
}

/// Convert a forward-slash path from a git tree to a platform `Utf8PathBuf`.
fn from_tree_path(path: &str) -> Utf8PathBuf {
    Utf8PathBuf::from(path)
}

pub struct PorchettaEngine {
    store: PorchettaStore,
    home: Option<Utf8PathBuf>,
}

impl PorchettaEngine {
    pub fn new(store: PorchettaStore) -> Self {
        Self { store, home: None }
    }

    /// Creates an engine bound to a specific home directory (useful for tests).
    pub fn with_home(store: PorchettaStore, home: Utf8PathBuf) -> Self {
        Self {
            store,
            home: Some(home),
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
    #[allow(clippy::too_many_lines)]
    pub fn sync(&mut self, verbose: bool, dry_run: bool, offline: bool) -> Result<()> {
        debug!("Starting sync operation");
        let home = match &self.home {
            Some(h) => h.clone(),
            None => Utf8PathBuf::try_from(
                dirs::home_dir().context("Could not determine home directory")?
            )
            .map_err(|e| anyhow::anyhow!("home directory is not valid UTF-8: {e}"))?,
        };
        let mut manifest = Manifest::load(&self.store.read_manifest()?)?;
        info!("Loaded manifest with {} topics", manifest.topics.len());

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {hostname}");

        let has_origin = if offline {
            false
        } else {
            self.store.has_origin()?
        };

        if has_origin {
            self.store.git_fetch()?;

            if let Some(remote_oid) = self.store.get_remote_manifest_head("origin")? {
                if let Some(local_oid) = self.store.get_manifest_head()? {
                    if remote_oid != local_oid {
                        if self.store.git_ancestor_check(remote_oid, local_oid)? {
                            // remote is ancestor of local, local is ahead — nothing to do
                        } else if self.store.git_ancestor_check(local_oid, remote_oid)? {
                            self.store.update_manifest_head(remote_oid)?;
                            manifest = Manifest::load(&self.store.read_manifest()?)?;
                        } else {
                            bail!("manifest branch has diverged between local and remote");
                        }
                    }
                } else {
                    self.store.update_manifest_head(remote_oid)?;
                    manifest = Manifest::load(&self.store.read_manifest()?)?;
                }
            }
        }

        let topics: Vec<_> = manifest.topics.iter().collect();
        let mut refs_to_push: Vec<String> = Vec::new();
        for (name, info) in topics {
            debug!("Syncing topic '{name}'");

            let topic_base = match &info.root {
                Some(root) if !root.as_str().is_empty() => home.join(root),
                _ => home.clone(),
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
                if let Some(local_oid) = self.store.get_topic_head(name)? {
                    if remote_oid != local_oid
                        && self.store.git_ancestor_check(local_oid, remote_oid)?
                    {
                        self.store.update_topic_head(name, remote_oid)?;
                    } else if remote_oid != local_oid
                        && !self.store.git_ancestor_check(remote_oid, local_oid)?
                    {
                        bail!("topic '{name}' has diverged between local and remote");
                    }
                } else {
                    self.store.update_topic_head(name, remote_oid)?;
                }
            }

            // Capture and create system ("our") tree.
            let mut topic_files = HashSet::new();

            for p in &info.paths {
                let abs_path = topic_base.join(p);
                if abs_path.is_file() {
                    let relative_path = to_tree_path(abs_path.strip_prefix(&topic_base)?);
                    if let Some(ref key) = info.should_include
                        && !crate::manifest::run_should_include(&manifest.lua, name, key, &relative_path)?
                    {
                        continue;
                    }
                    trace!("Found file: {abs_path}");
                    topic_files.insert(abs_path);
                } else if abs_path.is_dir() {
                    trace!("Scanning directory: {abs_path}");
                    let mut queue = VecDeque::new();
                    queue.push_back(abs_path);
                    while let Some(p2) = queue.pop_front() {
                        // Always skip .git directories; causes weird problems.
                        if p2.file_name() == Some(".git") {
                            debug!("Skipping .git directory at '{p2}'");
                            continue;
                        }

                        if p2.is_file() {
                            let relative_path = to_tree_path(p2.strip_prefix(&topic_base)?);
                            if let Some(ref key) = info.should_include
                                && !crate::manifest::run_should_include(&manifest.lua, name, key, &relative_path)?
                            {
                                debug!("Excluding file '{p2}' based on should_include hook");
                                continue;
                            }
                            trace!("Found file: {p2}");
                            topic_files.insert(p2);
                        } else if p2.is_dir() {
                            let relative_path = to_tree_path(p2.strip_prefix(&topic_base)?);
                            if let Some(ref key) = info.should_include
                                && !crate::manifest::run_should_include(&manifest.lua, name, key, &relative_path)?
                            {
                                debug!("Excluding directory '{p2}' based on should_include hook");
                                continue;
                            }
                            trace!("Queueing directory: {p2}");
                            for entry in std::fs::read_dir(&p2)? {
                                let path = Utf8PathBuf::try_from(entry?.path())
                                    .context("non-UTF-8 path encountered during scan")?;
                                queue.push_back(path);
                            }
                        } else {
                            bail!(
                                "Path '{p2}' does not exist or is not a file/directory"
                            );
                        }
                    }
                } else {
                    bail!(
                        "Path '{abs_path}' does not exist or is not a file/directory"
                    );
                }
            }

            let file_count = topic_files.len();
            debug!("Topic '{name}' has {file_count} files to sync");
            if verbose {
                ui::info(&format!("Syncing topic '{name}'"));
                ui::bullet(&format!("{file_count} files scanned"));
            }
            let mut our_tree_editor = self
                .store
                .repo
                .edit_tree(self.store.repo.empty_tree().id())?;
            for file in topic_files {
                let relative_path = to_tree_path(file.strip_prefix(&topic_base)?);
                let content = std::fs::read(&file)?;
                let content = if let Some(ref key) = info.to_repo {
                    crate::manifest::run_hook(&manifest.lua, name, "to_repo", key, &relative_path, &content)?
                } else {
                    content
                };
                let blob_oid = self.store.repo.write_blob(content)?;
                our_tree_editor.upsert(
                    relative_path,
                    gix::objs::tree::EntryKind::Blob,
                    blob_oid,
                )?;
            }
            let our_tree_oid = our_tree_editor.write()?;
            trace!("Built our tree: {our_tree_oid}");

            let their_tree_oid: ObjectId =
                if let Some(commit_oid) = self.store.get_topic_head(name)? {
                    trace!("Found their tree from topic head: {commit_oid}");
                    self.store
                        .repo
                        .find_object(commit_oid)?
                        .peel_to_tree()?
                        .id()
                        .into()
                } else {
                    debug!("No topic head found, using empty tree");
                    self.store.repo.empty_tree().id().into()
                };

            let base_tree_oid: ObjectId =
                if let Some(commit_oid) = self.store.get_topic_hostname_head(name, &hostname)? {
                    trace!("Found base tree from hostname head: {commit_oid}");
                    self.store
                        .repo
                        .find_object(commit_oid)?
                        .peel_to_tree()?
                        .id()
                        .into()
                } else {
                    debug!("No hostname head found, using empty tree");
                    self.store.repo.empty_tree().id().into()
                };

            let mut merge_outcome = self.store.repo.merge_trees(
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
                self.store.repo.tree_merge_options()?,
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
                        Self::conflict_location_description(ours_change, theirs_change);
                    ui::info(&format!("  unresolved conflict at {location_description}"));
                }
                continue;
            }

            if !dry_run {
                for conflict in &merge_outcome.conflicts {
                    if !conflict.is_unresolved(TreatAsUnresolved::default()) {
                        continue;
                    }
                    self.resolve_conflict(conflict, &mut merge_outcome.tree)?;
                }
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
                    let signature = gix::actor::Signature {
                        name: "Porchetta".into(),
                        email: "".into(),
                        time: gix::date::Time::now_utc(),
                    };
                    let mut commit = gix::objs::Commit {
                        tree: merged_tree_oid.into(),
                        parents: self
                            .store
                            .get_topic_head(name)?
                            .into_iter()
                            .chain(
                                self.store
                                    .get_topic_hostname_head(name, &hostname)?
                                    .into_iter(),
                            )
                            .collect(),
                        message: format!("Sync topic '{name}'").into(),
                        author: signature.clone(),
                        committer: signature,
                        encoding: None,
                        extra_headers: vec![],
                    };
                    commit.parents.dedup();
                    let commit_oid = self.store.repo.write_object(commit)?.into();
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
                let operations = self
                    .collect_apply_operations(our_tree_oid.into(), merged_tree_oid.into())
                    .with_context(|| {
                        format!("Failed to compute apply operations for topic '{name}'")
                    })?;

                if dry_run {
                    ui::bullet(&format!(
                        "would apply {} change(s) to system",
                        operations.len()
                    ));
                    for op in &operations {
                        match op {
                            ApplyOperation::Upsert { relative_path, .. } => {
                                ui::info(&format!("  would upsert {relative_path}"));
                            }
                            ApplyOperation::Delete { relative_path } => {
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

                    Self::preflight_apply_operations(&topic_base, name, &operations)
                        .with_context(|| {
                            format!("Pre-flight checks failed for topic '{name}'")
                        })?;

                    self.apply_operations(&topic_base, name, &manifest.lua, info.to_system.as_ref(), operations)
                        .with_context(|| format!("Failed to apply changes for topic '{name}'"))?;
                }
            }

            let status = if dry_run {
                if pushed && pulled {
                    "would sync"
                } else if pushed {
                    "would push"
                } else if pulled {
                    "would apply"
                } else {
                    "unchanged"
                }
            } else if pushed && pulled {
                "synced"
            } else if pushed {
                "pushed"
            } else if pulled {
                "applied"
            } else {
                "unchanged"
            };
            ui::bullet(&format!("{name} — {status}"));

            if !dry_run {
                let topic_head = self.store
                    .get_topic_head(name)?
                    .context("Missing topic head for existing topic")?;
                let old_system_head = self.store.get_topic_hostname_head(name, &hostname)?;
                self.store.update_topic_hostname_head(name, &hostname, topic_head)?;
                if old_system_head != Some(topic_head) {
                    refs_to_push.push(format!("refs/heads/system/{hostname}/{name}"));
                }
            }

            info!("Synchronized topic '{name}'");
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

    fn resolve_conflict(
        &self,
        conflict: &gix::merge::tree::Conflict,
        merged_tree: &mut gix::object::tree::Editor<'_>,
    ) -> Result<()> {
        let (ours_change, theirs_change) = conflict.changes_in_resolution();
        let location_description = Self::conflict_location_description(ours_change, theirs_change);

        warn!("Encountered unresolved conflict at {location_description}");
        debug!("Conflict resolution failure: {:?}", conflict.resolution);
        debug!("Our change: {ours_change:?}");
        debug!("Their change: {theirs_change:?}");

        if Self::is_blob_level_conflict(conflict, ours_change, theirs_change) {
            self.resolve_blob_level_conflict(conflict, merged_tree)?;
        } else {
            Self::resolve_tree_level_conflict(conflict, merged_tree)?;
        }

        Ok(())
    }

    fn resolve_blob_level_conflict(
        &self,
        conflict: &gix::merge::tree::Conflict,
        merged_tree: &mut gix::object::tree::Editor<'_>,
    ) -> Result<()> {
        let (ours_change, theirs_change) = conflict.changes_in_resolution();
        ensure!(
            ours_change.location() == theirs_change.location(),
            "Blob-level conflict unexpectedly changed location from '{}' to '{}'",
            ours_change.location().to_str_lossy(),
            theirs_change.location().to_str_lossy()
        );

        let content_merge = conflict
            .content_merge()
            .context("Expected merged blob for blob-level conflict")?;
        let edited_blob_id = self.edit_blob_in_editor(content_merge.merged_blob_id)?;

        let entry_kind = if ours_change.entry_mode().kind() == theirs_change.entry_mode().kind() {
            ours_change.entry_mode().kind()
        } else {
            Self::entry_kind_for_shared_location(ours_change, theirs_change)?
        };

        merged_tree.upsert(ours_change.location(), entry_kind, edited_blob_id)?;
        Ok(())
    }

    fn resolve_tree_level_conflict(
        conflict: &gix::merge::tree::Conflict,
        merged_tree: &mut gix::object::tree::Editor<'_>,
    ) -> Result<()> {
        use inquire::Select;

        let (ours_change, theirs_change) = conflict.changes_in_resolution();
        let prompt = Self::tree_conflict_prompt(conflict, ours_change, theirs_change);
        let choice = Select::new(
            &prompt,
            vec!["Keep local (ours)", "Keep remote (theirs)", "Abort sync"],
        )
        .prompt()
        .context("User canceled conflict resolution")?;

        match choice {
            "Keep local (ours)" => {
                Self::remove_change_effect_from_tree(merged_tree, theirs_change)?;
                Self::apply_change_to_tree(merged_tree, ours_change)?;
            }
            "Keep remote (theirs)" => {
                Self::remove_change_effect_from_tree(merged_tree, ours_change)?;
                Self::apply_change_to_tree(merged_tree, theirs_change)?;
            }
            "Abort sync" => bail!("Sync aborted by user while resolving conflict"),
            _ => bail!("Invalid conflict resolution choice"),
        }

        Ok(())
    }

    fn tree_conflict_prompt(
        conflict: &gix::merge::tree::Conflict,
        ours_change: &gix::diff::tree_with_rewrites::Change,
        theirs_change: &gix::diff::tree_with_rewrites::Change,
    ) -> String {
        let location_description = Self::conflict_location_description(ours_change, theirs_change);
        let resolution_description = match &conflict.resolution {
            Ok(resolution) => format!("Resolution: {resolution:?}"),
            Err(failure) => format!("Unresolved reason: {failure:?}"),
        };

        let mut prompt = format!(
            "Resolve tree conflict at {location_description}\n\n{resolution_description}\n\nOur change: {}\nTheir change: {}",
            Self::describe_change(ours_change),
            Self::describe_change(theirs_change),
        );

        if conflict.content_merge().is_some() {
            prompt.push_str("\n\nA merged blob exists, but this conflict still needs a structural decision.");
        }

        prompt.push_str("\n\nChoose which side to keep:");
        prompt
    }

    fn conflict_location_description(
        ours_change: &gix::diff::tree_with_rewrites::Change,
        theirs_change: &gix::diff::tree_with_rewrites::Change,
    ) -> String {
        let ours_location = ours_change.location();
        let theirs_location = theirs_change.location();

        if ours_location == theirs_location {
            format!("'{}'", ours_location.to_str_lossy())
        } else {
            format!(
                "ours='{}', theirs='{}'",
                ours_location.to_str_lossy(),
                theirs_location.to_str_lossy()
            )
        }
    }

    fn describe_change(change: &gix::diff::tree_with_rewrites::Change) -> String {
        match change {
            gix::diff::tree_with_rewrites::Change::Addition { location, entry_mode, .. } => {
                format!("add {:?} at '{}'", entry_mode.kind(), location.to_str_lossy())
            }
            gix::diff::tree_with_rewrites::Change::Deletion { location, entry_mode, .. } => {
                format!("delete {:?} at '{}'", entry_mode.kind(), location.to_str_lossy())
            }
            gix::diff::tree_with_rewrites::Change::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                format!(
                    "modify '{}' ({:?} -> {:?})",
                    location.to_str_lossy(),
                    previous_entry_mode.kind(),
                    entry_mode.kind()
                )
            }
            gix::diff::tree_with_rewrites::Change::Rewrite {
                source_location,
                location,
                source_entry_mode,
                entry_mode,
                copy,
                ..
            } => {
                let action = if *copy { "copy" } else { "rename" };
                format!(
                    "{action} {:?} '{}' -> '{}' ({:?} -> {:?})",
                    source_entry_mode.kind(),
                    source_location.to_str_lossy(),
                    location.to_str_lossy(),
                    source_entry_mode.kind(),
                    entry_mode.kind()
                )
            }
        }
    }

    fn is_blob_level_conflict(
        conflict: &gix::merge::tree::Conflict,
        ours_change: &gix::diff::tree_with_rewrites::Change,
        theirs_change: &gix::diff::tree_with_rewrites::Change,
    ) -> bool {
        conflict.content_merge().is_some()
            && ours_change.location() == theirs_change.location()
            && ours_change.entry_mode().is_blob_or_symlink()
            && theirs_change.entry_mode().is_blob_or_symlink()
    }

    fn entry_kind_for_shared_location(
        ours_change: &gix::diff::tree_with_rewrites::Change,
        theirs_change: &gix::diff::tree_with_rewrites::Change,
    ) -> Result<gix::objs::tree::EntryKind> {
        use inquire::Select;

        let ours_kind = ours_change.entry_mode().kind();
        let theirs_kind = theirs_change.entry_mode().kind();

        if ours_kind == theirs_kind {
            return Ok(ours_kind);
        }

        let choice = Select::new(
            "Local and remote entries have different kinds. Which should be used?",
            vec!["Use local kind (ours)", "Use remote kind (theirs)"],
        )
        .prompt()
        .context("User canceled kind selection for edited content")?;

        match choice {
            "Use local kind (ours)" => Ok(ours_kind),
            "Use remote kind (theirs)" => Ok(theirs_kind),
            _ => bail!("Invalid entry kind choice"),
        }
    }

    fn edit_blob_in_editor(&self, blob_oid: ObjectId) -> Result<ObjectId> {
        use std::io::Write;

        let blob = self
            .store
            .repo
            .find_blob(blob_oid)
            .with_context(|| format!("Failed to read merged blob '{blob_oid}' for conflict"))?;

        let tempfile = tempfile::NamedTempFile::new()
            .context("Failed to create temporary file for merge conflict")?;
        tempfile
            .as_file()
            .write_all(&blob.data)
            .context("Failed to write merged content to temporary file for conflict")?;

        let editor = crate::util::get_editor()
            .context("Failed to determine editor for merge conflict resolution")?;
        let status = std::process::Command::new(editor)
            .arg(tempfile.path())
            .status()
            .context("Failed to launch editor for merge conflict resolution")?;
        if !status.success() {
            bail!("Editor exited with non-zero status during merge conflict resolution");
        }

        let edited_content = std::fs::read(tempfile.path())
            .context("Failed to read edited conflict content from temporary file")?;
        Ok(self.store.repo.write_blob(edited_content)?.into())
    }

    fn apply_change_to_tree(
        tree: &mut gix::object::tree::Editor<'_>,
        change: &gix::diff::tree_with_rewrites::Change,
    ) -> Result<()> {
        match change {
            gix::diff::tree_with_rewrites::Change::Addition {
                location,
                entry_mode,
                id,
                ..
            }
            | gix::diff::tree_with_rewrites::Change::Modification {
                location,
                entry_mode,
                id,
                ..
            } => {
                tree.upsert(location.as_bstr(), entry_mode.kind(), *id)?;
            }
            gix::diff::tree_with_rewrites::Change::Deletion { location, .. } => {
                tree.remove(location.as_bstr())?;
            }
            gix::diff::tree_with_rewrites::Change::Rewrite {
                source_location,
                location,
                entry_mode,
                id,
                copy,
                ..
            } => {
                if !*copy {
                    tree.remove(source_location.as_bstr())?;
                }
                tree.upsert(location.as_bstr(), entry_mode.kind(), *id)?;
            }
        }

        Ok(())
    }

    fn remove_change_effect_from_tree(
        tree: &mut gix::object::tree::Editor<'_>,
        change: &gix::diff::tree_with_rewrites::Change,
    ) -> Result<()> {
        match change {
            gix::diff::tree_with_rewrites::Change::Addition { location, .. }
            | gix::diff::tree_with_rewrites::Change::Modification { location, .. }
            | gix::diff::tree_with_rewrites::Change::Rewrite { location, .. } => {
                tree.remove(location.as_bstr())?;
            }
            gix::diff::tree_with_rewrites::Change::Deletion { .. } => {}
        }

        Ok(())
    }

    fn collect_apply_operations(
        &self,
        our_tree_oid: ObjectId,
        merged_tree_oid: ObjectId,
    ) -> Result<Vec<ApplyOperation>> {
        let our_tree = self.store.repo.find_tree(our_tree_oid)?;
        let merged_tree = self.store.repo.find_tree(merged_tree_oid)?;

        let mut operations = Vec::new();
        let mut changes = our_tree.changes()?;
        changes.options(|options| {
            options.track_path();
            options.track_rewrites(None);
        });

        changes.for_each_to_obtain_tree(&merged_tree, |change| {
            match change {
                gix::object::tree::diff::Change::Addition {
                    location,
                    entry_mode,
                    id,
                    ..
                }
                | gix::object::tree::diff::Change::Modification {
                    location,
                    entry_mode,
                    id,
                    ..
                } => {
                    if entry_mode.is_tree() {
                        return Ok(ControlFlow::Continue(()));
                    }

                    operations.push(ApplyOperation::Upsert {
                        relative_path: Self::diff_location_to_path(location)?,
                        blob_oid: id.detach(),
                    });
                }
                gix::object::tree::diff::Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => {
                    if entry_mode.is_tree() {
                        return Ok(ControlFlow::Continue(()));
                    }

                    operations.push(ApplyOperation::Delete {
                        relative_path: Self::diff_location_to_path(location)?,
                    });
                }
                gix::object::tree::diff::Change::Rewrite { .. } => {
                    bail!("Rewrite operation encountered despite rewrite tracking being disabled");
                }
            }

            Ok(ControlFlow::Continue(()))
        })?;

        Ok(operations)
    }

    fn preflight_apply_operations(
        topic_base: &Utf8Path,
        topic_name: &str,
        operations: &[ApplyOperation],
    ) -> Result<()> {
        for operation in operations {
            match operation {
                ApplyOperation::Upsert { relative_path, .. } => {
                    let abs_path = topic_base.join(relative_path);

                    if abs_path.is_dir() {
                        bail!(
                            "Topic '{topic_name}' cannot write file '{abs_path}' because it is a directory"
                        );
                    }

                    if let Some(parent) = abs_path.parent()
                        && parent.is_file()
                    {
                        bail!(
                            "Topic '{topic_name}' cannot create '{abs_path}' because parent '{parent}' is a file"
                        );
                    }
                }
                ApplyOperation::Delete { relative_path } => {
                    let abs_path = topic_base.join(relative_path);
                    if abs_path.is_dir() {
                        bail!(
                            "Topic '{topic_name}' cannot delete '{abs_path}' as a file because it is a directory"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_operations(
        &self,
        topic_base: &Utf8Path,
        topic_name: &str,
        lua: &mlua::Lua,
        to_system: Option<&mlua::RegistryKey>,
        operations: Vec<ApplyOperation>,
    ) -> Result<()> {
        for operation in operations {
            match operation {
                ApplyOperation::Upsert {
                    relative_path,
                    blob_oid,
                } => {
                    let abs_path = topic_base.join(&relative_path);

                    if let Some(parent) = abs_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!(
                                "Failed to create parent directory '{parent}' for topic '{topic_name}'"
                            )
                        })?;
                    }

                    let blob = self.store.repo.find_blob(blob_oid).with_context(|| {
                        format!(
                            "Failed to read blob '{blob_oid}' for topic '{topic_name}'"
                        )
                    })?;

                    if let Some(key) = to_system {
                        let content = crate::manifest::run_hook(
                            lua, topic_name, "to_system", key, relative_path.as_str(), &blob.data,
                        )?;
                        std::fs::write(&abs_path, content).with_context(|| {
                            format!(
                                "Failed to write '{abs_path}' for topic '{topic_name}'"
                            )
                        })?;
                    } else {
                        std::fs::write(&abs_path, &blob.data).with_context(|| {
                            format!(
                                "Failed to write '{abs_path}' for topic '{topic_name}'"
                            )
                        })?;
                    }
                }
                ApplyOperation::Delete { relative_path } => {
                    let abs_path = topic_base.join(relative_path);
                    if abs_path.exists() {
                        std::fs::remove_file(&abs_path).with_context(|| {
                            format!(
                                "Failed to delete '{abs_path}' for topic '{topic_name}'"
                            )
                        })?;
                    }
                }
            }
        }

        Ok(())
    }

    fn diff_location_to_path(location: &gix::bstr::BStr) -> Result<Utf8PathBuf> {
        let relative_path = std::str::from_utf8(location.as_ref()).with_context(|| {
            format!("Diff path '{}' is not valid UTF-8", location.to_str_lossy())
        })?;
        Ok(from_tree_path(relative_path))
    }
}
