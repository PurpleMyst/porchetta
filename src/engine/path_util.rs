use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use gix::bstr::ByteSlice;

/// Convert a platform path to a forward-slash string for git tree storage.
#[must_use]
pub fn to_tree_path(path: &Utf8Path) -> String {
    path.as_str().replace('\\', "/")
}

/// Convert a forward-slash path from a git tree to a platform `Utf8PathBuf`.
#[must_use]
pub fn from_tree_path(path: &str) -> Utf8PathBuf {
    Utf8PathBuf::from(path)
}

/// Convert a `gix` diff location to a `Utf8PathBuf`.
///
/// # Errors
///
/// Returns an error if the location is not valid UTF-8.
pub fn diff_location_to_path(location: &gix::bstr::BStr) -> Result<Utf8PathBuf> {
    let relative_path = std::str::from_utf8(location.as_ref()).with_context(|| {
        format!("Diff path '{}' is not valid UTF-8", location.to_str_lossy())
    })?;
    Ok(from_tree_path(relative_path))
}
