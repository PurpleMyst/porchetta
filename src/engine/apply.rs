use anyhow::{Context, Result, bail};
use camino::Utf8Path;

use super::diff::{ChangeKind, FileChange};
use crate::manifest::Topic;

/// Validate that `changes` can be applied to the filesystem under `topic_base`.
fn validate_operations(
    topic_base: &Utf8Path,
    topic_name: &str,
    changes: &[FileChange],
) -> Result<()> {
    for change in changes {
        let abs_path = topic_base.join(&change.relative_path);
        match change.kind {
            ChangeKind::Add(_) | ChangeKind::Update(_) => {
                if abs_path.is_dir() {
                    bail!(
                        "Topic '{topic_name}' cannot write file '{abs_path}' because it is a directory"
                    );
                }
                if let Some(parent) = abs_path.parent()
                    && parent.is_file()
                {
                    bail!(
                        "Topic '{topic_name}' cannot create '{abs_path}' because parent '{parent}' is a file"
                    );
                }
            }
            ChangeKind::Delete => {
                if abs_path.is_dir() {
                    bail!(
                        "Topic '{topic_name}' cannot delete '{abs_path}' as a file because it is a directory"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Apply `changes` for `topic` to the filesystem under `topic_base`.
///
/// # Errors
///
/// Returns an error if any filesystem operation fails.
pub fn apply(
    repo: &gix::Repository,
    topic: &Topic,
    topic_base: &Utf8Path,
    changes: &[FileChange],
) -> Result<()> {
    let topic_name = &topic.name;
    validate_operations(topic_base, topic_name, changes)?;

    for change in changes {
        let abs_path = topic_base.join(&change.relative_path);
        match change.kind {
            ChangeKind::Add(blob_oid) | ChangeKind::Update(blob_oid) => {
                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "Failed to create parent directory '{parent}' for topic '{topic_name}'"
                        )
                    })?;
                }
                let mut blob = repo.find_blob(blob_oid).with_context(|| {
                    format!("Failed to read blob '{blob_oid}' for topic '{topic_name}'")
                })?;
                let content = topic.to_system(change.relative_path.as_str(), blob.take_data())?;
                std::fs::write(&abs_path, content).with_context(|| {
                    format!("Failed to write '{abs_path}' for topic '{topic_name}'")
                })?;
            }
            ChangeKind::Delete => {
                if abs_path.exists() {
                    std::fs::remove_file(&abs_path).with_context(|| {
                        format!("Failed to delete '{abs_path}' for topic '{topic_name}'")
                    })?;
                }
            }
        }
    }

    Ok(())
}
