use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;
use log::{debug, trace};
use mlua::{Lua, Value};

#[derive(Debug)]
pub struct Manifest {
    pub lua: Lua,
    pub topics: HashMap<String, Topic>,
}

pub struct Topic {
    pub root: Option<Utf8PathBuf>,
    pub paths: Vec<Utf8PathBuf>,
    pub to_repo: Option<mlua::RegistryKey>,
    pub to_system: Option<mlua::RegistryKey>,
    pub should_include: Option<mlua::RegistryKey>,
}

impl std::fmt::Debug for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Topic")
            .field("root", &self.root)
            .field("paths", &self.paths)
            .field("has_to_repo", &self.to_repo.is_some())
            .field("has_to_system", &self.to_system.is_some())
            .field("has_should_include", &self.should_include.is_some())
            .finish()
    }
}

impl Manifest {
    /// Loads a manifest from the given manifest content.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest is not a valid table or if any required fields are missing.
    pub fn load(manifest_content: &[u8]) -> Result<Self> {
        debug!("Parsing manifest ({:?} bytes)", manifest_content.len());
        let lua = Lua::new();
        let manifest_value = lua.load(manifest_content).eval::<Value>()?;

        let mut topics = HashMap::new();
        for (name, topic) in manifest_value
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("Manifest must be a table"))?
            .get::<HashMap<String, HashMap<String, Value>>>("topics")?
        {
            trace!("Loading topic '{name}'");
            let paths: Vec<String> = topic
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Topic must have a 'paths' field"))?
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("'paths' field must be a sequence"))?
                .sequence_values::<String>()
                .collect::<mlua::Result<_>>()?;
            let paths: Vec<Utf8PathBuf> = paths
                .into_iter()
                .map(Utf8PathBuf::from)
                .collect();

            trace!("Topic '{}' has {} paths", name, paths.len());

            let root = topic
                .get("root")
                .and_then(|v| v.as_string())
                .and_then(|s| {
                    let s = s.to_string_lossy();
                    if s.is_empty() { None } else { Some(Utf8PathBuf::from(s)) }
                });

            let to_repo = match topic.get("to_repo") {
                Some(Value::Function(f)) => Some(lua.create_registry_value(f.clone())?),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'to_repo' must be a function, got {}",
                    v.type_name()
                ),
            };

            let to_system = match topic.get("to_system") {
                Some(Value::Function(f)) => Some(lua.create_registry_value(f.clone())?),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'to_system' must be a function, got {}",
                    v.type_name()
                ),
            };

            let should_include = match topic.get("should_include") {
                Some(Value::Function(f)) => Some(lua.create_registry_value(f.clone())?),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'should_include' must be a function, got {}",
                    v.type_name()
                ),
            };

            topics.insert(name, Topic { root, paths, to_repo, to_system, should_include });
        }

        debug!("Manifest loaded with {} topics", topics.len());
        Ok(Manifest { lua, topics })
    }
}

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
    key: &mlua::RegistryKey,
    path: &str,
    content: &[u8],
) -> Result<Vec<u8>> {
    let func: mlua::Function = lua
        .registry_value(key)
        .with_context(|| format!("Failed to retrieve {hook_name} hook for topic '{topic_name}'"))?;
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
    key: &mlua::RegistryKey,
    path: &str,
) -> Result<bool> {
    let func: mlua::Function = lua
        .registry_value(key)
        .with_context(|| format!("Failed to retrieve should_include hook for topic '{topic_name}'"))?;
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
    fn test_load_manifest() {
        let manifest_content = r#"return {
            topics = {
                topic1 = {
                    paths = {"path/to/file1", "path/to/file2"}
                },
                topic2 = {
                    paths = {"path/to/file3"}
                },
                topic3 = {
                    paths = {"path/to/file4"}
                },
                topic4 = {
                    root = ".config/nvim",
                    paths = {"init.lua", "lua/plugins.lua"}
                },
                topic5 = {
                    root = "",
                    paths = {"bare_file"}
                }
            }
        }"#;
        let manifest =
            Manifest::load(manifest_content.as_bytes()).expect("Failed to load manifest");
        assert_eq!(manifest.topics.len(), 5);
        assert_eq!(manifest.topics["topic1"].paths.len(), 2);
        assert_eq!(manifest.topics["topic2"].paths.len(), 1);
        assert_eq!(manifest.topics["topic3"].paths.len(), 1);
        assert_eq!(manifest.topics["topic4"].paths.len(), 2);
        assert_eq!(manifest.topics["topic5"].paths.len(), 1);
        assert_eq!(
            manifest.topics["topic4"].root,
            Some(Utf8PathBuf::from(".config/nvim"))
        );
        assert!(manifest.topics["topic1"].root.is_none());
        assert!(manifest.topics["topic5"].root.is_none()); // empty string normalized to None
    }

    #[test]
    fn test_load_manifest_with_hooks() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {".gitconfig"},
                    to_repo = function(path, content)
                        return content
                    end,
                    to_system = function(path, content)
                        return content
                    end,
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert_eq!(manifest.topics.len(), 1);
        assert!(manifest.topics["git"].to_repo.is_some());
        assert!(manifest.topics["git"].to_system.is_some());
    }

    #[test]
    fn test_load_manifest_rejects_non_function_hook() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {".gitconfig"},
                    to_repo = "not a function",
                }
            }
        }"#;
        let result = Manifest::load(manifest_content.as_bytes());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("to_repo"));
    }

    #[test]
    fn test_run_hook_transforms_content() {
        let lua = Lua::new();
        let func = lua
            .load(r#"function(path, content) return content:gsub("hello", "goodbye") end"#)
            .eval::<mlua::Function>()
            .unwrap();
        let key = lua.create_registry_value(func).unwrap();
        let result = run_hook(&lua, "test", "to_repo", &key, "a.txt", b"hello world").unwrap();
        assert_eq!(result, b"goodbye world");
    }

    #[test]
    fn test_load_manifest_with_should_include() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {".gitconfig"},
                    should_include = function(path)
                        return path:sub(-4) ~= ".bak"
                    end,
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert_eq!(manifest.topics.len(), 1);
        assert!(manifest.topics["git"].should_include.is_some());
    }

    #[test]
    fn test_load_manifest_rejects_non_function_should_include() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {".gitconfig"},
                    should_include = true,
                }
            }
        }"#;
        let result = Manifest::load(manifest_content.as_bytes());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("should_include"));
    }

    #[test]
    fn test_run_hook_rejects_non_string_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path, content) return 42 end")
            .eval::<mlua::Function>()
            .unwrap();
        let key = lua.create_registry_value(func).unwrap();
        let result = run_hook(&lua, "test", "to_repo", &key, "a.txt", b"hello world");
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
        let key = lua.create_registry_value(func).unwrap();
        let result = run_should_include(&lua, "test", &key, "a.txt").unwrap();
        assert!(result);
    }

    #[test]
    fn test_run_should_include_false() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return false end")
            .eval::<mlua::Function>()
            .unwrap();
        let key = lua.create_registry_value(func).unwrap();
        let result = run_should_include(&lua, "test", &key, "a.txt").unwrap();
        assert!(!result);
    }

    #[test]
    fn test_run_should_include_rejects_non_boolean_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return 'yes' end")
            .eval::<mlua::Function>()
            .unwrap();
        let key = lua.create_registry_value(func).unwrap();
        let result = run_should_include(&lua, "test", &key, "a.txt");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a boolean"));
    }
}
