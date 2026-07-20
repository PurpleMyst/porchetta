mod lock;
mod remote;

pub use remote::Remote;

use lock::StoreLock;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use dirs::home_dir;
use gix::bstr::ByteSlice;
use log::{debug, info, trace};

/// The entry holding the manifest in every manifest tree.
const MANIFEST_FILE: &str = "manifest.lua";

#[derive(Debug)]
pub struct PorchettaStore {
    _lock: StoreLock,
    repo: gix::Repository,
}

/// A Porchetta branch: the single owner of the store's ref naming layout.
#[derive(Clone, Debug)]
pub enum Branch {
    Manifest,
    Topic(String),
    System { hostname: String, topic: String },
}

impl Branch {
    /// Returns the branch shared for a topic.
    pub fn topic(name: impl Into<String>) -> Self {
        Self::Topic(name.into())
    }

    /// Returns the branch tracking a topic's state on a specific hostname.
    pub fn system(hostname: impl Into<String>, topic: impl Into<String>) -> Self {
        Self::System {
            hostname: hostname.into(),
            topic: topic.into(),
        }
    }

    /// The branch portion of the ref name, e.g. `topic/shell`.
    fn short_name(&self) -> String {
        match self {
            Self::Manifest => "manifest".to_owned(),
            Self::Topic(topic) => format!("topic/{topic}"),
            Self::System { hostname, topic } => format!("system/{hostname}/{topic}"),
        }
    }

    /// The full local ref name, e.g. `refs/heads/topic/shell`.
    fn ref_name(&self) -> String {
        format!("refs/heads/{}", self.short_name())
    }

    /// The full name of this branch's tracking ref on `remote`, e.g.
    /// `refs/remotes/origin/topic/shell`.
    fn tracking_ref_name(&self, remote: &str) -> String {
        format!("refs/remotes/{remote}/{}", self.short_name())
    }

    /// The fetch refspec mapping this branch into `remote`'s tracking namespace.
    ///
    /// `Branch::topic("*")` yields the wildcard refspec covering all topics.
    fn fetch_refspec(&self, remote: &str) -> String {
        format!("+{}:{}", self.ref_name(), self.tracking_ref_name(remote))
    }

    /// Parses a branch from its short name, the inverse of [`Branch::short_name`].
    fn parse(short_name: &str) -> Option<Self> {
        if short_name == "manifest" {
            Some(Self::Manifest)
        } else if let Some(topic) = short_name.strip_prefix("topic/") {
            Some(Self::Topic(topic.to_owned()))
        } else if let Some(rest) = short_name.strip_prefix("system/") {
            let (hostname, topic) = rest.split_once('/')?;
            Some(Self::System {
                hostname: hostname.to_owned(),
                topic: topic.to_owned(),
            })
        } else {
            None
        }
    }
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
    /// Returns the underlying git repository for git-level operations
    /// (object access, tree editing, merges).
    #[must_use]
    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// Initializes a new Porchetta store at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be initialized or the manifest cannot be written.
    pub fn init_at(path: &Utf8Path) -> Result<Self> {
        let lock = StoreLock::acquire(path)?;
        let repo = gix::init_bare(path)?;
        info!("Initialized Porchetta store at {path}");
        let manifest_content = b"return { topics = {} }\n";
        let this = Self { repo, _lock: lock };
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
        let lock = StoreLock::acquire(path)?;
        let repo = gix::open(path)?;
        info!("Loaded Porchetta store from {path}");
        Ok(Self { repo, _lock: lock })
    }

    /// Clones the Porchetta refs from a remote into the given path.
    ///
    /// Only `manifest` and `topic/*` are fetched. The URL is retained as
    /// `origin`, and the fetched refs are atomically bootstrapped as local heads.
    ///
    /// # Errors
    ///
    /// Returns an error if the destination exists or any initialization, remote,
    /// fetch, reference iteration, or reference update operation fails.
    pub fn clone_from(url: &str, path: &Utf8Path) -> Result<Self> {
        if path.exists() {
            bail!(
                "Porchetta store already exists at {path}\n\
                 Remove it first or run `porchetta init` if this is a new machine."
            );
        }
        let lock = StoreLock::acquire(path)?;
        let repo = match gix::init_bare(path) {
            Ok(repo) => repo,
            Err(error) => {
                if path.exists() {
                    std::fs::remove_dir_all(path).with_context(|| {
                        format!("Failed to clean up incomplete clone destination '{path}'")
                    })?;
                }
                return Err(error.into());
            }
        };
        let this = Self { repo, _lock: lock };
        let result = (|| {
            let origin = this.add_remote("origin", url)?;
            this.fetch(&origin)?;

            let prefix = "refs/remotes/origin/";
            let mut local_heads = Vec::new();
            {
                let reference_platform = this.repo.references()?;
                let references = reference_platform.prefixed(prefix)?;
                for reference in references {
                    let reference =
                        reference.map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    let reference_name = reference.name().as_bstr().to_str()?;
                    let short_name = reference_name
                        .strip_prefix(prefix)
                        .context("Fetched reference is outside the origin namespace")?;
                    // Only shared refs (manifest and topics) become local heads.
                    let Some(branch) = Branch::parse(short_name)
                        .filter(|branch| !matches!(branch, Branch::System { .. }))
                    else {
                        continue;
                    };
                    let oid = reference
                        .target()
                        .try_id()
                        .context("Fetched reference does not point to an object id")?
                        .to_owned();
                    local_heads.push((branch, oid));
                }
            }

            this.update_heads(&local_heads)
        })();

        match result {
            Ok(()) => Ok(this),
            Err(error) => {
                // Drop the repository before deleting the directory: its open
                // handles would otherwise prevent removal (notably on Windows).
                // The lock file lives outside the store directory, so it
                // survives the cleanup and is released last.
                let Self { _lock: lock, repo } = this;
                drop(repo);
                if path.exists() {
                    std::fs::remove_dir_all(path).with_context(|| {
                        format!("Failed to clean up incomplete clone destination '{path}'")
                    })?;
                }
                drop(lock);
                Err(error)
            }
        }
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

    /// Reads the manifest from the manifest branch head.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest reference cannot be found or read.
    pub fn read_manifest(&self) -> Result<Vec<u8>> {
        let head = self
            .head(&Branch::Manifest)?
            .context("Manifest branch has no head")?;
        self.read_manifest_at(head)
    }

    /// Reads the manifest stored in the tree of the given commit.
    ///
    /// # Errors
    ///
    /// Returns an error if the commit cannot be peeled or has no manifest entry.
    pub fn read_manifest_at(&self, head: gix::ObjectId) -> Result<Vec<u8>> {
        debug!("Reading manifest at {head}");
        let content = self
            .repo
            .find_object(head)?
            .peel_to_tree()?
            .find_entry(MANIFEST_FILE)
            .context("Manifest entry not found in tree")?
            .object()?
            .try_into_blob()?
            .take_data();
        debug!("Successfully read manifest ({} bytes)", content.len());
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
        let manifest_head = self.head(&Branch::Manifest)?;
        let blob_oid = self
            .repo
            .write_blob(manifest_content)
            .context("Failed to write manifest blob")?;
        let tree_oid = self.repo.write_object(gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: MANIFEST_FILE.into(),
                oid: blob_oid.into(),
            }],
        })?;
        let commit_oid = self.commit_tree(tree_oid, manifest_head, "Update manifest")?;
        self.update_heads(&[(Branch::Manifest, commit_oid)])?;
        info!("Manifest written successfully");
        Ok(())
    }

    /// Gets the head commit for a local branch.
    ///
    /// # Errors
    ///
    /// Returns an error if the reference cannot be read.
    pub fn head(&self, branch: &Branch) -> Result<Option<gix::ObjectId>> {
        self.ref_head(&branch.ref_name())
    }

    /// Gets the head commit for a full ref name, or `None` if it does not exist.
    fn ref_head(&self, ref_name: &str) -> Result<Option<gix::ObjectId>> {
        trace!("Looking up branch head for '{ref_name}'");
        match self.repo.find_reference(ref_name) {
            Ok(reference) => Ok(Some(
                reference
                    .target()
                    .try_id()
                    .context("Reference does not point to an object id")?
                    .to_owned(),
            )),
            Err(gix::reference::find::existing::Error::NotFound { .. }) => {
                debug!("Branch '{ref_name}' not found");
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Updates multiple local branch heads in one reference transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if an update is duplicated or the transaction fails.
    pub fn update_heads(&self, updates: &[(Branch, gix::ObjectId)]) -> Result<()> {
        let mut names = std::collections::BTreeSet::new();
        let mut edits = Vec::with_capacity(updates.len());

        for (branch, new_head) in updates {
            let ref_name = branch.ref_name();
            let full_name: gix::refs::FullName = ref_name
                .clone()
                .try_into()
                .with_context(|| format!("Invalid branch name '{}'", branch.short_name()))?;
            if !names.insert(ref_name) {
                bail!("Duplicate branch update for '{}'", branch.short_name());
            }

            edits.push(gix::refs::transaction::RefEdit {
                change: gix::refs::transaction::Change::Update {
                    log: gix::refs::transaction::LogChange {
                        mode: gix::refs::transaction::RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!(
                            "Update head of branch {} to {new_head}",
                            branch.short_name()
                        )
                        .into(),
                    },
                    expected: gix::refs::transaction::PreviousValue::Any,
                    new: gix::refs::Target::Object(*new_head),
                },
                name: full_name,
                deref: false,
            });
        }

        if !edits.is_empty() {
            self.repo.edit_references(edits)?;
        }
        Ok(())
    }

    /// Updates the system hostname head for a topic to match the topic branch head.
    ///
    /// # Errors
    ///
    /// Returns an error if the topic branch cannot be read or the reference cannot be updated.
    pub fn update_topic_hostname_head(&self, topic: &str, hostname: &str) -> Result<()> {
        let topic_head = self
            .head(&Branch::topic(topic))?
            .context("Missing topic head for existing topic")?;
        debug!("Updating topic hostname head for '{hostname}/{topic}' to {topic_head}");
        self.update_heads(&[(Branch::system(hostname, topic), topic_head)])
    }

    // -- compound operations --

    /// Writes a commit with exactly the supplied parents, in iterator order.
    ///
    /// Parent selection and branch updates are intentionally left to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error if the commit object cannot be written.
    pub fn commit_tree(
        &self,
        tree_oid: impl Into<gix::ObjectId>,
        parents: impl IntoIterator<Item = gix::ObjectId>,
        message: impl Into<gix::bstr::BString>,
    ) -> Result<gix::ObjectId> {
        let commit = gix::objs::Commit {
            tree: tree_oid.into(),
            parents: parents.into_iter().collect(),
            message: message.into(),
            author: Self::porchetta_signature(),
            committer: Self::porchetta_signature(),
            encoding: None,
            extra_headers: vec![],
        };
        Ok(self.repo.write_object(commit)?.into())
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
            .head(&Branch::topic(topic))?
            .into_iter()
            .chain(self.head(&Branch::system(hostname, topic))?)
            .collect();
        parents.dedup();
        let commit_oid = self.commit_tree(tree_oid, parents, message)?;
        debug!("Committed topic '{topic}' as {commit_oid}");
        self.update_heads(&[(Branch::topic(topic), commit_oid)])?;
        Ok(commit_oid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_commit_deduplicates_identical_parents() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(temp.path()).unwrap();
        let store = PorchettaStore::init_at(path).unwrap();
        let tree: gix::ObjectId = store.repo().empty_tree().id().into();
        let head = store.commit_tree(tree, [], "topic head").unwrap();
        store
            .update_heads(&[
                (Branch::topic("test"), head),
                (Branch::system("host", "test"), head),
            ])
            .unwrap();

        let installed = store
            .commit_topic_tree("test", "host", tree, "deduplicated parents")
            .unwrap();
        let parents: Vec<_> = store
            .repo()
            .find_object(installed)
            .unwrap()
            .try_into_commit()
            .unwrap()
            .parent_ids()
            .map(gix::Id::detach)
            .collect();
        assert_eq!(parents, [head]);
    }
}
