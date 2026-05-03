use std::fmt;
use std::ops::ControlFlow;

use anyhow::{Result, bail};
use camino::Utf8PathBuf;
use gix::ObjectId;

use super::path_util::diff_location_to_path;

pub enum ApplyOperation {
    Upsert {
        relative_path: Utf8PathBuf,
        blob_oid: ObjectId,
    },
    Delete {
        relative_path: Utf8PathBuf,
    },
}

impl fmt::Display for ApplyOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upsert { relative_path, .. } => {
                write!(f, "upsert {relative_path}")
            }
            Self::Delete { relative_path } => {
                write!(f, "delete {relative_path}")
            }
        }
    }
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
            gix::object::tree::diff::Change::Addition {
                location,
                entry_mode,
                id,
                ..
            }
            | gix::object::tree::diff::Change::Modification {
                location,
                entry_mode,
                id,
                ..
            } => {
                if entry_mode.is_tree() {
                    return Ok(ControlFlow::Continue(()));
                }

                operations.push(ApplyOperation::Upsert {
                    relative_path: diff_location_to_path(location)?,
                    blob_oid: id.detach(),
                });
            }
            gix::object::tree::diff::Change::Deletion {
                location,
                entry_mode,
                ..
            } => {
                if entry_mode.is_tree() {
                    return Ok(ControlFlow::Continue(()));
                }

                operations.push(ApplyOperation::Delete {
                    relative_path: diff_location_to_path(location)?,
                });
            }
            gix::object::tree::diff::Change::Rewrite { .. } => {
                bail!("Rewrite operation encountered despite rewrite tracking being disabled");
            }
        }

        Ok(ControlFlow::Continue(()))
    })?;

    Ok(operations)
}
