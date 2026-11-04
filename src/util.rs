use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;

/// Returns the user's home directory as a UTF-8 path.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or is not valid UTF-8.
pub fn home_dir() -> Result<Utf8PathBuf> {
    Utf8PathBuf::try_from(dirs::home_dir().context("Could not determine home directory")?)
        .map_err(|e| anyhow::anyhow!("home directory is not valid UTF-8: {e}"))
}

/// Returns the user's preferred editor, checking `$EDITOR`, `$VISUAL`, and
/// common fallbacks in order.
///
/// # Errors
///
/// Returns an error if none of the known editors can be found in `PATH`.
pub fn get_editor() -> Result<String> {
    if let Ok(editor) = std::env::var("EDITOR") {
        return Ok(editor);
    }
    if let Ok(editor) = std::env::var("VISUAL") {
        return Ok(editor);
    }
    for editor in ["nvim", "nano", "vim", "vi"] {
        if is_in_path(editor) {
            return Ok(editor.to_string());
        }
    }
    bail!(
        "No suitable editor found. Please set the $EDITOR environment variable (e.g. export EDITOR=nvim)."
    )
}

fn is_in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Computes the relative path from `from_dir` to `to_path`.
///
/// Both paths should share a common base (e.g. both under home).
/// Returns a relative path suitable for a symlink placed in `from_dir` pointing to `to_path`.
#[must_use]
pub fn relative_path_between(
    from_dir: &camino::Utf8Path,
    to_path: &camino::Utf8Path,
) -> Utf8PathBuf {
    let from_components: Vec<_> = from_dir.components().collect();
    let to_components: Vec<_> = to_path.components().collect();

    let mut common_len = 0;
    while common_len < from_components.len()
        && common_len < to_components.len()
        && from_components[common_len] == to_components[common_len]
    {
        common_len += 1;
    }

    let mut result = Utf8PathBuf::new();
    for _ in common_len..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common_len..] {
        result.push(component.as_str());
    }
    if result.as_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        editor: Option<std::ffi::OsString>,
        visual: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                _lock: ENV_LOCK.lock().unwrap(),
                editor: std::env::var_os("EDITOR"),
                visual: std::env::var_os("VISUAL"),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.editor {
                    Some(v) => std::env::set_var("EDITOR", v),
                    None => std::env::remove_var("EDITOR"),
                }
                match &self.visual {
                    Some(v) => std::env::set_var("VISUAL", v),
                    None => std::env::remove_var("VISUAL"),
                }
            }
        }
    }

    #[test]
    fn test_get_editor_prefers_editor() {
        let _env = EnvGuard::new();

        unsafe {
            std::env::set_var("EDITOR", "my-custom-editor");
            std::env::remove_var("VISUAL");
        }

        assert_eq!(get_editor().unwrap(), "my-custom-editor");
    }

    #[test]
    fn test_get_editor_falls_back_to_visual() {
        let _env = EnvGuard::new();

        unsafe {
            std::env::remove_var("EDITOR");
            std::env::set_var("VISUAL", "my-visual-editor");
        }

        assert_eq!(get_editor().unwrap(), "my-visual-editor");
    }

    #[test]
    fn test_relative_path_between() {
        use camino::Utf8Path;

        // Sibling directories
        assert_eq!(
            relative_path_between(
                Utf8Path::new("/home/user/.gemini"),
                Utf8Path::new("/home/user/.pi/agent/AGENTS.md")
            ),
            Utf8Path::new("../.pi/agent/AGENTS.md")
        );

        // Same directory
        assert_eq!(
            relative_path_between(
                Utf8Path::new("/home/user/.gemini"),
                Utf8Path::new("/home/user/.gemini/skills")
            ),
            Utf8Path::new("skills")
        );

        // Nested link directory
        assert_eq!(
            relative_path_between(
                Utf8Path::new("/home/user/.gemini/sub/dir"),
                Utf8Path::new("/home/user/.pi/agent/skills")
            ),
            Utf8Path::new("../../../.pi/agent/skills")
        );

        // Target at root of base
        assert_eq!(
            relative_path_between(
                Utf8Path::new("/home/user/.gemini"),
                Utf8Path::new("/home/user/root_file")
            ),
            Utf8Path::new("../root_file")
        );
    }
}
