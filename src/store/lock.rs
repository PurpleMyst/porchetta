use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

#[derive(Debug)]
pub(super) struct StoreLock {
    _file: std::fs::File,
}

impl StoreLock {
    pub(super) fn acquire(path: &Utf8Path) -> Result<Self> {
        let store_path = if path.exists() {
            path.canonicalize_utf8()
                .with_context(|| format!("Failed to canonicalize Porchetta store path '{path}'"))?
        } else {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_str().is_empty())
                .unwrap_or(Utf8Path::new("."))
                .canonicalize_utf8()
                .with_context(|| {
                    format!("Failed to canonicalize parent of Porchetta store path '{path}'")
                })?;
            let file_name = path
                .file_name()
                .context("Porchetta store path has no file name")?;
            parent.join(file_name)
        };
        let lock_path = Utf8PathBuf::from(format!("{store_path}.lock"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("Failed to open Porchetta store lock '{lock_path}'"))?;

        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("Porchetta store is already in use: {store_path}");
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("Failed to lock Porchetta store at '{store_path}'")),
        }
    }
}
