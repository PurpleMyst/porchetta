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
    fn git_output_at(
        repo_path: &std::path::Path,
        args: &[&str],
        operation: &str,
    ) -> Result<std::process::Output> {
        let output = std::process::Command::new("git")
            .current_dir(repo_path)
            .args(["-c", "core.filesRefLockTimeout=5000"])
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

    fn git_output(&self, args: &[&str], operation: &str) -> Result<std::process::Output> {
        Self::git_output_at(self.repo.path(), args, operation)
    }

    /// Returns all configured URL remotes in lexical name order.
    ///
    /// # Errors
    ///
    /// Returns an error if Git cannot read the remote configuration or it contains
    /// non-UTF-8 data, invalid remote names, or missing or invalid URLs.
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        let repo = gix::open(self.repo.path())?;
        let names = repo.remote_names();
        let mut remotes = Vec::with_capacity(names.len());
        for name in names {
            let name = std::str::from_utf8(name.as_ref())
                .context("Git remote list contains non-UTF-8 data")?;
            Remote::validate_name(name)?;
            let remote = repo
                .find_remote(name)
                .with_context(|| format!("Failed to read remote '{name}'"))?;
            let url = remote
                .url(gix::remote::Direction::Fetch)
                .with_context(|| format!("Remote '{name}' has no fetch URL"))?
                .to_bstring();
            let url = std::str::from_utf8(url.as_ref())
                .with_context(|| format!("URL for remote '{name}' is not UTF-8"))?;
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

    fn fetch_at(repo_path: &std::path::Path, remote: &Remote) -> Result<()> {
        let name = remote.name();
        let manifest_refspec = format!("+refs/heads/manifest*:refs/remotes/{name}/manifest*");
        let topics_refspec = Branch::topic("*").fetch_refspec(name);
        Self::git_output_at(
            repo_path,
            &[
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--prune",
                name,
                &manifest_refspec,
                &topics_refspec,
            ],
            "fetch",
        )?;
        Ok(())
    }

    /// Fetches only `manifest` and `topic/*` into this remote's tracking namespace.
    ///
    /// Existing tracking refs are force-updated, while tags and all `system/*` refs
    /// are excluded.
    ///
    /// # Implementation notes
    ///
    /// Git fails any exact refspec whose ref is missing on the remote, whereas
    /// wildcard refspecs tolerate zero matches and participate in `--prune`. Using a
    /// wildcard refspec for `manifest*` allows a single `git fetch --prune` to both
    /// bootstrap an empty remote and prune a deleted manifest tracking ref.
    ///
    /// # Errors
    ///
    /// Returns an error if the `git` binary is unavailable or the fetch fails.
    pub fn fetch(&self, remote: &Remote) -> Result<()> {
        Self::fetch_at(self.repo.path(), remote)
    }

    /// Fetches all `remotes` concurrently.
    ///
    /// # Errors
    ///
    /// Returns the first fetch error in `remotes` order if any remote fails.
    pub(crate) fn fetch_remotes(&self, remotes: &[Remote]) -> Result<()> {
        let repo_path = self.repo.path();
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = remotes
                .iter()
                .map(|remote| scope.spawn(move || Self::fetch_at(repo_path, remote)))
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|e| std::panic::resume_unwind(e))
                })
                .collect()
        });
        for (remote, result) in remotes.iter().zip(results) {
            result.with_context(|| format!("Failed to fetch remote '{}'", remote.name()))?;
        }
        Ok(())
    }

    fn push_refspecs(&self, remote: &Remote, topics: &[&str]) -> Result<Option<Vec<String>>> {
        let mut refspecs = vec![format!("{0}:{0}", Branch::Manifest.ref_name())];
        let mut needs_push =
            self.head(&Branch::Manifest)? != self.remote_head(remote, &Branch::Manifest)?;
        let mut seen = std::collections::BTreeSet::new();
        for &topic in topics {
            let branch = Branch::topic(topic);
            let ref_name = branch.ref_name();
            let _: gix::refs::FullName = ref_name
                .clone()
                .try_into()
                .with_context(|| format!("Invalid topic name '{topic}'"))?;
            if seen.insert(topic)
                && let Some(local_head) = self.head(&branch)?
            {
                needs_push |= self.remote_head(remote, &branch)? != Some(local_head);
                refspecs.push(format!("{ref_name}:{ref_name}"));
            }
        }
        Ok(needs_push.then_some(refspecs))
    }

    fn push_refspecs_at(
        repo_path: &std::path::Path,
        remote: &Remote,
        refspecs: &[String],
    ) -> Result<()> {
        let mut args = vec!["push", "--atomic", remote.name()];
        args.extend(refspecs.iter().map(String::as_str));
        Self::git_output_at(repo_path, &args, "push")?;
        Ok(())
    }

    /// Atomically pushes the manifest and declared topic branches to one remote without force.
    ///
    /// Missing local topic branches are skipped, and the push is elided entirely if
    /// all target refs already match the remote's tracking refs. No local system refs
    /// are pushed.
    ///
    /// # Errors
    ///
    /// Returns an error if a topic ref or Git operation is invalid, or if the remote
    /// rejects the non-fast-forward update.
    pub fn push_topics(&self, remote: &Remote, topics: &[&str]) -> Result<()> {
        let Some(refspecs) = self.push_refspecs(remote, topics)? else {
            return Ok(());
        };
        Self::push_refspecs_at(self.repo.path(), remote, &refspecs)
    }

    /// Atomically pushes the manifest and declared topic branches to all `remotes` concurrently.
    #[must_use]
    pub(crate) fn push_topics_to_remotes(
        &self,
        remotes: &[Remote],
        topics: &[&str],
    ) -> Vec<Result<()>> {
        let plans: Vec<_> = remotes
            .iter()
            .map(|remote| self.push_refspecs(remote, topics))
            .collect();
        let repo_path = self.repo.path();
        std::thread::scope(|scope| {
            let handles: Vec<_> = remotes
                .iter()
                .zip(plans)
                .map(|(remote, plan)| {
                    scope.spawn(move || match plan? {
                        Some(refspecs) => Self::push_refspecs_at(repo_path, remote, &refspecs),
                        None => Ok(()),
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|e| std::panic::resume_unwind(e))
                })
                .collect()
        })
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
