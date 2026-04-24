use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;
use log::{debug, trace};
use mlua::{Lua, Value};

#[derive(Debug)]
pub struct Manifest {
    pub topics: Vec<Topic>,
}

pub struct Topic {
    pub name: String,
    pub lua: Lua,
    pub root: Option<Utf8PathBuf>,
    pub paths: Vec<Utf8PathBuf>,
    pub to_repo: Option<mlua::Function>,
    pub to_system: Option<mlua::Function>,
    pub should_include: Option<mlua::Function>,
}

impl std::fmt::Debug for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Topic")
            .field("name", &self.name)
            .field("root", &self.root)
            .field("paths", &self.paths)
            .field("has_to_repo", &self.to_repo.is_some())
            .field("has_to_system", &self.to_system.is_some())
            .field("has_should_include", &self.should_include.is_some())
            .field("lua", &"...")
            .finish()
    }
}

impl Topic {
    fn run_transform_hook(
        &self,
        hook_name: &str,
        func: &mlua::Function,
        path: &str,
        content: &[u8],
    ) -> Result<Vec<u8>> {
        let path_arg = self
            .lua
            .create_string(path)
            .with_context(|| format!("Failed to create path string for {hook_name}"))?;
        let content_arg = self
            .lua
            .create_string(content)
            .with_context(|| format!("Failed to create content string for {hook_name}"))?;
        let result: Value = func.call((path_arg, content_arg)).with_context(|| {
            format!(
                "{hook_name} hook for topic '{}' on '{path}' failed",
                self.name
            )
        })?;
        match result {
            Value::String(s) => Ok(s.as_bytes().to_vec()),
            other => bail!(
                "topic '{}': {hook_name} for '{path}' must return a string, got {}",
                self.name,
                other.type_name()
            ),
        }
    }

    /// Run the `to_repo` hook, returning transformed content.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-string value.
    pub fn to_repo(&self, path: &str, content: &[u8]) -> Result<Vec<u8>> {
        match &self.to_repo {
            Some(func) => self.run_transform_hook("to_repo", func, path, content),
            None => Ok(content.to_vec()),
        }
    }

    /// Run the `to_system` hook, returning transformed content.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-string value.
    pub fn to_system(&self, path: &str, content: &[u8]) -> Result<Vec<u8>> {
        match &self.to_system {
            Some(func) => self.run_transform_hook("to_system", func, path, content),
            None => Ok(content.to_vec()),
        }
    }

    /// Run the `should_include` hook, returning whether a path should be included.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-boolean value.
    pub fn should_include(&self, path: &str) -> Result<bool> {
        match &self.should_include {
            Some(func) => {
                let path_arg = self.lua.create_string(path).with_context(|| {
                    "Failed to create path string for should_include".to_string()
                })?;
                let result: Value = func.call(path_arg).with_context(|| {
                    format!(
                        "should_include hook for topic '{}' on '{path}' failed",
                        self.name
                    )
                })?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    other => bail!(
                        "topic '{}': should_include for '{path}' must return a boolean, got {}",
                        self.name,
                        other.type_name()
                    ),
                }
            }
            None => Ok(true),
        }
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

        let mut topics: Vec<Topic> = Vec::new();
        for (name, topic) in manifest_value
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("Manifest must be a table"))?
            .get::<std::collections::HashMap<String, std::collections::HashMap<String, Value>>>(
                "topics",
            )?
        {
            trace!("Loading topic '{name}'");
            let paths: Vec<String> = topic
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Topic must have a 'paths' field"))?
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("'paths' field must be a sequence"))?
                .sequence_values::<String>()
                .collect::<mlua::Result<_>>()?;
            let paths: Vec<Utf8PathBuf> = paths.into_iter().map(Utf8PathBuf::from).collect();

            trace!("Topic '{}' has {} paths", name, paths.len());

            let root = topic.get("root").and_then(|v| v.as_string()).and_then(|s| {
                let s = s.to_string_lossy();
                if s.is_empty() {
                    None
                } else {
                    Some(Utf8PathBuf::from(s))
                }
            });

            let to_repo = match topic.get("to_repo") {
                Some(Value::Function(f)) => Some(f.clone()),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'to_repo' must be a function, got {}",
                    v.type_name()
                ),
            };

            let to_system = match topic.get("to_system") {
                Some(Value::Function(f)) => Some(f.clone()),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'to_system' must be a function, got {}",
                    v.type_name()
                ),
            };

            let should_include = match topic.get("should_include") {
                Some(Value::Function(f)) => Some(f.clone()),
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'should_include' must be a function, got {}",
                    v.type_name()
                ),
            };

            topics.push(Topic {
                name: name.clone(),
                lua: lua.clone(),
                root,
                paths,
                to_repo,
                to_system,
                should_include,
            });
        }

        topics.sort_by(|a, b| a.name.cmp(&b.name));

        debug!("Manifest loaded with {} topics", topics.len());
        Ok(Manifest { topics })
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
        assert_eq!(manifest.topics[0].name, "topic1");
        assert_eq!(manifest.topics[0].paths.len(), 2);
        assert_eq!(manifest.topics[1].paths.len(), 1);
        assert_eq!(manifest.topics[2].paths.len(), 1);
        assert_eq!(manifest.topics[3].paths.len(), 2);
        assert_eq!(manifest.topics[4].paths.len(), 1);
        assert_eq!(
            manifest.topics[3].root,
            Some(Utf8PathBuf::from(".config/nvim"))
        );
        assert!(manifest.topics[0].root.is_none());
        assert!(manifest.topics[4].root.is_none()); // empty string normalized to None
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
        assert!(manifest.topics[0].to_repo.is_some());
        assert!(manifest.topics[0].to_system.is_some());
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
        assert!(manifest.topics[0].should_include.is_some());
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
    fn test_to_repo_transforms_content() {
        let lua = Lua::new();
        let func = lua
            .load(r#"function(path, content) return content:gsub("hello", "goodbye") end"#)
            .eval::<mlua::Function>()
            .unwrap();
        let topic = Topic {
            name: "test".to_string(),
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: Some(func.clone()),
            to_system: None,
            should_include: None,
        };
        let result = topic.to_repo("a.txt", b"hello world").unwrap();
        assert_eq!(result, b"goodbye world");
    }

    #[test]
    fn test_to_repo_rejects_non_string_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path, content) return 42 end")
            .eval::<mlua::Function>()
            .unwrap();
        let topic = Topic {
            name: "test".to_string(),
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: Some(func),
            to_system: None,
            should_include: None,
        };
        let result = topic.to_repo("a.txt", b"hello world");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a string"));
    }

    #[test]
    fn test_should_include_true() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return true end")
            .eval::<mlua::Function>()
            .unwrap();
        let topic = Topic {
            name: "test".to_string(),
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: None,
            to_system: None,
            should_include: Some(func),
        };
        assert!(topic.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_should_include_false() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return false end")
            .eval::<mlua::Function>()
            .unwrap();
        let topic = Topic {
            name: "test".to_string(),
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: None,
            to_system: None,
            should_include: Some(func),
        };
        assert!(!topic.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_should_include_rejects_non_boolean_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return 'yes' end")
            .eval::<mlua::Function>()
            .unwrap();
        let topic = Topic {
            name: "test".to_string(),
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: None,
            to_system: None,
            should_include: Some(func),
        };
        let result = topic.should_include("a.txt");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a boolean"));
    }
}
