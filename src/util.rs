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
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_editor_prefers_editor() {
        // Temporarily override environment variables.
        let old_editor = std::env::var_os("EDITOR");
        let old_visual = std::env::var_os("VISUAL");

        unsafe {
            std::env::set_var("EDITOR", "my-custom-editor");
            std::env::remove_var("VISUAL");
        }

        assert_eq!(get_editor().unwrap(), "my-custom-editor");

        // Restore.
        unsafe {
            match old_editor {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
            match old_visual {
                Some(v) => std::env::set_var("VISUAL", v),
                None => std::env::remove_var("VISUAL"),
            }
        }
    }

    #[test]
    fn test_get_editor_falls_back_to_visual() {
        let old_editor = std::env::var_os("EDITOR");
        let old_visual = std::env::var_os("VISUAL");

        unsafe {
            std::env::remove_var("EDITOR");
            std::env::set_var("VISUAL", "my-visual-editor");
        }

        assert_eq!(get_editor().unwrap(), "my-visual-editor");

        unsafe {
            match old_editor {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
            match old_visual {
                Some(v) => std::env::set_var("VISUAL", v),
                None => std::env::remove_var("VISUAL"),
            }
        }
    }
}
