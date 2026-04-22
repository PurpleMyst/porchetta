use std::collections::{HashSet, VecDeque};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
use log::{debug, info, trace, warn};

use crate::manifest::Manifest;
use crate::store::PorchettaStore;

/*
for conflict in &outcome.conflicts {
           if conflict.resolution.is_ok() {
               continue; // Already resolved
           }

           let (ours_change, their_change) = conflict.changes_in_resolution();
           let path = ours_change.location();
           let entries = conflict.entries(); // [base, ours, theirs]

           // Read blob contents
           let base_content = entries[0].as_ref()
               .map(|e| repo.find_blob(e.id, &mut vec![]).map(|b| b.data.to_vec()))
               .transpose()?.unwrap_or_default();

           let ours_content = entries[1].as_ref()
               .map(|e| repo.find_blob(e.id, &mut vec![]).map(|b| b.data.to_vec()))
               .transpose()?.unwrap_or_default();

           let theirs_content = entries[2].as_ref()
               .map(|e| repo.find_blob(e.id, &mut vec![]).map(|b| b.data.to_vec()))
               .transpose()?.unwrap_or_default();

           // Generate conflict markers
           let mut output = vec![];
           let resolution = builtin_driver::text(
               &mut output,
               &mut Default::default(),
               Labels {
                   ancestor: Some("base".into()),
                   current: Some("HEAD".into()),
                   other: Some("feature".into()),
               },
               &ours_content,
               &base_content,
               &theirs_content,
               builtin_driver::text::Options {
                   diff_algorithm: imara_diff::Algorithm::Myers,
                   conflict: builtin_driver::text::Conflict::Keep {
                       style: ConflictStyle::Merge,
                       marker_size: 7.try_into().unwrap(),
                   },
               },
           );

           // Write to worktree
           let file_path = workdir.join(path.as_ref());
           if let Some(parent) = file_path.parent() {
               std::fs::create_dir_all(parent)?;
           }
           std::fs::write(&file_path, &output)?;

           println!("Wrote conflict markers to: {}", path);
       }
 */

enum ApplyOperation {
    Upsert {
        relative_path: PathBuf,
        blob_oid: ObjectId,
    },
    Delete {
        relative_path: PathBuf,
    },
}

pub struct PorchettaEngine {
    store: PorchettaStore,
}

impl PorchettaEngine {
    pub fn new(store: PorchettaStore) -> Self {
        Self { store }
    }

    pub fn edit_manifest(&mut self, editor: impl FnOnce(&[u8]) -> Vec<u8>) -> Result<()> {
        debug!("Starting manifest edit");
        let manifest_content = self.store.read_manifest()?;
        let new_manifest_content = editor(&manifest_content);
        self.store.write_manifest(&new_manifest_content)?;
        debug!("Manifest edit completed");
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        debug!("Starting sync operation");
        let home = std::env::home_dir().context("Could not determine home directory")?;
        let manifest = Manifest::load(&self.store.read_manifest()?)?;
        info!("Loaded manifest with {} topics", manifest.topics.len());

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();
        debug!("Detected hostname: {}", hostname);

        for (name, info) in manifest.topics {
            debug!("Syncing topic '{}'", name);

            // Capture and create system ("our") tree.
            let mut topic_files = HashSet::new();

            for p in &info.paths {
                let abs_path = home.join(p);
                if abs_path.is_file() {
                    trace!("Found file: {}", abs_path.display());
                    topic_files.insert(abs_path);
                } else if abs_path.is_dir() {
                    trace!("Scanning directory: {}", abs_path.display());
                    let mut queue = VecDeque::new();
                    queue.push_back(abs_path);
                    while let Some(p2) = queue.pop_front() {
                        if p2.is_file() {
                            trace!("Found file: {}", p2.display());
                            topic_files.insert(p2);
                        } else if p2.is_dir() {
                            for entry in std::fs::read_dir(p2)? {
                                queue.push_back(entry?.path());
                            }
                        } else {
                            bail!(
                                "Path '{}' does not exist or is not a file/directory",
                                p2.display()
                            );
                        }
                    }
                } else {
                    bail!(
                        "Path '{}' does not exist or is not a file/directory",
                        abs_path.display()
                    );
                }
            }

            debug!("Topic '{}' has {} files to sync", name, topic_files.len());

            // let Some(mut common_ancestor) = info.paths.iter().cloned().reduce(|a, b| {
            //     let mut c = PathBuf::new();
            //     for (a_part, b_part) in a.iter().zip(b.iter()) {
            //         if a_part == b_part {
            //             c.push(a_part);
            //         } else {
            //             break;
            //         }
            //     }
            //     c
            // }) else {
            //     continue;
            // };
            //
            // common_ancestor = home.join(common_ancestor);
            //
            // if common_ancestor.is_file() {
            //     common_ancestor.pop();
            // }
            //
            // // XXX: this practically hangs if common_ancestor == home
            // let our_tree = RootedTree::capture(
            //     common_ancestor,
            //     |_p, c| Ok(c),
            //     |p| {
            //         info.paths.iter().any(|tp| {
            //             p.strip_prefix(&home)
            //                 .unwrap_or(p)
            //                 .components()
            //                 .zip(tp.components())
            //                 .all(|(a, b)| a == b)
            //         })
            //     },
            // )?;
            //
            // for (oid, obj) in our_tree.objects.iter() {
            //     let new_oid = self.store.repo.write_object(obj.clone())?;
            //     debug_assert_eq!(*oid, ObjectId::from(new_oid));
            // }

            let mut our_tree_editor = self
                .store
                .repo
                .edit_tree(self.store.repo.empty_tree().id())?;
            for file in topic_files {
                let content = std::fs::read(&file)?;
                let blob_oid = self.store.repo.write_blob(content)?;
                let relative_path = file.strip_prefix(&home)?.to_str().with_context(|| {
                    format!(
                        "Failed to convert path '{}' to string",
                        file.strip_prefix(&home).unwrap_or(&file).display()
                    )
                })?;
                our_tree_editor.upsert(
                    relative_path,
                    gix::objs::tree::EntryKind::Blob,
                    blob_oid,
                )?;
            }
            let our_tree_oid = our_tree_editor.write()?;
            trace!("Built our tree: {}", our_tree_oid);

            let their_tree_oid: ObjectId =
                if let Some(commit_oid) = self.store.get_topic_head(&name)? {
                    trace!("Found their tree from topic head: {}", commit_oid);
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
                if let Some(commit_oid) = self.store.get_topic_hostname_head(&name, &hostname)? {
                    trace!("Found base tree from hostname head: {}", commit_oid);
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
                &base_tree_oid,
                &our_tree_oid,
                &their_tree_oid,
                Labels {
                    ancestor: Some(
                        gix::bstr::BString::from(format!("{} (last applied)", name)).as_bstr(),
                    ),
                    current: Some(
                        gix::bstr::BString::from(format!("{} (on system)", name)).as_bstr(),
                    ),
                    other: Some(gix::bstr::BString::from(format!("{} (in repo)", name)).as_bstr()),
                },
                self.store.repo.tree_merge_options()?,
            )?;

            for conflict in &merge_outcome.conflicts {
                use std::io::Write;

                if !conflict.is_unresolved(Default::default()) {
                    continue;
                }

                let Some(cm) = conflict.content_merge() else {
                    bail!("Expected content merge for conflict {conflict:?}, but got none");
                };

                let blob = self
                    .store
                    .repo
                    .find_blob(cm.merged_blob_id)
                    .with_context(|| {
                        format!(
                            "Failed to read merged blob '{}' for conflict",
                            cm.merged_blob_id
                        )
                    })?;

                let tempfile = tempfile::NamedTempFile::new()
                    .context("Failed to create temporary file for merge conflict")?;
                tempfile
                    .as_file()
                    .write_all(&blob.data)
                    .context("Failed to write merged content to temporary file for conflict")?;
                let editor =
                    std::env::var("EDITOR").context("EDITOR environment variable is not set")?;
                let status = std::process::Command::new(editor)
                    .arg(tempfile.path())
                    .status()
                    .context("Failed to launch editor for merge conflict resolution")?;
                if !status.success() {
                    bail!("Editor exited with non-zero status during merge conflict resolution");
                }

                let our_location = conflict.ours.location();
                let their_location = conflict.theirs.location();
                ensure!(
                    our_location == their_location,
                    "Expected conflict locations to match, but got '{}' and '{}'",
                    our_location.to_str_lossy(),
                    their_location.to_str_lossy()
                );
                let location = our_location;

                merge_outcome.tree.upsert(
                    Self::diff_location_to_path(location)?.to_str().unwrap(),
                    gix::objs::tree::EntryKind::Blob,
                    self.store
                        .repo
                        .write_blob(std::fs::read(tempfile.path())?)?,
                )?;
            }

            let merged_tree_oid = merge_outcome.tree.write()?;
            trace!("Merged tree: {}", merged_tree_oid);

            if merged_tree_oid != their_tree_oid {
                // The merged tree is different from the one in the repo, so we need to create a new commit and update the topic head.
                let signature = gix::actor::Signature {
                    name: "Porchetta".into(),
                    email: "".into(),
                    time: gix::date::Time::now_utc(),
                };
                let mut commit = gix::objs::Commit {
                    tree: merged_tree_oid.into(),
                    parents: self
                        .store
                        .get_topic_head(&name)?
                        .into_iter()
                        .chain(
                            self.store
                                .get_topic_hostname_head(&name, &hostname)?
                                .into_iter(),
                        )
                        .collect(),
                    message: format!("Sync topic '{}'", name).into(),
                    author: signature.clone(),
                    committer: signature,
                    encoding: None,
                    extra_headers: vec![],
                };
                commit.parents.dedup();
                let commit_oid = self.store.repo.write_object(commit)?.into();
                debug!("Created commit: {}", commit_oid);
                self.store.update_topic_head(&name, commit_oid)?;
            } else {
                debug!("Topic '{}' has no changes from repo", name);
            }

            if merged_tree_oid != our_tree_oid {
                if false {
                    warn!(
                        "Skipping apply for topic '{}' due to unresolved merge conflicts",
                        name
                    );
                } else {
                    let operations = self
                        .collect_apply_operations(our_tree_oid.into(), merged_tree_oid.into())
                        .with_context(|| {
                            format!("Failed to compute apply operations for topic '{}'", name)
                        })?;

                    self.preflight_apply_operations(&home, &name, &operations)
                        .with_context(|| {
                            format!("Pre-flight checks failed for topic '{}'", name)
                        })?;

                    self.apply_operations(&home, &name, operations)
                        .with_context(|| format!("Failed to apply changes for topic '{}'", name))?;
                }
            }

            self.store.update_topic_hostname_head(
                &name,
                &hostname,
                self.store
                    .get_topic_head(&name)?
                    .context("Missing topic head for existing topic")?,
            )?;

            info!("Synchronized topic '{}'", name);
        }

        debug!("Sync operation completed");
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
                gix::object::tree::diff::Change::Modification {
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
                gix::object::tree::diff::Change::Rewrite { .. } => {
                    bail!("Rewrite operation encountered despite rewrite tracking being disabled");
                }
            }

            Ok(ControlFlow::Continue(()))
        })?;

        Ok(operations)
    }

    fn preflight_apply_operations(
        &self,
        home: &Path,
        topic_name: &str,
        operations: &[ApplyOperation],
    ) -> Result<()> {
        for operation in operations {
            match operation {
                ApplyOperation::Upsert { relative_path, .. } => {
                    let abs_path = home.join(relative_path);

                    if abs_path.is_dir() {
                        bail!(
                            "Topic '{}' cannot write file '{}' because it is a directory",
                            topic_name,
                            abs_path.display()
                        );
                    }

                    if let Some(parent) = abs_path.parent()
                        && parent.is_file()
                    {
                        bail!(
                            "Topic '{}' cannot create '{}' because parent '{}' is a file",
                            topic_name,
                            abs_path.display(),
                            parent.display()
                        );
                    }
                }
                ApplyOperation::Delete { relative_path } => {
                    let abs_path = home.join(relative_path);
                    if abs_path.is_dir() {
                        bail!(
                            "Topic '{}' cannot delete '{}' as a file because it is a directory",
                            topic_name,
                            abs_path.display()
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_operations(
        &self,
        home: &Path,
        topic_name: &str,
        operations: Vec<ApplyOperation>,
    ) -> Result<()> {
        for operation in operations {
            match operation {
                ApplyOperation::Upsert {
                    relative_path,
                    blob_oid,
                } => {
                    let abs_path = home.join(&relative_path);

                    if let Some(parent) = abs_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!(
                                "Failed to create parent directory '{}' for topic '{}'",
                                parent.display(),
                                topic_name
                            )
                        })?;
                    }

                    let blob = self.store.repo.find_blob(blob_oid).with_context(|| {
                        format!(
                            "Failed to read blob '{}' for topic '{}'",
                            blob_oid, topic_name
                        )
                    })?;

                    std::fs::write(&abs_path, &blob.data).with_context(|| {
                        format!(
                            "Failed to write '{}' for topic '{}'",
                            abs_path.display(),
                            topic_name
                        )
                    })?;
                }
                ApplyOperation::Delete { relative_path } => {
                    let abs_path = home.join(relative_path);
                    if abs_path.exists() {
                        std::fs::remove_file(&abs_path).with_context(|| {
                            format!(
                                "Failed to delete '{}' for topic '{}'",
                                abs_path.display(),
                                topic_name
                            )
                        })?;
                    }
                }
            }
        }

        Ok(())
    }

    fn diff_location_to_path(location: &gix::bstr::BStr) -> Result<PathBuf> {
        let relative_path = std::str::from_utf8(location.as_ref()).with_context(|| {
            format!("Diff path '{}' is not valid UTF-8", location.to_str_lossy())
        })?;
        Ok(PathBuf::from(relative_path))
    }
}
