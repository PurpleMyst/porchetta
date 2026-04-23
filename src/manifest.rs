use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;
use log::{debug, trace};
use mlua::{Function, Lua, Value};

#[derive(Debug)]
pub struct Manifest {
    #[allow(dead_code)]
    pub lua: Lua,
    pub topics: HashMap<String, Topic>,
}

#[derive(Debug)]
pub struct Topic {
    pub root: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub predicate: Option<Function>,
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
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect();

            trace!("Topic '{}' has {} paths", name, paths.len());

            let root = topic
                .get("root")
                .and_then(|v| v.as_string())
                .and_then(|s| {
                    let s = s.to_string_lossy();
                    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
                });

            let predicate = topic
                .get("predicate")
                .and_then(|v| v.as_function().cloned());

            topics.insert(name, Topic { root, paths, predicate });
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
                    paths = {"path/to/file4"},
                    predicate = function(path) return path:match("%.txt$") end
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
        assert!(manifest.topics["topic1"].predicate.is_none());
        assert!(manifest.topics["topic2"].predicate.is_none());
        assert!(manifest.topics["topic3"].predicate.is_some());
        assert!(manifest.topics["topic4"].predicate.is_none());
        assert!(manifest.topics["topic5"].predicate.is_none());

        assert_eq!(
            manifest.topics["topic4"].root,
            Some(PathBuf::from(".config/nvim"))
        );
        assert!(manifest.topics["topic1"].root.is_none());
        assert!(manifest.topics["topic5"].root.is_none()); // empty string normalized to None

        let predicate = manifest.topics["topic3"].predicate.as_ref().unwrap();
        assert!(predicate.call::<bool>("file.txt").unwrap());
        assert!(!predicate.call::<bool>("file.jpg").unwrap());
    }
}
