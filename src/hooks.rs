use anyhow::{Context, Result, bail};
use mlua::{Lua, Value};

/// Run a topic rewrite hook.
///
/// # Errors
///
/// Returns an error if the hook cannot be retrieved from the registry, if the
/// call fails, or if the hook returns a non-string value.
pub fn run_hook(
    lua: &Lua,
    topic_name: &str,
    hook_name: &str,
    func: &mlua::Function,
    path: &str,
    content: &[u8],
) -> Result<Vec<u8>> {
    let path_arg = lua
        .create_string(path)
        .with_context(|| format!("Failed to create path string for {hook_name}"))?;
    let content_arg = lua
        .create_string(content)
        .with_context(|| format!("Failed to create content string for {hook_name}"))?;
    let result: Value = func
        .call((path_arg, content_arg))
        .with_context(|| format!("{hook_name} hook for topic '{topic_name}' on '{path}' failed"))?;
    match result {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        other => bail!(
            "topic '{topic_name}': {hook_name} for '{path}' must return a string, got {}",
            other.type_name()
        ),
    }
}

/// Run a topic `should_include` hook.
///
/// # Errors
///
/// Returns an error if the hook cannot be retrieved from the registry, if the
/// call fails, or if the hook returns a non-boolean value.
pub fn run_should_include(
    lua: &Lua,
    topic_name: &str,
    func: &mlua::Function,
    path: &str,
) -> Result<bool> {
    let path_arg = lua
        .create_string(path)
        .with_context(|| "Failed to create path string for should_include".to_string())?;
    let result: Value = func
        .call(path_arg)
        .with_context(|| format!("should_include hook for topic '{topic_name}' on '{path}' failed"))?;
    match result {
        Value::Boolean(b) => Ok(b),
        other => bail!(
            "topic '{topic_name}': should_include for '{path}' must return a boolean, got {}",
            other.type_name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_hook_transforms_content() {
        let lua = Lua::new();
        let func = lua
            .load(r#"function(path, content) return content:gsub("hello", "goodbye") end"#)
            .eval::<mlua::Function>()
            .unwrap();
        let result = run_hook(&lua, "test", "to_repo", &func, "a.txt", b"hello world").unwrap();
        assert_eq!(result, b"goodbye world");
    }

    #[test]
    fn test_run_hook_rejects_non_string_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path, content) return 42 end")
            .eval::<mlua::Function>()
            .unwrap();
        let result = run_hook(&lua, "test", "to_repo", &func, "a.txt", b"hello world");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a string"));
    }

    #[test]
    fn test_run_should_include_true() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return true end")
            .eval::<mlua::Function>()
            .unwrap();
        let result = run_should_include(&lua, "test", &func, "a.txt").unwrap();
        assert!(result);
    }

    #[test]
    fn test_run_should_include_false() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return false end")
            .eval::<mlua::Function>()
            .unwrap();
        let result = run_should_include(&lua, "test", &func, "a.txt").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_run_should_include_rejects_non_boolean_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return 'yes' end")
            .eval::<mlua::Function>()
            .unwrap();
        let result = run_should_include(&lua, "test", &func, "a.txt");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a boolean"));
    }
}
