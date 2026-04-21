use std::env::home_dir;

use anyhow::{Context, Result};

#[derive(Debug)]
pub struct PorchettaStore {
    pub repo: gix::Repository,
}

impl PorchettaStore {
    pub fn init() -> Result<Self> {
        let repo = gix::init_bare(Self::store_path()?)?;
        let manifest_content = b"return { topics = {} }\n";
        let this = Self { repo };
        this.write_manifest(manifest_content)?;
        Ok(this)
    }

    pub fn load() -> Result<Self> {
        let repo = gix::open(Self::store_path()?)?;
        Ok(Self { repo })
    }

    fn store_path() -> Result<std::path::PathBuf> {
        let home = home_dir().context("Could not determine home directory")?;
        Ok(home.join(".porchetta"))
    }

    pub fn read_manifest(&self) -> Result<Vec<u8>> {
        Ok(self
            .repo
            .find_reference("heads/manifest")?
            .peel_to_tree()?
            .find_entry("manifest.lua")
            .context("Manifest entry not found in tree")?
            .object()?
            .try_into_blob()?
            .take_data())
    }

    pub fn write_manifest(&self, manifest_content: &[u8]) -> Result<()> {
        let blob_oid = self
            .repo
            .write_blob(manifest_content)
            .context("Failed to write manifest blob")?;
        let tree_oid = self.repo.write_object(gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: b"manifest.lua".to_vec().into(),
                oid: blob_oid.into(),
            }],
        })?;
        let signature = gix::actor::Signature {
            name: "Porchetta".into(),
            email: "".into(),
            time: gix::date::Time::now_utc(),
        };
        let commit_oid = self.repo.write_object(gix::objs::Commit {
            tree: tree_oid.into(),
            parents: self
                .repo
                .try_find_reference("heads/manifest")?
                // XXX: ↓ We're not handling the id() error here, should we?
                .map_or(Default::default(), |r| [r.target().id().to_owned()].into()),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: "Update manifest".into(),
            extra_headers: vec![],
        })?;
        self.repo.reference(
            "refs/heads/manifest",
            commit_oid,
            gix::refs::transaction::PreviousValue::Any,
            "Update manifest branch",
        )?;
        Ok(())
    }

    fn get_branch_head(&self, branch: &str) -> Result<Option<gix::ObjectId>> {
        let reference_name = format!("refs/heads/{}", branch);
        match self.repo.find_reference(&reference_name) {
            Ok(reference) => Ok(Some(
                reference
                    .target()
                    .try_id()
                    .context("Reference does not point to an object id")?
                    .to_owned(),
            )),
            Err(gix::reference::find::existing::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_topic_head(&self, topic: &str) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("topic/{}", topic);
        self.get_branch_head(&branch_name)
    }

    pub fn get_topic_hostname_head(
        &self,
        topic: &str,
        hostname: &str,
    ) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("system/{}/{topic}", hostname);
        self.get_branch_head(&branch_name)
    }

    fn update_branch_head(&self, branch: &str, new_head: gix::ObjectId) -> Result<()> {
        let reference_name = format!("refs/heads/{}", branch);
        self.repo.reference(
            reference_name.as_str(),
            new_head,
            gix::refs::transaction::PreviousValue::Any,
            format!("Update head of branch {} to {}", branch, new_head),
        )?;
        // self.repo.edit_reference(RefEdit {
        //     name: FullName::try_from(reference_name)
        //         .context("Invalid reference name")?,
        //     change: gix::refs::transaction::Change::Update {
        //         log: LogChange {
        //             mode: RefLog::AndReference,
        //             force_create_reflog: false,
        //             message: format!("Update head of branch {} to {}", branch, new_head).into(),
        //         },
        //         expected: gix::refs::transaction::PreviousValue::Any,
        //         new: new_head.into(),
        //     },
        //     deref: false,
        // })?;
        Ok(())
    }

    pub fn update_topic_head(&self, topic: &str, new_head: gix::ObjectId) -> Result<()> {
        let branch_name = format!("topic/{topic}");
        self.update_branch_head(&branch_name, new_head)
    }

    pub fn update_topic_hostname_head(
        &self,
        topic: &str,
        hostname: &str,
        new_head: gix::ObjectId,
    ) -> Result<()> {
        let branch_name = format!("system/{hostname}/{topic}");
        self.update_branch_head(&branch_name, new_head)
    }
}
