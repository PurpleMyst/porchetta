use anyhow::{Result, bail};

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
}
