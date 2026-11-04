use anyhow::Result;

/// Resolution strategy for a tree-level merge conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeConflictResolution {
    /// Keep the local (ours) side.
    KeepOurs,
    /// Keep the remote (theirs) side.
    KeepTheirs,
    /// Abort the sync operation.
    Abort,
}

/// Resolution strategy for a symlink conflict with an existing file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkConflictResolution {
    /// Replace the existing file/directory with the symlink.
    Replace,
    /// Keep the existing file/directory (skip creating the symlink).
    KeepExisting,
    /// Abort the sync operation.
    Abort,
}

/// Abstraction over interactive conflict resolution.
///
/// Implementations may prompt the user via TUI, log and pick a default,
/// or panic when called in non-interactive contexts such as tests.
pub trait ConflictResolver {
    /// Prompt for a tree-level conflict decision.
    ///
    /// # Errors
    ///
    /// Returns an error if the user cancels or the prompt fails.
    fn resolve_tree_conflict(&self, prompt: &str) -> Result<TreeConflictResolution>;

    /// Prompt for a symlink conflict decision when an existing file or directory
    /// is at the symlink's path.
    ///
    /// # Errors
    ///
    /// Returns an error if the user cancels or the prompt fails.
    fn resolve_symlink_conflict(&self, prompt: &str) -> Result<SymlinkConflictResolution>;

    /// Prompt for which entry kind to use when local and remote disagree.
    ///
    /// # Errors
    ///
    /// Returns an error if the user cancels or the prompt fails.
    fn choose_entry_kind(
        &self,
        ours: gix::objs::tree::EntryKind,
        theirs: gix::objs::tree::EntryKind,
    ) -> Result<gix::objs::tree::EntryKind>;

    /// Open an editor (or equivalent) to modify merged blob content.
    ///
    /// # Errors
    ///
    /// Returns an error if editing fails or the user cancels.
    fn edit_blob(&self, content: &[u8], path: &str) -> Result<Vec<u8>>;
}

/// A resolver that panics if any conflict-resolution method is called.
///
/// Useful for tests that set up scenarios guaranteed not to produce conflicts.
pub struct PanickingResolver;

impl ConflictResolver for PanickingResolver {
    fn resolve_tree_conflict(&self, _prompt: &str) -> Result<TreeConflictResolution> {
        panic!("PanickingResolver: unexpected tree conflict in test");
    }

    fn resolve_symlink_conflict(&self, _prompt: &str) -> Result<SymlinkConflictResolution> {
        panic!("PanickingResolver: unexpected symlink conflict in test");
    }

    fn choose_entry_kind(
        &self,
        _ours: gix::objs::tree::EntryKind,
        _theirs: gix::objs::tree::EntryKind,
    ) -> Result<gix::objs::tree::EntryKind> {
        panic!("PanickingResolver: unexpected entry-kind conflict in test");
    }

    fn edit_blob(&self, _content: &[u8], _path: &str) -> Result<Vec<u8>> {
        panic!("PanickingResolver: unexpected blob conflict in test");
    }
}
