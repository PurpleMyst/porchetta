use std::collections::HashMap;

use anyhow::{Result, bail};
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
    pub to_repo: Option<mlua::Function>,
    pub to_system: Option<mlua::Function>,
    pub should_include: Option<mlua::Function>,
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

            topics.insert(name, Topic { root, paths, to_repo, to_system, should_include });
        }

        debug!("Manifest loaded with {} topics", topics.len());
        Ok(Manifest { lua, topics })
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

}
