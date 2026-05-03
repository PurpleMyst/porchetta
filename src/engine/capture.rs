use anyhow::{Context, Result};
use camino::Utf8Path;
use gix::ObjectId;

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

/// Read files from disk, apply the `to_repo` transform, and write them into a new git tree.
///
/// # Errors
///
/// Returns an error if a file cannot be read, the transform fails, or writing to the store fails.
pub fn snapshot_topic<'a>(
    store: &PorchettaStore,
    topic_base: &Utf8Path,
    files: impl IntoIterator<Item = &'a Utf8Path>,
    mut to_repo: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
) -> Result<ObjectId> {
    let mut editor = store.edit_tree(store.empty_tree_id())?;

    for file in files {
        let relative_path = to_tree_path(file.strip_prefix(topic_base)?);
        let content =
            std::fs::read(file).with_context(|| format!("Failed to read file '{file}'"))?;
        let content = normalize_line_endings(to_repo(&relative_path, &content)?);
        let blob_oid = store.write_blob(&content)?;
        editor.upsert(&relative_path, gix::objs::tree::EntryKind::Blob, blob_oid)?;
    }

    Ok(editor.write()?.into())
}
