use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use gix::ObjectId;
use log::{debug, trace};

use crate::manifest::{Manifest, Topic};

/// Normalize line endings in the content to LF. If the content is not valid UTF-8, it is returned as-is.
fn normalize_line_endings(content: Vec<u8>) -> Vec<u8> {
    // XXX: Heuristic-y but in my own usage this works; we can add a topic-level is_binary callback
    // later if the need arises. ¯\_(ツ)_/¯
    if !content.contains(&b'\r') || content.contains(&0) {
        return content;
    }
    let Ok(content_str) = std::str::from_utf8(&content) else {
        // If the content is not valid UTF-8, return it as-is.
        return content;
    };
    // Normalize line endings to LF.
    content_str.replace("\r\n", "\n").into_bytes()
}

fn to_tree_path(path: &Utf8Path) -> String {
    path.as_str().replace('\\', "/")
}

/// Walk `topic`'s paths under `topic_base`, apply `should_include` and `to_repo`,
/// and write the resulting snapshot into a new git tree.
///
/// # Errors
///
/// Returns an error if the include hook fails, a file cannot be read,
/// the transform fails, or writing to the store fails.
pub fn snapshot_topic(
    repo: &gix::Repository,
    manifest: &Manifest,
    topic: &Topic,
    topic_base: &Utf8Path,
) -> Result<ObjectId> {
    let mut editor = repo.edit_tree(repo.empty_tree().id())?;
    let mut file_count = 0;
    let mut queue = VecDeque::new();

    // Seed the queue with all topic paths
    for p in &topic.paths {
        let abs_path = topic_base.join(p);
        if let Ok(meta) = std::fs::symlink_metadata(&abs_path) {
            queue.push_back((abs_path, meta.file_type()));
        } else {
            debug!("Skipping missing path '{abs_path}'");
        }
    }

    // Single unified processing loop
    while let Some((path, file_type)) = queue.pop_front() {
        if path.file_name() == Some(".git") {
            debug!("Skipping .git directory at '{path}'");
            continue;
        }
        let relative_path = to_tree_path(path.strip_prefix(topic_base)?);
        if !manifest.includes(topic, &relative_path)? {
            debug!("Excluding '{path}' based on should_include hook");
            continue;
        }
        if file_type.is_file() {
            trace!("Found file: {path}");
            let content = normalize_line_endings(
                std::fs::read(&path).with_context(|| format!("Failed to read file '{path}'"))?,
            );
            let content = normalize_line_endings(topic.to_repo(&relative_path, content)?);
            let blob_oid = repo.write_blob(&content)?;
            editor.upsert(&relative_path, gix::objs::tree::EntryKind::Blob, blob_oid)?;
            file_count += 1;
        } else if file_type.is_dir() {
            trace!("Queueing directory: {path}");
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                let entry_type = entry.file_type()?;
                let entry_path = Utf8PathBuf::try_from(entry.path())
                    .context("non-UTF-8 path encountered during scan")?;
                queue.push_back((entry_path, entry_type));
            }
        } else {
            bail!("Path '{path}' does not exist or is not a file/directory");
        }
    }

    debug!("Captured {file_count} files from topic paths");
    Ok(editor.write()?.into())
}
