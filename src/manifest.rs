use anyhow::{Context, Result, bail, ensure};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use log::{debug, trace};
use mlua::{Lua, Value};

/// Machine-readable manifest validation failure.
#[derive(Debug)]
pub struct Diagnostic {
    pub message: String,
    pub code: &'static str,
    pub topic: Option<String>,
    pub field: Option<&'static str>,
    pub value: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}

pub struct Manifest {
    lua: Lua,
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

/// State marker for a topic whose symlink targets may still contain unresolved topic references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unresolved;

/// State marker for a topic whose symlink targets are fully resolved to relative paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved;

pub trait SymlinkState: 'static + std::fmt::Debug {
    type Target: std::fmt::Debug + Clone + PartialEq + Eq;
}

impl SymlinkState for Unresolved {
    type Target = RawSymlinkTarget;
}

impl SymlinkState for Resolved {
    type Target = Utf8PathBuf;
}

/// A raw symlink target before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawSymlinkTarget {
    Literal(Utf8PathBuf),
    TopicRef {
        topic: String,
        subpath: Option<Utf8PathBuf>,
    },
}

/// A symlink managed as part of a topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symlink<S: SymlinkState = Resolved> {
    /// Link location relative to topic root.
    pub link: Utf8PathBuf,
    /// Symlink target (either raw reference or relative path).
    pub target: S::Target,
}

pub struct Topic<S: SymlinkState = Resolved> {
    pub name: String,
    pub enabled: bool,
    lua: Lua,
    pub root: Option<Utf8PathBuf>,
    pub paths: Vec<Utf8PathBuf>,
    pub symlinks: Vec<Symlink<S>>,
    pub to_repo: Option<mlua::Function>,
    pub to_system: Option<mlua::Function>,
    pub should_include: Option<mlua::Function>,
    _state: std::marker::PhantomData<S>,
}

impl<S: SymlinkState> std::fmt::Debug for Topic<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Topic")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("root", &self.root)
            .field("paths", &self.paths)
            .field("symlinks", &self.symlinks)
            .field("has_to_repo", &self.to_repo.is_some())
            .field("has_to_system", &self.to_system.is_some())
            .field("has_should_include", &self.should_include.is_some())
            .field("lua", &"...")
            .finish()
    }
}

impl<S: SymlinkState> Topic<S> {
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
    pub fn to_repo(&self, path: &str, content: impl Into<Vec<u8>>) -> Result<Vec<u8>> {
        let content = content.into();
        match &self.to_repo {
            Some(func) => self.run_transform_hook("to_repo", func, path, &content),
            None => Ok(content),
        }
    }

    /// Run the `to_system` hook, returning transformed content.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-string value.
    pub fn to_system(&self, path: &str, content: impl Into<Vec<u8>>) -> Result<Vec<u8>> {
        let content = content.into();
        match &self.to_system {
            Some(func) => self.run_transform_hook("to_system", func, path, &content),
            None => Ok(content),
        }
    }

    /// Returns the base directory on disk for this topic under `home`.
    #[must_use]
    pub fn base_dir(&self, home: &Utf8Path) -> Utf8PathBuf {
        self.root
            .as_ref()
            .map_or_else(|| home.to_owned(), |root| home.join(root))
    }

    /// Run the `should_include` hook, returning whether a path should be included.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-boolean value.
    pub fn should_include(&self, path: &str) -> Result<bool> {
        run_include_hook(
            &self.lua,
            self.should_include.as_ref(),
            &format!("topic '{}'", self.name),
            path,
        )
    }
}

impl Topic<Resolved> {
    fn validate(&self) -> Result<()> {
        if let Some(root) = &self.root {
            ensure_relative_path(root, &format!("Topic '{}' root", self.name), PathKind::Root)
                .map_err(|error| {
                    let message = error.to_string();
                    error.context(Diagnostic {
                        message,
                        code: "invalid_path",
                        topic: Some(self.name.clone()),
                        field: Some("root"),
                        value: Some(root.to_string()),
                    })
                })?;
        }

        for path in &self.paths {
            ensure_relative_path(path, &format!("Topic '{}' path", self.name), PathKind::Path)
                .map_err(|error| {
                    let message = error.to_string();
                    error.context(Diagnostic {
                        message,
                        code: "invalid_path",
                        topic: Some(self.name.clone()),
                        field: Some("paths"),
                        value: Some(path.to_string()),
                    })
                })?;
        }

        for symlink in &self.symlinks {
            ensure_relative_path(
                &symlink.link,
                &format!("Topic '{}' symlink link", self.name),
                PathKind::SymlinkLink,
            )
            .map_err(|error| {
                let message = error.to_string();
                error.context(Diagnostic {
                    message,
                    code: "invalid_path",
                    topic: Some(self.name.clone()),
                    field: Some("symlinks"),
                    value: Some(symlink.link.to_string()),
                })
            })?;

            ensure!(
                !symlink.target.is_absolute() && !symlink.target.has_root(),
                "Topic '{}' symlink target '{}' must be relative",
                self.name,
                symlink.target
            );

            ensure!(
                !self.paths.contains(&symlink.link),
                "Topic '{}' path '{}' cannot be both in paths and symlinks",
                self.name,
                symlink.link
            );
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PathKind {
    Root,
    Path,
    SymlinkLink,
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
    if matches!(kind, PathKind::SymlinkLink) {
        ensure!(path != Utf8Path::new("."), "{label} may not be '.'");
        ensure!(!path.as_str().is_empty(), "{label} may not be empty");
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

fn run_include_hook(
    lua: &Lua,
    hook: Option<&mlua::Function>,
    scope: &str,
    path: &str,
) -> Result<bool> {
    let Some(func) = hook else {
        return Ok(true);
    };
    let path_arg = lua
        .create_string(path)
        .with_context(|| format!("Failed to create path string for {scope} should_include"))?;
    let result: Value = func
        .call(path_arg)
        .with_context(|| format!("{scope} should_include for '{path}' failed"))?;
    match result {
        Value::Boolean(b) => Ok(b),
        other => bail!(
            "{scope} should_include for '{path}' must return a boolean, got {}",
            other.type_name()
        ),
    }
}

impl Manifest {
    /// Run the manifest-level `should_include` hook, returning whether a path should be included.
    ///
    /// # Errors
    ///
    /// Returns an error if the hook call fails or returns a non-boolean value.
    pub fn should_include(&self, path: &str) -> Result<bool> {
        run_include_hook(
            &self.lua,
            self.should_include.as_ref(),
            "manifest-level",
            path,
        )
    }

    /// Returns whether `path` is included by both the manifest-level and topic-level hooks.
    ///
    /// # Errors
    ///
    /// Returns an error if either hook fails or returns a non-boolean value.
    pub fn includes(&self, topic: &Topic, path: &str) -> Result<bool> {
        Ok(self.should_include(path)? && topic.should_include(path)?)
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
    pub fn load_with_home(manifest_content: &[u8], home: Option<&Utf8Path>) -> Result<Self> {
        Self::parse_with_home(manifest_content, home).map_err(|error| {
            if error.downcast_ref::<Diagnostic>().is_some() {
                error
            } else {
                let message = error.to_string();
                error.context(Diagnostic {
                    message,
                    code: "invalid_field",
                    topic: None,
                    field: None,
                    value: None,
                })
            }
        })
    }

    fn parse_with_home(manifest_content: &[u8], home: Option<&Utf8Path>) -> Result<Self> {
        debug!("Parsing manifest ({:?} bytes)", manifest_content.len());
        let lua = Lua::new();
        lua.globals().set(
            "porchetta",
            crate::lua_runtime::create_porchetta_table(&lua)?,
        )?;

        let manifest_value = lua
            .load(manifest_content)
            .eval::<Value>()
            .map_err(|error| {
                let code = if matches!(error, mlua::Error::SyntaxError { .. }) {
                    "lua_syntax_error"
                } else {
                    "lua_runtime_error"
                };
                let message = error.to_string();
                anyhow::Error::from(error).context(Diagnostic {
                    message,
                    code,
                    topic: None,
                    field: None,
                    value: None,
                })
            })?;
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

        let parsed_topics: Vec<Topic<Unresolved>> = manifest_table
            .get::<std::collections::HashMap<String, std::collections::HashMap<String, Value>>>(
                "topics",
            )?
            .into_iter()
            .map(|(name, topic)| Topic::parse(name, &topic, &lua, home))
            .collect::<Result<_>>()?;

        let topic_roots: std::collections::HashMap<String, Option<Utf8PathBuf>> = parsed_topics
            .iter()
            .map(|t| (t.name.clone(), t.root.clone()))
            .collect();

        let base_home = home.unwrap_or_else(|| Utf8Path::new("/dummy_home"));

        let mut topics: Vec<Topic<Resolved>> = parsed_topics
            .into_iter()
            .map(|topic| topic.resolve(&topic_roots, base_home))
            .collect::<Result<_>>()?;

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

impl Topic<Unresolved> {
    fn parse(
        name: String,
        topic: &std::collections::HashMap<String, Value>,
        lua: &Lua,
        home: Option<&Utf8Path>,
    ) -> Result<Self> {
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

        let symlinks = match topic.get("symlinks") {
            Some(Value::Table(t)) => {
                let mut links = Vec::new();
                for pair in t.clone().pairs::<String, Value>() {
                    let (link, target_val) = pair?;
                    let target = match target_val {
                        Value::String(s) => {
                            RawSymlinkTarget::Literal(Utf8PathBuf::from(s.to_str()?.as_ref()))
                        }
                        Value::Table(ref tbl) if tbl.contains_key("__topic")? => {
                            let topic_ref: String = tbl.get("__topic")?;
                            let subpath: Option<String> = tbl.get("__path")?;
                            RawSymlinkTarget::TopicRef {
                                topic: topic_ref,
                                subpath: subpath.map(Utf8PathBuf::from),
                            }
                        }
                        other => bail!(
                            "Topic '{name}' symlink '{link}' must be a string or topic reference, got {}",
                            other.type_name()
                        ),
                    };
                    links.push(Symlink {
                        link: Utf8PathBuf::from(link),
                        target,
                    });
                }
                links
            }
            Some(Value::Nil) | None => Vec::new(),
            Some(v) => bail!(
                "Topic '{name}' field 'symlinks' must be a table, got {}",
                v.type_name()
            ),
        };

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

        let to_repo = Self::optional_function(topic, &name, "to_repo")?;
        let to_system = Self::optional_function(topic, &name, "to_system")?;
        let should_include = Self::optional_function(topic, &name, "should_include")?;

        let enabled = match topic.get("enabled") {
            Some(Value::Boolean(b)) => *b,
            Some(Value::Nil) | None => true,
            Some(v) => bail!(
                "Topic '{name}' field 'enabled' must be a boolean, got {}",
                v.type_name()
            ),
        };

        Ok(Self {
            name,
            lua: lua.clone(),
            enabled,
            root,
            paths,
            symlinks,
            to_repo,
            to_system,
            should_include,
            _state: std::marker::PhantomData,
        })
    }

    fn resolve(
        self,
        topic_roots: &std::collections::HashMap<String, Option<Utf8PathBuf>>,
        base_home: &Utf8Path,
    ) -> Result<Topic<Resolved>> {
        let topic_base = self.base_dir(base_home);
        let mut resolved_symlinks = Vec::with_capacity(self.symlinks.len());
        for symlink in self.symlinks {
            let target = match symlink.target {
                RawSymlinkTarget::Literal(p) => p,
                RawSymlinkTarget::TopicRef {
                    topic: target_name,
                    subpath,
                } => {
                    let target_root = topic_roots.get(&target_name).ok_or_else(|| {
                        anyhow::Error::from(Diagnostic {
                            message: format!(
                                "Topic '{}' symlink '{}' references unknown topic '{target_name}'",
                                self.name, symlink.link
                            ),
                            code: "unknown_topic",
                            topic: Some(self.name.clone()),
                            field: Some("symlinks"),
                            value: Some(target_name.clone()),
                        })
                    })?;
                    let target_base = target_root
                        .as_ref()
                        .map_or_else(|| base_home.to_owned(), |r| base_home.join(r));
                    let to_path = match subpath {
                        Some(sub) => target_base.join(sub),
                        None => target_base,
                    };
                    let link_abs = topic_base.join(&symlink.link);
                    let from_dir = link_abs.parent().unwrap_or(topic_base.as_path());
                    crate::util::relative_path_between(from_dir, &to_path)
                }
            };
            resolved_symlinks.push(Symlink {
                link: symlink.link,
                target,
            });
        }
        resolved_symlinks.sort_by(|a, b| a.link.cmp(&b.link));

        Ok(Topic {
            name: self.name,
            enabled: self.enabled,
            lua: self.lua,
            root: self.root,
            paths: self.paths,
            symlinks: resolved_symlinks,
            to_repo: self.to_repo,
            to_system: self.to_system,
            should_include: self.should_include,
            _state: std::marker::PhantomData,
        })
    }

    fn optional_function(
        topic: &std::collections::HashMap<String, Value>,
        topic_name: &str,
        field: &str,
    ) -> Result<Option<mlua::Function>> {
        match topic.get(field) {
            Some(Value::Function(f)) => Ok(Some(f.clone())),
            Some(Value::Nil) | None => Ok(None),
            Some(v) => bail!(
                "Topic '{topic_name}' field '{field}' must be a function, got {}",
                v.type_name()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_diagnostics_distinguish_lua_fields_and_paths() {
        for (source, code) in [
            ("return {", "lua_syntax_error"),
            ("error('bad config')", "lua_runtime_error"),
            (
                "return { topics = { git = { paths = {'../bad'} } } }",
                "invalid_path",
            ),
            (
                "return { topics = { git = { paths = {}, enabled = 'yes' } } }",
                "invalid_field",
            ),
        ] {
            let error = Manifest::load(source.as_bytes()).unwrap_err();
            let diagnostic = error
                .downcast_ref::<Diagnostic>()
                .expect("structured validation diagnostic");
            assert_eq!(diagnostic.code, code);
            if code == "invalid_path" {
                assert_eq!(diagnostic.topic.as_deref(), Some("git"));
                assert_eq!(diagnostic.field, Some("paths"));
                assert_eq!(diagnostic.value.as_deref(), Some("../bad"));
            }
        }
    }

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
        let manifest = Manifest::load(
            br#"return { topics = { test = { paths = {"a.txt"},
                to_repo = function(path, content) return content:gsub("hello", "goodbye") end } } }"#,
        )
        .unwrap();
        let result = manifest.topics[0].to_repo("a.txt", b"hello world").unwrap();
        assert_eq!(result, b"goodbye world");
    }

    #[test]
    fn test_to_repo_rejects_non_string_return() {
        let manifest = Manifest::load(
            br#"return { topics = { test = { paths = {"a.txt"},
                to_repo = function(path, content) return 42 end } } }"#,
        )
        .unwrap();
        let err = manifest.topics[0]
            .to_repo("a.txt", b"hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("must return a string"));
    }

    #[test]
    fn test_should_include_true() {
        let manifest = Manifest::load(
            br#"return { topics = { test = { paths = {"a.txt"},
                should_include = function(path) return true end } } }"#,
        )
        .unwrap();
        assert!(manifest.topics[0].should_include("a.txt").unwrap());
    }

    #[test]
    fn test_should_include_false() {
        let manifest = Manifest::load(
            br#"return { topics = { test = { paths = {"a.txt"},
                should_include = function(path) return false end } } }"#,
        )
        .unwrap();
        assert!(!manifest.topics[0].should_include("a.txt").unwrap());
    }

    #[test]
    fn test_should_include_rejects_non_boolean_return() {
        let manifest = Manifest::load(
            br#"return { topics = { test = { paths = {"a.txt"},
                should_include = function(path) return 'yes' end } } }"#,
        )
        .unwrap();
        let err = manifest.topics[0]
            .should_include("a.txt")
            .unwrap_err()
            .to_string();
        assert!(err.contains("must return a boolean"));
    }

    #[test]
    fn test_load_manifest_with_top_level_should_include() {
        let manifest = Manifest::load(
            br#"return {
                should_include = function(path) return path:sub(-4) ~= ".bak" end,
                topics = { git = { paths = {".gitconfig"} } }
            }"#,
        )
        .unwrap();
        assert!(manifest.should_include.is_some());
        assert!(manifest.should_include(".gitconfig").unwrap());
        assert!(!manifest.should_include("foo.bak").unwrap());
    }

    #[test]
    fn test_load_manifest_rejects_non_function_top_level_should_include() {
        let err = Manifest::load(
            br#"return { should_include = true, topics = { git = { paths = {".gitconfig"} } } }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("should_include"));
    }

    #[test]
    fn test_manifest_should_include_true() {
        let manifest = Manifest::load(
            br"return { should_include = function(path) return true end, topics = {} }",
        )
        .unwrap();
        assert!(manifest.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_manifest_should_include_false() {
        let manifest = Manifest::load(
            br"return { should_include = function(path) return false end, topics = {} }",
        )
        .unwrap();
        assert!(!manifest.should_include("a.txt").unwrap());
    }

    #[test]
    fn test_manifest_should_include_rejects_non_boolean_return() {
        let manifest = Manifest::load(
            br"return { should_include = function(path) return 'yes' end, topics = {} }",
        )
        .unwrap();
        let err = manifest.should_include("a.txt").unwrap_err().to_string();
        assert!(err.contains("must return a boolean"));
    }

    #[test]
    fn test_manifest_and_topic_should_include_and_combination() {
        let manifest = Manifest::load(
            br#"return {
                should_include = function(path) return path:sub(-4) ~= '.bak' end,
                topics = {
                    test = {
                        paths = {"gitconfig"},
                        should_include = function(path) return path:sub(1, 1) ~= '.' end,
                    }
                }
            }"#,
        )
        .unwrap();
        let topic = &manifest.topics[0];
        assert!(manifest.includes(topic, "gitconfig").unwrap());
        assert!(!manifest.includes(topic, "foo.bak").unwrap());
        assert!(!manifest.includes(topic, ".gitconfig").unwrap());
    }

    #[test]
    fn test_manifest_symlinks_topic_reference() {
        let manifest = Manifest::load(
            br#"return {
                topics = {
                    pi_agent = {
                        root = ".pi/agent",
                        paths = {"AGENTS.md", "skills"},
                    },
                    agy = {
                        root = ".gemini",
                        paths = {"config.json"},
                        symlinks = {
                            ["GEMINI.md"] = porchetta.topic("pi_agent") / "AGENTS.md",
                            ["skills"] = porchetta.topics.pi_agent / "skills",
                        },
                    },
                }
            }"#,
        )
        .unwrap();

        let agy = manifest.topics.iter().find(|t| t.name == "agy").unwrap();
        assert_eq!(agy.symlinks.len(), 2);
        assert_eq!(agy.symlinks[0].link, "GEMINI.md");
        assert_eq!(agy.symlinks[0].target, "../.pi/agent/AGENTS.md");
        assert_eq!(agy.symlinks[1].link, "skills");
        assert_eq!(agy.symlinks[1].target, "../.pi/agent/skills");
    }

    #[test]
    fn test_manifest_symlinks_literal_string() {
        let manifest = Manifest::load(
            br#"return {
                topics = {
                    agy = {
                        root = ".gemini",
                        paths = {"config.json"},
                        symlinks = {
                            ["GEMINI.md"] = "../.pi/agent/AGENTS.md",
                        },
                    },
                }
            }"#,
        )
        .unwrap();

        let agy = &manifest.topics[0];
        assert_eq!(agy.symlinks.len(), 1);
        assert_eq!(agy.symlinks[0].link, "GEMINI.md");
        assert_eq!(agy.symlinks[0].target, "../.pi/agent/AGENTS.md");
    }

    #[test]
    fn test_manifest_symlinks_unknown_topic() {
        let err = Manifest::load(
            br#"return {
                topics = {
                    agy = {
                        root = ".gemini",
                        paths = {"config.json"},
                        symlinks = {
                            ["GEMINI.md"] = porchetta.topic("nonexistent") / "AGENTS.md",
                        },
                    },
                }
            }"#,
        )
        .unwrap_err();

        let diag = err.downcast_ref::<Diagnostic>().unwrap();
        assert_eq!(diag.code, "unknown_topic");
        assert!(diag.message.contains("unknown topic 'nonexistent'"));
    }

    #[test]
    fn test_manifest_symlinks_rejects_paths_overlap() {
        let err = Manifest::load(
            br#"return {
                topics = {
                    agy = {
                        root = ".gemini",
                        paths = {"GEMINI.md"},
                        symlinks = {
                            ["GEMINI.md"] = "target.md",
                        },
                    },
                }
            }"#,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("cannot be both in paths and symlinks")
        );
    }

    #[test]
    fn test_manifest_symlinks_rejects_parent_dir_in_link() {
        let err = Manifest::load(
            br#"return {
                topics = {
                    agy = {
                        root = ".gemini",
                        paths = {"config.json"},
                        symlinks = {
                            ["../escape.md"] = "target.md",
                        },
                    },
                }
            }"#,
        )
        .unwrap_err();

        let diag = err.downcast_ref::<Diagnostic>().unwrap();
        assert_eq!(diag.code, "invalid_path");
    }
}
