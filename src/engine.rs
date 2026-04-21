use std::path::PathBuf;

use anyhow::{Context, Result};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::merge::blob::builtin_driver::text::Labels;
use gix::objs::Tree;

use crate::manifest::Manifest;
use crate::rooted_tree::RootedTree;
use crate::store::PorchettaStore;

pub struct PorchettaEngine {
    store: PorchettaStore,
}

impl PorchettaEngine {
    pub fn new(store: PorchettaStore) -> Self {
        Self { store }
    }

    pub fn edit_manifest(&mut self, editor: impl FnOnce(&[u8]) -> Vec<u8>) -> Result<()> {
        let manifest_content = self.store.read_manifest()?;
        let new_manifest_content = editor(&manifest_content);
        self.store.write_manifest(&new_manifest_content)?;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        let home = std::env::home_dir().context("Could not determine home directory")?;
        let manifest = Manifest::load(&self.store.read_manifest()?)?;

        let hostname = ::hostname::get()
            .context("Could not get hostname")?
            .to_string_lossy()
            .into_owned();

        for (name, info) in manifest.topics {
            let Some(mut common_ancestor) = info.paths.iter().cloned().reduce(|a, b| {
                let mut c = PathBuf::new();
                for (a_part, b_part) in a.iter().zip(b.iter()) {
                    if a_part == b_part {
                        c.push(a_part);
                    } else {
                        break;
                    }
                }
                c
            }) else {
                continue;
            };

            common_ancestor = home.join(common_ancestor);

            if common_ancestor.is_file() {
                common_ancestor.pop();
            }

            // XXX: this practically hangs if common_ancestor == home
            let our_tree = RootedTree::capture(
                common_ancestor,
                |_p, c| Ok(c),
                |p| {
                    info.paths.iter().any(|tp| {
                        p.strip_prefix(&home)
                            .unwrap_or(p)
                            .components()
                            .zip(tp.components())
                            .all(|(a, b)| a == b)
                    })
                },
            )?;

            for (oid, obj) in our_tree.objects.iter() {
                let new_oid = self.store.repo.write_object(obj.clone())?;
                debug_assert_eq!(*oid, ObjectId::from(new_oid));
            }

            let their_tree_oid: ObjectId =
                if let Some(commit_oid) = self.store.get_topic_head(&name)? {
                    self.store
                        .repo
                        .find_object(commit_oid)?
                        .peel_to_tree()?
                        .id()
                        .into()
                } else {
                    self.get_empty_tree_oid()?
                };

            let base_tree_oid: ObjectId =
                if let Some(commit_oid) = self.store.get_topic_hostname_head(&name, &hostname)? {
                    self.store
                        .repo
                        .find_object(commit_oid)?
                        .peel_to_tree()?
                        .id()
                        .into()
                } else {
                    self.get_empty_tree_oid()?
                };

            let mut merge_outcome = self.store.repo.merge_trees(
                &base_tree_oid,
                &our_tree.tree_oid,
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
                todo!("Implement conflict resolution strategy for merge conflicts in topic '{}'", name);
            }

            let merged_tree_oid = merge_outcome.tree.write()?;

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
                        .chain(self.store.get_topic_hostname_head(&name, &hostname)?.into_iter())
                        .collect(),
                    message: format!("Sync topic '{}'", name).into(),
                    author: signature.clone(),
                    committer: signature,
                    encoding: None,
                    extra_headers: vec![],
                };
                commit.parents.dedup();
                let commit_oid = self.store.repo.write_object(commit)?.into();
                self.store.update_topic_head(&name, commit_oid)?;
            }

            if merged_tree_oid != our_tree.tree_oid {
                // The merged tree is different from our tree, so we need to update the files on the system.
                todo!("Implement file updates on the system based on the merged tree");
            }

            self.store
                .update_topic_hostname_head(&name, &hostname, self.store.get_topic_head(&name)?
                    .context("Missing topic head for existing topic")?)?;

            println!("Synchronized topic '{}'", name);
        }

        Ok(())
    }

    fn get_empty_tree_oid(&self) -> Result<ObjectId> {
        let empty_tree = Tree::empty();
        Ok(self.store.repo.write_object(empty_tree)?.into())
    }
}
