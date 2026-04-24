use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use gix::ObjectId;

use crate::store::PorchettaStore;
use super::path_util::to_tree_path;

/// A single captured file ready to be written into a git tree.
pub struct SnapshotEntry {
    pub relative_path: String,
    pub content: Vec<u8>,
}

/// Read files from disk and apply the `to_repo` transform.
///
/// This is a pure function: it does not touch the git store.
///
/// # Errors
///
/// Returns an error if a file cannot be read or the transform fails.
pub fn capture_files(
    topic_base: &Utf8Path,
    files: &[Utf8PathBuf],
    mut to_repo: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
) -> Result<Vec<SnapshotEntry>> {
    let mut entries = Vec::with_capacity(files.len());

    for file in files {
        let relative_path = to_tree_path(file.strip_prefix(topic_base)?);
        let content = std::fs::read(file)
            .with_context(|| format!("Failed to read file '{file}'"))?;
        let content = to_repo(&relative_path, &content)?;
        entries.push(SnapshotEntry {
            relative_path,
            content,
        });
    }

    Ok(entries)
}

/// Write snapshot entries into a new git tree.
///
/// # Errors
///
/// Returns an error if writing blobs or the tree fails.
pub fn write_snapshot(
    store: &PorchettaStore,
    entries: &[SnapshotEntry],
) -> Result<ObjectId> {
    let mut editor = store.edit_tree(store.empty_tree_id())?;
    for entry in entries {
        let blob_oid = store.write_blob(&entry.content)?;
        editor.upsert(
            &entry.relative_path,
            gix::objs::tree::EntryKind::Blob,
            blob_oid,
        )?;
    }
    Ok(editor.write()?.into())
}
