use anyhow::{Context, Result, bail};

use super::{Branch, PorchettaStore};

/// A configured remote with a validated name and URL.
#[derive(Clone, Debug)]
pub struct Remote {
    name: String,
    url: String,
}

impl Remote {
    fn new(name: impl Into<String>, url: impl Into<String>) -> Result<Self> {
        let name = name.into();
        Self::validate_name(&name)?;
        let url = url.into();
        if url.is_empty() {
            bail!("Remote URL cannot be empty");
        }
        Ok(Self { name, url })
    }

    pub(crate) fn validate_name(name: &str) -> Result<()> {
        if name.starts_with('-') {
            bail!("Invalid remote name '{name}': names cannot start with '-'");
        }
        let tracking_ref = format!("refs/remotes/{name}/manifest");
        let _: gix::refs::FullName = tracking_ref
            .try_into()
            .with_context(|| format!("Invalid remote name '{name}'"))?;
        Ok(())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl PorchettaStore {
    fn git_output(&self, args: &[&str], operation: &str) -> Result<std::process::Output> {
        let output = std::process::Command::new("git")
            .current_dir(self.repo.path())
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {operation}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            if detail.is_empty() {
                bail!("git {operation} failed with status {}", output.status);
            }
            bail!("git {operation} failed: {detail}");
        }
        Ok(output)
    }

    /// Returns all configured URL remotes in lexical name order.
    ///
    /// # Errors
    ///
    /// Returns an error if Git cannot read the remote configuration or it contains
    /// non-UTF-8 data, invalid remote names, or missing or invalid URLs.
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let output = self.git_output(&["remote"], "remote list")?;
        let stdout = std::str::from_utf8(&output.stdout)
            .context("Git remote list contains non-UTF-8 data")?;
        let mut names: Vec<_> = stdout.lines().filter(|name| !name.is_empty()).collect();
        names.sort_unstable();

        let mut remotes = Vec::with_capacity(names.len());
        for name in names {
            Remote::validate_name(name)?;
            let operation = format!("remote URL lookup for '{name}'");
            let output = self.git_output(&["remote", "get-url", name], &operation)?;
            let url = std::str::from_utf8(&output.stdout)
                .with_context(|| format!("URL for remote '{name}' is not UTF-8"))?
                .trim_end_matches(&['\r', '\n'][..]);
            remotes.push(Remote::new(name, url)?);
        }
        Ok(remotes)
    }

    /// Adds a URL remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the name or URL is invalid, the remote already exists,
    /// or Git cannot update the repository configuration.
    pub fn add_remote(&self, name: &str, url: &str) -> Result<Remote> {
        let remote = Remote::new(name, url)?;
        self.git_output(
            &["remote", "add", "--no-tags", remote.name(), remote.url()],
            "remote add",
        )?;
        Ok(remote)
    }

    /// Removes a configured remote and its remote-tracking refs.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is invalid, the remote does not exist, or Git
    /// cannot update the repository configuration and refs.
    pub fn remove_remote(&self, name: &str) -> Result<()> {
        Remote::validate_name(name)?;
        self.git_output(&["remote", "remove", name], "remote remove")?;
        Ok(())
    }

    /// Fetches only `manifest` and `topic/*` into this remote's tracking namespace.
    ///
    /// Existing tracking refs are force-updated, while tags and all `system/*` refs
    /// are excluded.
    ///
    /// # Implementation notes
    ///
    /// Git fails any exact refspec whose ref is missing on the remote, whether the
    /// refspec comes from the command line or from `remote.<name>.fetch` config;
    /// only wildcard refspecs tolerate zero matches. The manifest refspec is
    /// therefore fetched only after `ls-remote` confirms it exists, so a fresh
    /// remote without a manifest can still be bootstrapped.
    ///
    /// `--prune` only covers refs matched by the fetch's refspecs, so when the
    /// remote has no manifest the stale tracking ref is deleted explicitly.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is unavailable or the fetch fails.
    pub fn fetch(&self, remote: &Remote) -> Result<()> {
        let manifest = Branch::Manifest;
        let manifest_exists = !self
            .git_output(
                &["ls-remote", "--heads", remote.name(), &manifest.ref_name()],
                "ls-remote",
            )?
            .stdout
            .is_empty();

        let manifest_refspec = manifest.fetch_refspec(remote.name());
        let topics_refspec = Branch::topic("*").fetch_refspec(remote.name());
        let mut args = vec!["fetch", "--no-tags", "--prune", remote.name()];
        if manifest_exists {
            args.push(&manifest_refspec);
        }
        args.push(&topics_refspec);
        self.git_output(&args, "fetch")?;
        if !manifest_exists {
            let tracking_ref = manifest.tracking_ref_name(remote.name());
            self.git_output(&["update-ref", "-d", &tracking_ref], "update-ref")?;
        }
        Ok(())
    }

    /// Atomically pushes the manifest and declared topic branches to one remote without force.
    ///
    /// Missing local topic branches are skipped. No local system refs are pushed.
    ///
    /// # Errors
    ///
    /// Returns an error if a topic ref or Git operation is invalid, or if the remote
    /// rejects the non-fast-forward update.
    pub fn push_topics(&self, remote: &Remote, topics: &[&str]) -> Result<()> {
        let mut refspecs = vec![format!("{0}:{0}", Branch::Manifest.ref_name())];
        let mut seen = std::collections::BTreeSet::new();
        for &topic in topics {
            let branch = Branch::topic(topic);
            let ref_name = branch.ref_name();
            let _: gix::refs::FullName = ref_name
                .clone()
                .try_into()
                .with_context(|| format!("Invalid topic name '{topic}'"))?;
            if seen.insert(topic) && self.head(&branch)?.is_some() {
                refspecs.push(format!("{ref_name}:{ref_name}"));
            }
        }

        let mut args = vec!["push", "--atomic", remote.name()];
        args.extend(refspecs.iter().map(String::as_str));
        self.git_output(&args, "push")?;
        Ok(())
    }

    /// Gets the head commit for a branch on a remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the branch name is invalid or the reference cannot be read.
    pub fn remote_head(&self, remote: &Remote, branch: &Branch) -> Result<Option<gix::ObjectId>> {
        let ref_name = branch.tracking_ref_name(remote.name());
        let _: gix::refs::FullName = ref_name
            .clone()
            .try_into()
            .with_context(|| format!("Invalid remote branch '{ref_name}'"))?;
        self.ref_head(&ref_name)
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::*;

    #[test]
    fn remotes_validate_names_before_passing_them_to_git() {
        let temp = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(temp.path()).unwrap();
        let store = PorchettaStore::init_at(path).unwrap();
        let status = std::process::Command::new("git")
            .current_dir(path)
            .args(["config", "remote.-v.url", "example"])
            .status()
            .unwrap();
        assert!(status.success());

        let error = store.remotes().unwrap_err();

        assert!(error.to_string().contains("names cannot start with '-'"));
    }
}
