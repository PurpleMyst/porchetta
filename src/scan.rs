use std::collections::{HashSet, VecDeque};

use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use log::{debug, trace};

/// Walk the filesystem starting at `topic_base` + each path in `paths`,
/// returning the set of absolute file paths that should be captured.
///
/// The `should_include` closure is called with the tree-relative path
/// (forward-slash separated) and should return `true` if the file or
/// directory should be included.
///
/// # Errors
///
/// Returns an error if a path contains `..`, a path does not exist, or
/// the `should_include` closure fails.
pub fn scan_topic_files(
    topic_base: &Utf8Path,
    paths: &[Utf8PathBuf],
    mut should_include: impl FnMut(&str) -> Result<bool>,
) -> Result<HashSet<Utf8PathBuf>> {
    use crate::path_util::to_tree_path;

    for p in paths {
        ensure!(
            !p.components().any(|c| c == Utf8Component::ParentDir),
            "path '{p}' contains '..' which is not allowed"
        );
    }

    let mut topic_files = HashSet::new();

    for p in paths {
        let abs_path = topic_base.join(p);
        if abs_path.is_file() {
            let relative_path = to_tree_path(abs_path.strip_prefix(topic_base)?);
            if !should_include(&relative_path)? {
                continue;
            }
            trace!("Found file: {abs_path}");
            topic_files.insert(abs_path);
        } else if abs_path.is_dir() {
            trace!("Scanning directory: {abs_path}");
            let mut queue = VecDeque::new();
            queue.push_back(abs_path);
            while let Some(p2) = queue.pop_front() {
                if p2.file_name() == Some(".git") {
                    debug!("Skipping .git directory at '{p2}'");
                    continue;
                }
                if p2.is_file() {
                    let relative_path = to_tree_path(p2.strip_prefix(topic_base)?);
                    if !should_include(&relative_path)? {
                        debug!("Excluding file '{p2}' based on should_include hook");
                        continue;
                    }
                    trace!("Found file: {p2}");
                    topic_files.insert(p2);
                } else if p2.is_dir() {
                    let relative_path = to_tree_path(p2.strip_prefix(topic_base)?);
                    if !should_include(&relative_path)? {
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
                    bail!("Path '{p2}' does not exist or is not a file/directory");
                }
            }
        } else {
            bail!("Path '{abs_path}' does not exist or is not a file/directory");
        }
    }

    Ok(topic_files)
}
