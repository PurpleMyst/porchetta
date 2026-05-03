use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use dirs::home_dir;
use gix::bstr::{BString, ByteSlice};
use log::{debug, info, trace};

#[derive(Debug)]
pub struct PorchettaStore {
    repo: gix::Repository,
}

impl PorchettaStore {
    /// Returns the standard Porchetta commit signature.
    #[must_use]
    pub fn porchetta_signature() -> gix::actor::Signature {
        gix::actor::Signature {
            name: "Porchetta".into(),
            email: "".into(),
            time: gix::date::Time::now_utc(),
        }
    }

    // -- thin wrappers over gix::Repository --

    #[allow(clippy::missing_errors_doc)]
    pub fn empty_tree_id(&self) -> gix::ObjectId {
        self.repo.empty_tree().id().into()
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn edit_tree(&self, id: impl Into<gix::ObjectId>) -> Result<gix::object::tree::Editor<'_>> {
        Ok(self.repo.edit_tree(id)?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn write_blob(&self, data: impl AsRef<[u8]>) -> Result<gix::Id<'_>> {
        Ok(self.repo.write_blob(data)?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn find_object(&self, id: impl Into<gix::ObjectId>) -> Result<gix::Object<'_>> {
        Ok(self.repo.find_object(id)?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn find_blob(&self, id: impl Into<gix::ObjectId>) -> Result<gix::Blob<'_>> {
        Ok(self.repo.find_blob(id)?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn find_tree(&self, id: impl Into<gix::ObjectId>) -> Result<gix::Tree<'_>> {
        Ok(self.repo.find_tree(id)?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn merge_trees(
        &self,
        base: impl AsRef<gix::oid>,
        ours: impl AsRef<gix::oid>,
        theirs: impl AsRef<gix::oid>,
        name: impl std::fmt::Display,
    ) -> Result<gix::merge::tree::Outcome<'_>> {
        Ok(self.repo.merge_trees(
            base,
            ours,
            theirs,
            gix::merge::blob::builtin_driver::text::Labels {
                ancestor: Some(BString::from(format!("{name} (last applied)")).as_bstr()),
                current: Some(BString::from(format!("{name} (on system)")).as_bstr()),
                other: Some(BString::from(format!("{name} (in repo)")).as_bstr()),
            },
            self.repo.tree_merge_options()?,
        )?)
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn write_object(&self, object: impl gix::objs::WriteTo) -> Result<gix::Id<'_>> {
        Ok(self.repo.write_object(object)?)
    }

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
    ///
    /// # Implementation note
    ///
    /// We shell out to the `git` CLI rather than using `gix` directly so that
    /// the user's credential helpers, SSH agent, and `~/.gitconfig` are inherited
    /// automatically.
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
        let commit_oid = self.repo.write_object(gix::objs::Commit {
            tree: tree_oid.into(),
            parents: self
                .repo
                .try_find_reference("heads/manifest")?
                .and_then(|r| r.target().try_id().map(|id| [id.to_owned()].into()))
                .unwrap_or_default(),
            author: Self::porchetta_signature(),
            committer: Self::porchetta_signature(),
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

    fn fast_forward_branch(&self, branch: &str, remote_oid: gix::ObjectId) -> Result<()> {
        let Some(local_oid) = self.get_branch_head(branch)? else {
            self.update_branch_head(branch, remote_oid)?;
            return Ok(());
        };

        if remote_oid == local_oid {
            return Ok(());
        }

        if self.git_ancestor_check(remote_oid, local_oid)? {
            // remote is ancestor of local, local is ahead
            return Ok(());
        }

        if self.git_ancestor_check(local_oid, remote_oid)? {
            self.update_branch_head(branch, remote_oid)?;
            return Ok(());
        }

        bail!(
            "Cannot fast-forward branch '{branch}' from {local_oid} to {remote_oid} because they have diverged"
        );
    }

    /// Fast-forwards the manifest branch to `remote_oid` if possible.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or updating the branch fails.
    pub fn fast_forward_manifest(&self, remote_oid: gix::ObjectId) -> Result<()> {
        self.fast_forward_branch("manifest", remote_oid)
    }

    /// Fast-forwards a topic branch to `remote_oid` if possible.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or updating the branch fails.
    pub fn fast_forward_topic(&self, topic: &str, remote_oid: gix::ObjectId) -> Result<()> {
        self.fast_forward_branch(&format!("topic/{topic}"), remote_oid)
    }

    /// Fast-forwards a topic/hostname branch to `remote_oid` if possible.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or updating the branch fails.
    pub fn fast_forward_topic_hostname(
        &self,
        topic: &str,
        hostname: &str,
        remote_oid: gix::ObjectId,
    ) -> Result<()> {
        self.fast_forward_branch(&format!("system/{hostname}/{topic}"), remote_oid)
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

    /// Updates the system hostname head for a topic to match the topic branch head.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic head cannot be read or the branch cannot be updated.
    pub fn update_topic_hostname_head(&self, topic: &str, hostname: &str) -> Result<()> {
        let topic_head = self
            .get_topic_head(topic)?
            .context("Missing topic head for existing topic")?;
        let branch_name = format!("system/{hostname}/{topic}");
        debug!("Updating topic hostname head for '{hostname}/{topic}' to {topic_head}");
        self.update_branch_head(&branch_name, topic_head)
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
    /// Returns an error if the repository configuration cannot be read.
    pub fn has_origin(&self) -> Result<bool> {
        Ok(self
            .repo
            .config_snapshot()
            .string("remote.origin.url")
            .is_some())
    }

    /// Runs `git fetch origin` in the store repository.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is missing or the fetch fails.
    ///
    /// # Implementation note
    ///
    /// We shell out to the `git` CLI rather than using `gix` directly so that
    /// the user's credential helpers, SSH agent, and `~/.gitconfig` are inherited
    /// automatically.
    pub fn git_fetch(&self) -> Result<()> {
        let status = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .args(["fetch", "origin", "+refs/heads/*:refs/remotes/origin/*"])
            .status()
            .context("Failed to run git fetch")?;
        if !status.success() {
            bail!("git fetch failed");
        }
        Ok(())
    }

    /// Tests whether `ancestor` is an ancestor of `descendant`.
    ///
    /// # Errors
    ///
    /// Returns an error if the object graph cannot be traversed.
    pub fn git_ancestor_check(
        &self,
        ancestor: gix::ObjectId,
        descendant: gix::ObjectId,
    ) -> Result<bool> {
        match self.repo.merge_base(ancestor, descendant) {
            Ok(base) => Ok(base == ancestor),
            Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Pushes all Porchetta refs to `origin`.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is missing or the push fails.
    ///
    /// # Implementation note
    ///
    /// We shell out to the `git` CLI rather than using `gix` directly so that
    /// the user's credential helpers, SSH agent, and `~/.gitconfig` are inherited
    /// automatically.
    pub fn git_push_all(&self, hostname: &str) -> Result<()> {
        let refspecs = [
            "refs/heads/manifest",
            "refs/heads/topic/*",
            &format!("refs/heads/system/{hostname}/*"),
        ];
        let status = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .arg("push")
            .arg("origin")
            .args(refspecs)
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

    /// Gets the head commit for a topic/hostname pair on a remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the remote tracking branch cannot be read.
    pub fn get_remote_topic_hostname_head(
        &self,
        remote: &str,
        topic: &str,
        hostname: &str,
    ) -> Result<Option<gix::ObjectId>> {
        let branch_name = format!("{remote}/system/{hostname}/{topic}");
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

    // -- compound operations --

    /// Returns the tree OID for a topic, or the empty tree if the topic has no head.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic head cannot be read or peeled.
    pub fn get_topic_tree_oid(&self, topic: &str) -> Result<gix::ObjectId> {
        let oid = match self.get_topic_head(topic)? {
            Some(commit_oid) => self.find_object(commit_oid)?.peel_to_tree()?.id().into(),
            None => self.empty_tree_id(),
        };
        trace!("Resolved topic '{topic}' tree to {oid}");
        Ok(oid)
    }

    /// Returns the tree OID for a topic on the current hostname, or the empty tree if none.
    ///
    /// # Errors
    ///
    /// Returns an error if the hostname head cannot be read or peeled.
    pub fn get_topic_hostname_tree_oid(
        &self,
        topic: &str,
        hostname: &str,
    ) -> Result<gix::ObjectId> {
        let oid = match self.get_topic_hostname_head(topic, hostname)? {
            Some(commit_oid) => self.find_object(commit_oid)?.peel_to_tree()?.id().into(),
            None => self.empty_tree_id(),
        };
        trace!("Resolved topic '{topic}' hostname '{hostname}' tree to {oid}");
        Ok(oid)
    }

    /// Creates a commit for a topic tree and returns the commit OID.
    ///
    /// Parents are the current topic head and hostname head (deduplicated).
    ///
    /// # Errors
    ///
    /// Returns an error if reading heads or writing the commit fails.
    pub fn commit_topic_tree(
        &self,
        topic: &str,
        hostname: &str,
        tree_oid: impl Into<gix::ObjectId>,
        message: impl Into<gix::bstr::BString>,
    ) -> Result<gix::ObjectId> {
        let mut parents: smallvec::SmallVec<[gix::ObjectId; 1]> = self
            .get_topic_head(topic)?
            .into_iter()
            .chain(self.get_topic_hostname_head(topic, hostname)?)
            .collect();
        parents.dedup();
        let commit = gix::objs::Commit {
            tree: tree_oid.into(),
            parents,
            message: message.into(),
            author: Self::porchetta_signature(),
            committer: Self::porchetta_signature(),
            encoding: None,
            extra_headers: vec![],
        };
        let commit_oid: gix::ObjectId = self.write_object(commit)?.into();
        let branch_name = format!("topic/{topic}");
        debug!("Committed topic '{topic}' as {commit_oid}");
        self.update_branch_head(&branch_name, commit_oid)?;
        Ok(commit_oid)
    }
}
