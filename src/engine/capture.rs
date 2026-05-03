use std::collections::VecDeque;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use gix::ObjectId;
use log::{debug, trace};

use super::path_util::to_tree_path;
use crate::store::PorchettaStore;

/// Normalize line endings in the content to LF. If the content is not valid UTF-8, it is returned as-is.
fn normalize_line_endings(content: Vec<u8>) -> Vec<u8> {
    // XXX: Heuristic-y but in my own usage this works; we can add a topic-level is_binary callback
    // later if the need arises. ¯\_(ツ)_/¯
    let Some(content_str) = std::str::from_utf8(&content).ok() else {
        // If the content is not valid UTF-8, return it as-is.
        return content;
    };
    // Normalize line endings to LF.
    content_str.replace("\r\n", "\n").into_bytes()
}

/// Walk the topic paths, read files from disk, apply the `to_repo` transform, and write them into a new git tree.
///
/// The `should_include` closure is called with the tree-relative path
/// (forward-slash separated) and should return `true` if the file or
/// directory should be included.
///
/// # Errors
///
/// Returns an error if the include hook fails, a file cannot be read,
/// the transform fails, or writing to the store fails.
pub fn snapshot_topic(
    store: &PorchettaStore,
    topic_base: &Utf8Path,
    paths: &[Utf8PathBuf],
    mut should_include: impl FnMut(&str) -> Result<bool>,
    mut to_repo: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
) -> Result<ObjectId> {
    let mut editor = store.edit_tree(store.empty_tree_id())?;
    let mut file_count = 0;

    for p in paths {
        let abs_path = topic_base.join(p);
        if abs_path.is_file() {
            if snapshot_file(
                store,
                topic_base,
                &abs_path,
                &mut should_include,
                &mut to_repo,
                &mut editor,
            )? {
                file_count += 1;
            }
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
                    if snapshot_file(
                        store,
                        topic_base,
                        &p2,
                        &mut should_include,
                        &mut to_repo,
                        &mut editor,
                    )? {
                        file_count += 1;
                    }
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
            debug!("Skipping missing path '{abs_path}'");
        }
    }

    debug!("Captured {file_count} files from topic paths");
    Ok(editor.write()?.into())
}

fn snapshot_file(
    store: &PorchettaStore,
    topic_base: &Utf8Path,
    file: &Utf8Path,
    should_include: &mut impl FnMut(&str) -> Result<bool>,
    to_repo: &mut impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
    editor: &mut gix::object::tree::Editor<'_>,
) -> Result<bool> {
    let relative_path = to_tree_path(file.strip_prefix(topic_base)?);
    if !should_include(&relative_path)? {
        debug!("Excluding file '{file}' based on should_include hook");
        return Ok(false);
    }
    trace!("Found file: {file}");
    let content = std::fs::read(file).with_context(|| format!("Failed to read file '{file}'"))?;
    let content = normalize_line_endings(to_repo(&relative_path, &content)?);
    let blob_oid = store.write_blob(&content)?;
    editor.upsert(&relative_path, gix::objs::tree::EntryKind::Blob, blob_oid)?;
    Ok(true)
}
