use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use gix::ObjectId;

use crate::store::PorchettaStore;
use super::path_util::to_tree_path;

/// Read files from disk, apply the `to_repo` transform, and write them into a new git tree.
///
/// # Errors
///
/// Returns an error if a file cannot be read, the transform fails, or writing to the store fails.
pub fn snapshot_topic(
    store: &PorchettaStore,
    topic_base: &Utf8Path,
    files: &[Utf8PathBuf],
    mut to_repo: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
) -> Result<ObjectId> {
    let mut editor = store.edit_tree(store.empty_tree_id())?;

    for file in files {
        let relative_path = to_tree_path(file.strip_prefix(topic_base)?);
        let content = std::fs::read(file)
            .with_context(|| format!("Failed to read file '{file}'"))?;
        let content = to_repo(&relative_path, &content)?;
        let blob_oid = store.write_blob(&content)?;
        editor.upsert(
            &relative_path,
            gix::objs::tree::EntryKind::Blob,
            blob_oid,
        )?;
    }

    Ok(editor.write()?.into())
}
