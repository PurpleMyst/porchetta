use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use log::{debug, trace};
use mlua::{Lua, Value};

pub struct Manifest {
    pub lua: Lua,
    pub topics: Vec<Topic>,
    pub should_include: Option<mlua::Function>,
}

impl std::fmt::Debug for Manifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manifest")
            .field("topics", &self.topics)
            .field("has_should_include", &self.should_include.is_some())
            .field("lua", &"...")
            .finish()
    }
}

pub struct Topic {
    pub name: String,
    pub enabled: bool,
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
            .field("enabled", &self.enabled)
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

    fn validate(&self) -> Result<()> {
        if let Some(root) = &self.root {
            ensure_relative_path(root, &format!("Topic '{}' root", self.name), PathKind::Root)?;
        }

        for path in &self.paths {
            ensure_relative_path(path, &format!("Topic '{}' path", self.name), PathKind::Path)?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PathKind {
    Root,
    Path,
}

fn ensure_relative_path(path: &Utf8Path, label: &str, kind: PathKind) -> Result<()> {
    ensure!(
        !path.is_absolute() && !path.has_root(),
        "{label} '{path}' must be relative"
    );
    ensure!(
        !path.components().any(|c| c == Utf8Component::ParentDir),
        "{label} '{path}' contains '..' which is not allowed"
    );
    if matches!(kind, PathKind::Root) {
        ensure!(path != Utf8Path::new("."), "{label} may not be '.'");
    }

    Ok(())
}

/// If `root` is an absolute/rooted path under `home`, normalize it to relative.
fn normalize_root_under_home(root: &mut Option<Utf8PathBuf>, home: Option<&Utf8Path>) {
    if let Some(home_dir) = home
        && let Some(root_path) = root
        && (root_path.is_absolute() || root_path.has_root())
        && let Ok(rel) = root_path.strip_prefix(home_dir)
    {
        let rel = Utf8PathBuf::from(rel);
        if rel.as_str().is_empty() {
            // Root equals home → no explicit root needed
            *root = None;
        } else {
            *root = Some(rel);
        }
    }
}

impl Manifest {
    /// Run the manifest-level `should_include` hook, returning whether a path should be included.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-boolean value.
    pub fn should_include(&self, path: &str) -> Result<bool> {
        match &self.should_include {
            Some(func) => {
                let path_arg = self.lua.create_string(path).with_context(|| {
                    "Failed to create path string for manifest should_include".to_string()
                })?;
                let result: Value = func.call(path_arg).with_context(|| {
                    format!("manifest-level should_include for '{path}' failed")
                })?;
                match result {
                    Value::Boolean(b) => Ok(b),
                    other => bail!(
                        "manifest-level should_include for '{path}' must return a boolean, got {}",
                        other.type_name()
                    ),
                }
            }
            None => Ok(true),
        }
    }

    /// Loads a manifest from the given manifest content.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest is not a valid table or if any required fields are missing.
    pub fn load(manifest_content: &[u8]) -> Result<Self> {
        Self::load_with_home(manifest_content, None)
    }

    /// Loads a manifest with an optional home directory for normalizing absolute roots.
    ///
    /// When `home` is provided, any absolute `root` path that falls under the home directory
    /// is normalized to a relative path. Absolute roots outside the home directory are still
    /// rejected during validation. Individual topic `paths` are never normalized — they must
    /// always be relative.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest is not a valid table or if any required fields are missing.
    #[allow(clippy::too_many_lines)]
    pub fn load_with_home(manifest_content: &[u8], home: Option<&Utf8Path>) -> Result<Self> {
        debug!("Parsing manifest ({:?} bytes)", manifest_content.len());
        let lua = Lua::new();
        let porchetta_tbl = lua.create_table()?;
        porchetta_tbl.set("system", lua.create_function(crate::lua_runtime::system)?)?;
        porchetta_tbl.set(
            "hostname",
            lua.create_function(crate::lua_runtime::hostname)?,
        )?;
        lua.globals().set("porchetta", porchetta_tbl)?;

        let manifest_value = lua.load(manifest_content).eval::<Value>()?;
        let manifest_table = manifest_value
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("Manifest must be a table"))?;

        let should_include = match manifest_table.get::<Value>("should_include")? {
            Value::Function(f) => Some(f.clone()),
            Value::Nil => None,
            v => bail!(
                "Manifest field 'should_include' must be a function, got {}",
                v.type_name()
            ),
        };

        let mut topics: Vec<Topic> = Vec::new();
        for (name, topic) in manifest_table
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

            let mut root = match topic.get("root") {
                Some(Value::String(s)) => {
                    let s = s.to_string_lossy();
                    if s.is_empty() {
                        None
                    } else {
                        Some(Utf8PathBuf::from(s))
                    }
                }
                Some(Value::Nil) | None => None,
                Some(v) => bail!(
                    "Topic '{name}' field 'root' must be a string, got {}",
                    v.type_name()
                ),
            };

            // Normalize absolute/rooted root that falls under the home directory
            normalize_root_under_home(&mut root, home);

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

            let enabled = match topic.get("enabled") {
                Some(Value::Boolean(b)) => *b,
                Some(Value::Nil) | None => true,
                Some(v) => bail!(
                    "Topic '{name}' field 'enabled' must be a boolean, got {}",
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
                enabled,
                root,
                paths,
                to_repo,
                to_system,
                should_include,
            });
        }

        topics.sort_by(|a, b| a.name.cmp(&b.name));

        for topic in &topics {
            topic.validate()?;
        }

        debug!("Manifest loaded with {} topics", topics.len());
        Ok(Manifest {
            lua,
            topics,
            should_include,
        })
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
    fn test_load_manifest_allows_dot_path() {
        let manifest_content = r#"return {
            topics = {
                dotfiles = {
                    paths = {"."},
                }
            }
        }"#;

        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();

        assert_eq!(manifest.topics[0].paths, vec![Utf8PathBuf::from(".")]);
    }

    #[test]
    fn test_load_manifest_rejects_path_parent_dir() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {"../.gitconfig"},
                }
            }
        }"#;

        let err = Manifest::load(manifest_content.as_bytes())
            .unwrap_err()
            .to_string();

        assert!(err.contains("contains '..'"));
    }

    #[test]
    fn test_load_manifest_rejects_absolute_path() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {"/tmp/.gitconfig"},
                }
            }
        }"#;

        let err = Manifest::load(manifest_content.as_bytes())
            .unwrap_err()
            .to_string();

        assert!(err.contains("must be relative"));
    }

    #[test]
    fn test_load_manifest_rejects_root_parent_dir() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = "..",
                    paths = {".gitconfig"},
                }
            }
        }"#;

        let err = Manifest::load(manifest_content.as_bytes())
            .unwrap_err()
            .to_string();

        assert!(err.contains("contains '..'"));
    }

    #[test]
    fn test_load_manifest_rejects_absolute_root() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = "/tmp",
                    paths = {".gitconfig"},
                }
            }
        }"#;

        let err = Manifest::load(manifest_content.as_bytes())
            .unwrap_err()
            .to_string();

        assert!(err.contains("must be relative"));
    }

    #[test]
    fn test_load_manifest_normalizes_absolute_root_under_home() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = "/home/user/.config/git",
                    paths = {"config"},
                }
            }
        }"#;
        let home = Utf8Path::new("/home/user");
        let manifest = Manifest::load_with_home(manifest_content.as_bytes(), Some(home)).unwrap();
        assert_eq!(
            manifest.topics[0].root,
            Some(Utf8PathBuf::from(".config/git"))
        );
    }

    #[test]
    fn test_load_manifest_normalizes_root_equal_to_home() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = "/home/user",
                    paths = {"config"},
                }
            }
        }"#;
        let home = Utf8Path::new("/home/user");
        let manifest = Manifest::load_with_home(manifest_content.as_bytes(), Some(home)).unwrap();
        assert!(manifest.topics[0].root.is_none());
    }

    #[test]
    fn test_load_manifest_absolute_root_outside_home_still_rejected() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = "/opt/other",
                    paths = {"config"},
                }
            }
        }"#;
        let home = Utf8Path::new("/home/user");
        let err = Manifest::load_with_home(manifest_content.as_bytes(), Some(home))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be relative"));
    }

    #[test]
    fn test_load_manifest_rejects_non_string_root() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    root = true,
                    paths = {".gitconfig"},
                }
            }
        }"#;

        let err = Manifest::load(manifest_content.as_bytes())
            .unwrap_err()
            .to_string();

        assert!(err.contains("root"));
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
    fn test_load_manifest_with_enabled() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    enabled = false,
                    paths = {".gitconfig"},
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert!(!manifest.topics[0].enabled);
    }

    #[test]
    fn test_load_manifest_defaults_enabled() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    paths = {".gitconfig"},
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert!(manifest.topics[0].enabled);
    }

    #[test]
    fn test_load_manifest_rejects_non_boolean_enabled() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    enabled = "yes",
                    paths = {".gitconfig"},
                }
            }
        }"#;
        let result = Manifest::load(manifest_content.as_bytes());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("enabled"));
    }

    #[test]
    fn test_load_manifest_with_hostname() {
        let manifest_content = r#"return {
            topics = {
                git = {
                    enabled = porchetta.hostname() ~= "",
                    paths = {".gitconfig"},
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert!(manifest.topics[0].enabled);
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
            enabled: true,
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
            enabled: true,
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
            enabled: true,
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
            enabled: true,
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
            enabled: true,
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

    #[test]
    fn test_load_manifest_with_top_level_should_include() {
        let manifest_content = r#"return {
            should_include = function(path)
                return path:sub(-4) ~= ".bak"
            end,
            topics = {
                git = {
                    paths = {".gitconfig"}
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).unwrap();
        assert!(manifest.should_include.is_some());
        assert!(manifest.should_include(".gitconfig").unwrap());
        assert!(!manifest.should_include("foo.bak").unwrap());
    }

    #[test]
    fn test_load_manifest_rejects_non_function_top_level_should_include() {
        let manifest_content = r#"return {
            should_include = true,
            topics = {
                git = {
                    paths = {".gitconfig"}
                }
            }
        }"#;
        let result = Manifest::load(manifest_content.as_bytes());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("should_include"));
    }

    #[test]
    fn test_manifest_should_include_true() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return true end")
            .eval::<mlua::Function>()
            .unwrap();
        let manifest = Manifest {
            lua: lua.clone(),
            topics: vec![],
            should_include: Some(func),
        };
        assert!(manifest.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_manifest_should_include_false() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return false end")
            .eval::<mlua::Function>()
            .unwrap();
        let manifest = Manifest {
            lua: lua.clone(),
            topics: vec![],
            should_include: Some(func),
        };
        assert!(!manifest.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_manifest_should_include_rejects_non_boolean_return() {
        let lua = Lua::new();
        let func = lua
            .load(r"function(path) return 'yes' end")
            .eval::<mlua::Function>()
            .unwrap();
        let manifest = Manifest {
            lua: lua.clone(),
            topics: vec![],
            should_include: Some(func),
        };
        let result = manifest.should_include("a.txt");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a boolean"));
    }

    #[test]
    fn test_manifest_and_topic_should_include_and_combination() {
        let lua = Lua::new();
        let manifest_func = lua
            .load(r"function(path) return path:sub(-4) ~= '.bak' end")
            .eval::<mlua::Function>()
            .unwrap();
        let topic_func = lua
            .load(r"function(path) return path:sub(1, 1) ~= '.' end")
            .eval::<mlua::Function>()
            .unwrap();
        let manifest = Manifest {
            lua: lua.clone(),
            topics: vec![],
            should_include: Some(manifest_func),
        };
        let topic = Topic {
            name: "test".to_string(),
            enabled: true,
            lua: lua.clone(),
            root: None,
            paths: vec![],
            to_repo: None,
            to_system: None,
            should_include: Some(topic_func),
        };
        // Both allow
        assert!(
            manifest.should_include("gitconfig").unwrap()
                && topic.should_include("gitconfig").unwrap()
        );
        // Manifest rejects .bak
        assert!(
            !(manifest.should_include("foo.bak").unwrap()
                && topic.should_include("foo.bak").unwrap())
        );
        // Topic rejects dotfile
        assert!(
            !(manifest.should_include(".gitconfig").unwrap()
                && topic.should_include(".gitconfig").unwrap())
        );
    }
}
