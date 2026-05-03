use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use gix::bstr::ByteSlice;

/// Convert a platform path to a forward-slash string for git tree storage.
#[must_use]
pub fn to_tree_path(path: &Utf8Path) -> String {
    path.as_str().replace('\\', "/")
}

/// Convert a `gix` diff location to a `Utf8PathBuf`.
///
/// # Errors
///
/// Returns an error if the location is not valid UTF-8.
pub fn diff_location_to_path(location: &gix::bstr::BStr) -> Result<Utf8PathBuf> {
    let relative_path = std::str::from_utf8(location.as_ref())
        .with_context(|| format!("Diff path '{}' is not valid UTF-8", location.to_str_lossy()))?;
    Ok(Utf8PathBuf::from(relative_path))
}
