use camino::{Utf8Path, Utf8PathBuf};
use dirs::home_dir;

use anyhow::{Context, Result, bail};
use log::{debug, info, trace};
use smallvec::SmallVec;

#[derive(Debug)]
pub struct PorchettaStore {
    pub repo: gix::Repository,
}

impl PorchettaStore {
    /// Initializes a new Porchetta store.
    ///
    /// # Errors
    ///
    /// Returns an error if the store path cannot be determined or if any git operation fails.
    /// Initializes a new Porchetta store at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be initialized.
    pub fn init_at(path: &Utf8Path) -> Result<Self> {
        let repo = gix::init_bare(path)?;
        info!("Initialized Porchetta store at {path}");
        let manifest_content = b"return { topics = {} }\n";
        let this = Self { repo };
        this.write_manifest(manifest_content)?;
        Ok(this)
    }

    /// Initializes a new Porchetta store at the default location.
    ///
    /// # Errors
    ///
    /// Returns an error if the store path cannot be determined or if any git operation fails.
    pub fn init() -> Result<Self> {
        Self::init_at(&Self::store_path()?)
    }

    /// Loads the Porchetta store from the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be opened.
    pub fn load_at(path: &Utf8Path) -> Result<Self> {
        let repo = gix::open(path)?;
        info!("Loaded Porchetta store from {path}");
        Ok(Self { repo })
    }

    /// Clones a remote Porchetta store into the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if `git` is not available, if the clone fails, or if the
    /// resulting repository cannot be opened.
    pub fn clone_from(url: &str, path: &Utf8Path) -> Result<Self> {
        let status = std::process::Command::new("git")
            .args(["clone", "--bare", url, path.as_str()])
            .status()
            .context("Failed to run git clone")?;
        if !status.success() {
            bail!("git clone failed with non-zero exit code");
        }
        Self::load_at(path)
    }

    /// Loads the Porchetta store from the default location.
    ///
    /// # Errors
    ///
    /// Returns an error if the store path cannot be determined or if the store cannot be opened.
    pub fn load() -> Result<Self> {
        Self::load_at(&Self::store_path()?)
    }

    /// Returns the store path for a given home directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the resulting path is not valid UTF-8.
    pub fn store_path_for(home: &std::path::Path) -> Result<Utf8PathBuf> {
        let path = Utf8PathBuf::try_from(home.join(".porchetta"))
            .map_err(|e| anyhow::anyhow!("store path is not valid UTF-8: {e}"))?;
        trace!("Store path resolved to {path}");
        Ok(path)
    }

    /// Returns the path to the Porchetta store.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    pub fn store_path() -> Result<Utf8PathBuf> {
        let home = home_dir().context("Could not determine home directory")?;
        Self::store_path_for(&home)
    }

    /// Reads the manifest from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest reference cannot be found or read.
    pub fn read_manifest(&self) -> Result<Vec<u8>> {
        debug!("Reading manifest from store");
        let content = self
            .repo
            .find_reference("heads/manifest")?
            .peel_to_tree()?
            .find_entry("manifest.lua")
            .context("Manifest entry not found in tree")?
            .object()?
            .try_into_blob()?
            .take_data();
        let len = content.len();
        debug!("Successfully read manifest ({len} bytes)");
        Ok(content)
    }

    /// Writes the manifest to the store.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the manifest blob or updating the reference fails.
    pub fn write_manifest(&self, manifest_content: &[u8]) -> Result<()> {
        debug!(
            "Writing manifest to store ({} bytes)",
            manifest_content.len()
        );
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
                .map_or(SmallVec::default(), |r| [r.target().id().to_owned()].into()),
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
        info!("Manifest written successfully");
        Ok(())
    }

    fn get_branch_head(&self, branch: &str) -> Result<Option<gix::ObjectId>> {
        let reference_name = format!("refs/heads/{branch}");
        trace!("Looking up branch head for '{reference_name}'");
        match self.repo.find_reference(&reference_name) {
            Ok(reference) => Ok(Some(
                reference
                    .target()
                    .try_id()
                    .context("Reference does not point to an object id")?
                    .to_owned(),
            )),
            Err(gix::reference::find::existing::Error::NotFound { .. }) => {
                debug!("Branch '{reference_name}' not found");
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Gets the head commit for a topic.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic branch cannot be read.
    pub fn get_topic_head(&self, topic: &str) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("topic/{topic}");
        self.get_branch_head(&branch_name)
    }

    /// Gets the head commit for a topic on the current hostname.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic hostname branch cannot be read.
    pub fn get_topic_hostname_head(
        &self,
        topic: &str,
        hostname: &str,
    ) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("system/{hostname}/{topic}");
        self.get_branch_head(&branch_name)
    }

    fn update_branch_head(&self, branch: &str, new_head: gix::ObjectId) -> Result<()> {
        let reference_name = format!("refs/heads/{branch}");
        self.repo.reference(
            reference_name.as_str(),
            new_head,
            gix::refs::transaction::PreviousValue::Any,
            format!("Update head of branch {branch} to {new_head}"),
        )?;
        Ok(())
    }

    /// Updates the head commit for a topic.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic branch cannot be updated.
    pub fn update_topic_head(&self, topic: &str, new_head: gix::ObjectId) -> Result<()> {
        let branch_name = format!("topic/{topic}");
        debug!("Updating topic head for '{topic}' to {new_head}");
        self.update_branch_head(&branch_name, new_head)
    }

    /// Updates the head commit for a topic on the current hostname.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic hostname branch cannot be updated.
    pub fn update_topic_hostname_head(
        &self,
        topic: &str,
        hostname: &str,
        new_head: gix::ObjectId,
    ) -> Result<()> {
        let branch_name = format!("system/{hostname}/{topic}");
        debug!(
            "Updating topic hostname head for '{hostname}/{topic}' to {new_head}"
        );
        self.update_branch_head(&branch_name, new_head)
    }

    /// Gets the head commit for the manifest branch.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest reference cannot be read.
    pub fn get_manifest_head(&self) -> Result<Option<gix::ObjectId>> {
        self.get_branch_head("manifest")
    }

    /// Updates the head commit for the manifest branch.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest reference cannot be updated.
    pub fn update_manifest_head(&self, new_head: gix::ObjectId) -> Result<()> {
        self.update_branch_head("manifest", new_head)
    }

    /// Returns whether the store has a remote named `origin`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary cannot be executed.
    pub fn has_origin(&self) -> Result<bool> {
        let output = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .args(["config", "--get", "remote.origin.url"])
            .output()
            .context("Failed to run git config")?;
        Ok(output.status.success())
    }

    /// Runs `git fetch origin` in the store repository.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is missing or the fetch fails.
    pub fn git_fetch(&self) -> Result<()> {
        let status = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .args([
                "fetch",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
            ])
            .status()
            .context("Failed to run git fetch")?;
        if !status.success() {
            bail!("git fetch failed");
        }
        Ok(())
    }

    /// Runs `git merge-base --is-ancestor` to test ancestry.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is missing or the command fails unexpectedly.
    pub fn git_ancestor_check(
        &self,
        ancestor: gix::ObjectId,
        descendant: gix::ObjectId,
    ) -> Result<bool> {
        let status = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .args([
                "merge-base",
                "--is-ancestor",
                &ancestor.to_string(),
                &descendant.to_string(),
            ])
            .status()
            .context("Failed to run git merge-base")?;
        if status.success() {
            Ok(true)
        } else if status.code() == Some(1) {
            Ok(false)
        } else {
            bail!("git merge-base failed with unexpected exit code");
        }
    }

    /// Pushes the given refs to `origin`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is missing or the push fails.
    pub fn git_push(&self, refs: &[String]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let status = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .arg("push")
            .arg("origin")
            .args(refs)
            .status()
            .context("Failed to run git push")?;
        if !status.success() {
            bail!("git push failed");
        }
        Ok(())
    }

    /// Gets the head commit for a topic on a remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the remote tracking branch cannot be read.
    pub fn get_remote_topic_head(
        &self,
        remote: &str,
        topic: &str,
    ) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("{remote}/topic/{topic}");
        self.get_remote_branch_head(&branch_name)
    }

    /// Gets the head commit for the manifest on a remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the remote tracking branch cannot be read.
    pub fn get_remote_manifest_head(&self, remote: &str) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("{remote}/manifest");
        self.get_remote_branch_head(&branch_name)
    }

    fn get_remote_branch_head(&self, branch: &str) -> Result<Option<gix::ObjectId>> {
        let reference_name = format!("refs/remotes/{branch}");
        trace!("Looking up remote branch head for '{reference_name}'");
        match self.repo.find_reference(&reference_name) {
            Ok(reference) => Ok(Some(
                reference
                    .target()
                    .try_id()
                    .context("Reference does not point to an object id")?
                    .to_owned(),
            )),
            Err(gix::reference::find::existing::Error::NotFound { .. }) => {
                debug!("Remote branch '{reference_name}' not found");
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}
