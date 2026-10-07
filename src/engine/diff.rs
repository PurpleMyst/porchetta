use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use camino::Utf8PathBuf;
use gix::error::ErrorExt as _;
use gix::{ObjectId, object::tree::diff::Change};

use super::path_util::diff_location_to_path;

/// Convert an error into the error type expected by `gix` diff callbacks.
fn to_exn(error: anyhow::Error) -> gix::Exn {
    gix::Error::from_boxed(error.into()).raise_erased()
}

pub enum ApplyOperation {
    Upsert {
        relative_path: Utf8PathBuf,
        blob_oid: ObjectId,
    },
    Delete {
        relative_path: Utf8PathBuf,
    },
}

/// Compare `our_tree` with `merged_tree` and return the operations needed
/// to bring the filesystem from `our_tree` to `merged_tree`.
///
/// # Errors
///
/// Returns an error if reading trees or diffing fails.
pub fn collect_apply_operations(
    our_tree: &gix::Tree<'_>,
    merged_tree: &gix::Tree<'_>,
) -> Result<Vec<ApplyOperation>> {
    let mut operations = Vec::new();
    let mut changes = our_tree.changes()?;
    changes.options(|options| {
        options.track_path();
        options.track_rewrites(None);
    });

    changes.for_each_to_obtain_tree(merged_tree, |change| {
        match change {
            Change::Addition {
                location,
                entry_mode,
                id,
                ..
            }
            | Change::Modification {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    return Ok(ControlFlow::Continue(()));
                }

                operations.push(ApplyOperation::Upsert {
                    relative_path: diff_location_to_path(location).map_err(to_exn)?,
                    blob_oid: id.detach(),
                });
            }
            Change::Deletion {
                location,
                entry_mode,
                ..
            } => {
                if entry_mode.is_tree() {
                    return Ok(ControlFlow::Continue(()));
                }

                operations.push(ApplyOperation::Delete {
                    relative_path: diff_location_to_path(location).map_err(to_exn)?,
                });
            }
            Change::Rewrite { .. } => {
                return Err(to_exn(anyhow!(
                    "Rewrite operation encountered despite rewrite tracking being disabled"
                )));
            }
        }

        Ok(ControlFlow::Continue(()))
    })?;

    Ok(operations)
}

/// A file-level change for reporting, relative to a topic root.
pub struct FileChange {
    pub action: &'static str,
    pub relative_path: Utf8PathBuf,
}

/// Describe the file changes between two trees, without applying them.
///
/// # Errors
///
/// Returns an error if reading trees or diffing fails.
pub fn collect_file_changes(
    before: &gix::Tree<'_>,
    after: &gix::Tree<'_>,
) -> Result<Vec<FileChange>> {
    collect_apply_operations(before, after)?
        .into_iter()
        .map(|operation| {
            let (action, relative_path) = match operation {
                ApplyOperation::Upsert { relative_path, .. } => {
                    let action = if before.lookup_entry_by_path(&relative_path)?.is_some() {
                        "update"
                    } else {
                        "add"
                    };
                    (action, relative_path)
                }
                ApplyOperation::Delete { relative_path } => ("delete", relative_path),
            };
            Ok(FileChange {
                action,
                relative_path,
            })
        })
        .collect()
}
