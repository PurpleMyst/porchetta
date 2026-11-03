use std::ops::ControlFlow;

use anyhow::{Context, Result, anyhow};
use camino::Utf8PathBuf;
use gix::bstr::ByteSlice;
use gix::error::ErrorExt as _;
use gix::{ObjectId, object::tree::diff::Change};

fn to_exn(error: anyhow::Error) -> gix::Exn {
    gix::Error::from_boxed(error.into()).raise_erased()
}

pub enum ChangeKind {
    Add(ObjectId),
    Update(ObjectId),
    Delete,
}

/// A file-level change between two trees, relative to a topic root.
pub struct FileChange {
    pub relative_path: Utf8PathBuf,
    pub kind: ChangeKind,
}

impl FileChange {
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self.kind {
            ChangeKind::Add(_) => "add",
            ChangeKind::Update(_) => "update",
            ChangeKind::Delete => "delete",
        }
    }
}

/// Compare `before` with `after` and return the file-level changes needed
/// to transform `before` into `after`.
pub fn collect_file_changes(
    before: &gix::Tree<'_>,
    after: &gix::Tree<'_>,
) -> Result<Vec<FileChange>> {
    let mut operations = Vec::new();
    let mut changes = before.changes()?;
    changes.options(|options| {
        options.track_path();
        options.track_rewrites(None);
    });

    changes.for_each_to_obtain_tree(after, |change| {
        if change.entry_mode().is_tree() {
            return Ok(ControlFlow::Continue(()));
        }
        let relative_path = std::str::from_utf8(change.location().as_ref())
            .with_context(|| {
                format!(
                    "Diff path '{}' is not valid UTF-8",
                    change.location().to_str_lossy()
                )
            })
            .map(Utf8PathBuf::from)
            .map_err(to_exn)?;
        let kind = match change {
            Change::Addition { id, .. } => ChangeKind::Add(id.detach()),
            Change::Modification { id, .. } => ChangeKind::Update(id.detach()),
            Change::Deletion { .. } => ChangeKind::Delete,
            Change::Rewrite { .. } => {
                return Err(to_exn(anyhow!(
                    "Rewrite operation encountered despite rewrite tracking being disabled"
                )));
            }
        };
        operations.push(FileChange {
            relative_path,
            kind,
        });
        Ok(ControlFlow::Continue(()))
    })?;

    Ok(operations)
}
