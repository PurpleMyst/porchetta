use std::collections::{HashSet, VecDeque};

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
use log::{debug, info, trace, warn};

use crate::manifest::Manifest;
use crate::store::PorchettaStore;

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

            if merge_outcome.has_unresolved_conflicts(Default::default()) {
                warn!(
                    "Merge conflicts detected for topic '{}' - resolution not yet implemented",
                    name
                );
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
                // The merged tree is different from our tree, so we need to update the files on the system.
                warn!(
                    "Topic '{}' has remote changes - file update not yet implemented",
                    name
                );
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
}
