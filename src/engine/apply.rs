use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use gix::ObjectId;

use super::diff::ApplyOperation;

/// Validate that `operations` can be applied to the filesystem under `topic_base`.
fn validate_operations(
    topic_base: &Utf8Path,
    topic_name: &str,
    operations: &[ApplyOperation],
) -> Result<()> {
    for operation in operations {
        match operation {
            ApplyOperation::Upsert { relative_path, .. } => {
                let abs_path = topic_base.join(relative_path);

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
            ApplyOperation::Delete { relative_path } => {
                let abs_path = topic_base.join(relative_path);
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

/// Apply `operations` to the filesystem under `topic_base`.
///
/// `to_system` is called for every upsert to transform content before writing.
/// `read_blob` is called to resolve blob OIDs into bytes.
///
/// # Errors
///
/// Returns an error if any filesystem operation fails.
pub fn apply(
    topic_base: &Utf8Path,
    topic_name: &str,
    operations: Vec<ApplyOperation>,
    mut to_system: impl FnMut(&str, &[u8]) -> Result<Vec<u8>>,
    mut read_blob: impl FnMut(ObjectId) -> Result<Vec<u8>>,
) -> Result<()> {
    validate_operations(topic_base, topic_name, &operations)?;

    for operation in operations {
        match operation {
            ApplyOperation::Upsert {
                relative_path,
                blob_oid,
            } => {
                let abs_path = topic_base.join(&relative_path);

                if let Some(parent) = abs_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "Failed to create parent directory '{parent}' for topic '{topic_name}'"
                        )
                    })?;
                }

                let content = read_blob(blob_oid).with_context(|| {
                    format!("Failed to read blob '{blob_oid}' for topic '{topic_name}'")
                })?;

                let content = to_system(relative_path.as_str(), &content)?;
                std::fs::write(&abs_path, content).with_context(|| {
                    format!("Failed to write '{abs_path}' for topic '{topic_name}'")
                })?;
            }
            ApplyOperation::Delete { relative_path } => {
                let abs_path = topic_base.join(relative_path);
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
