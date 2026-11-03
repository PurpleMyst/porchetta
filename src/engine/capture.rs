use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use gix::ObjectId;
use log::{debug, trace};

use crate::manifest::{Manifest, Topic};

/// Normalize line endings in the content to LF. If the content is not valid UTF-8, it is returned as-is.
fn normalize_line_endings(content: Vec<u8>) -> Vec<u8> {
    let Some(content_str) = std::str::from_utf8(&content).ok() else {
        return content;
    };
    content_str.replace("\r\n", "\n").into_bytes()
}

fn to_tree_path(path: &Utf8Path) -> String {
    path.as_str().replace('\\', "/")
}

/// Walk `topic`'s paths under `topic_base`, apply `should_include` and `to_repo`,
/// and write the resulting snapshot into a new git tree.
pub fn snapshot_topic(
    repo: &gix::Repository,
    manifest: &Manifest,
    topic: &Topic,
    topic_base: &Utf8Path,
) -> Result<ObjectId> {
    let mut editor = repo.edit_tree(repo.empty_tree().id())?;
    let mut file_count = 0;
    let mut queue = VecDeque::new();

    for p in &topic.paths {
        let abs_path = topic_base.join(p);
        if abs_path.exists() {
            queue.push_back(abs_path);
        } else {
            debug!("Skipping missing path '{abs_path}'");
        }
    }

    while let Some(path) = queue.pop_front() {
        if path.file_name() == Some(".git") {
            debug!("Skipping .git directory at '{path}'");
            continue;
        }
        let relative_path = to_tree_path(path.strip_prefix(topic_base)?);
        if !manifest.includes(topic, &relative_path)? {
            debug!("Excluding '{path}' based on should_include hook");
            continue;
        }
        if path.is_file() {
            trace!("Found file: {path}");
            let content =
                std::fs::read(&path).with_context(|| format!("Failed to read file '{path}'"))?;
            let content = normalize_line_endings(topic.to_repo(&relative_path, &content)?);
            let blob_oid = repo.write_blob(&content)?;
            editor.upsert(&relative_path, gix::objs::tree::EntryKind::Blob, blob_oid)?;
            file_count += 1;
        } else if path.is_dir() {
            trace!("Queueing directory: {path}");
            for entry in std::fs::read_dir(&path)? {
                let entry_path = Utf8PathBuf::try_from(entry?.path())
                    .context("non-UTF-8 path encountered during scan")?;
                queue.push_back(entry_path);
            }
        } else {
            bail!("Path '{path}' does not exist or is not a file/directory");
        }
    }

    debug!("Captured {file_count} files from topic paths");
    Ok(editor.write()?.into())
}
