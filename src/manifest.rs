use std::{
    collections::HashMap,
    path::PathBuf,
};

use anyhow::Result;
use mlua::{Function, Lua, Value};

#[cfg_attr(not(test), expect(dead_code))]
struct Manifest {
    #[allow(dead_code)]
    pub lua: Lua,
    pub topics: HashMap<String, Topic>,
}

#[cfg_attr(not(test), expect(dead_code))]
struct Topic {
    pub paths: Vec<PathBuf>,
    pub predicate: Option<Function>,
}

impl Manifest {
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn load(manifest_content: &[u8]) -> Result<Self> {
        let lua = Lua::new();
        let manifest_value = lua.load(manifest_content).eval::<Value>()?;

        let mut topics = HashMap::new();
        for (name, topic) in manifest_value
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("Manifest must be a table"))?
            .get::<HashMap<String, HashMap<String, Value>>>("topics")?
        {
            let paths: Vec<String> = topic
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Topic must have a 'paths' field"))?
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("'paths' field must be a sequence"))?
                .sequence_values::<String>()
                .collect::<mlua::Result<_>>()?;
            let paths = paths
                .into_iter()
                .map(|s| std::path::PathBuf::from(s))
                .collect();

            let predicate = topic
                .get("predicate")
                .and_then(|v| v.as_function().cloned());

            topics.insert(name, Topic { paths, predicate });
        }

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
                }
            }
        }"#;
        let manifest = Manifest::load(manifest_content.as_bytes()).expect("Failed to load manifest");
        assert_eq!(manifest.topics.len(), 3);
        assert_eq!(manifest.topics["topic1"].paths.len(), 2);
        assert_eq!(manifest.topics["topic2"].paths.len(), 1);
        assert_eq!(manifest.topics["topic3"].paths.len(), 1);
        assert!(manifest.topics["topic1"].predicate.is_none());
        assert!(manifest.topics["topic2"].predicate.is_none());
        assert!(manifest.topics["topic3"].predicate.is_some());

        let predicate = manifest.topics["topic3"].predicate.as_ref().unwrap();
        assert!(predicate.call::<bool>("file.txt").unwrap());
        assert!(!predicate.call::<bool>("file.jpg").unwrap());
    }
}
