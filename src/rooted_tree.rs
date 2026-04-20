use std::collections::HashMap;
use std::path::Path;
use std::{fs, path::PathBuf};

use anyhow::{Result, anyhow, bail};
use gix::objs::{Blob, Object, Tree, WriteTo, compute_hash, tree::EntryKind};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct RootedTree {
    pub root: PathBuf,
    pub tree_oid: gix::hash::ObjectId,
    pub objects: HashMap<gix::hash::ObjectId, Object>,
}

impl RootedTree {
    pub fn capture(
        root: PathBuf,
        normalize_content: impl Fn(&Path, Vec<u8>) -> Result<Vec<u8>>,
        should_include: impl Fn(&Path) -> bool,
    ) -> Result<Self> {
        let wd = WalkDir::new(&root).contents_first(true).sort_by_file_name();

        let mut tree_entries = HashMap::new();
        let mut objects = HashMap::<gix::hash::ObjectId, Object>::new();

        for maybe_entry in wd {
            let entry = maybe_entry?;

            if !should_include(&entry.path()) {
                continue;
            }

            let n = entry.path()
            .strip_prefix(&root)?
            .components().count().saturating_sub(1);
            eprintln!("{}{}", "  ".repeat(n), entry.path().display());

            let ty = entry.file_type();
            let key = entry.path().strip_prefix(&root)?.to_path_buf();
            if ty.is_file() {
                let blob = Blob {
                    data: normalize_content(entry.path(), fs::read(entry.path())?)?,
                };
                let oid = compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &blob.data)?;
                let kind = EntryKind::Blob;
                tree_entries.insert(
                    key,
                    gix::objs::tree::Entry {
                        mode: kind.into(),
                        filename: entry.file_name().to_string_lossy().as_bytes().into(),
                        oid: oid.into(),
                    },
                );
                objects.insert(oid.into(), Object::Blob(blob));
            } else if ty.is_dir() {
                let mut this_tree_entries = Vec::new();
                for maybe_subentry in fs::read_dir(entry.path())? {
                    let subentry = maybe_subentry?;
                    if !should_include(&subentry.path()) {
                        continue;
                    }
                    let subkey = key.join(subentry.file_name());
                    let new_entry = tree_entries
                        .get(&subkey)
                        .ok_or_else(|| anyhow!("Missing tree entry for {}", subkey.display()))?;
                    this_tree_entries.push(new_entry.clone());
                }
                let this_tree = Tree {
                    entries: this_tree_entries,
                };
                let mut data = Vec::<u8>::new();
                this_tree.write_to(&mut data)?;
                let oid = compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Tree, &data)?;
                tree_entries.insert(
                    key,
                    gix::objs::tree::Entry {
                        mode: EntryKind::Tree.into(),
                        filename: entry.file_name().to_string_lossy().as_bytes().into(),
                        oid: oid.into(),
                    },
                );
                objects.insert(oid.into(), Object::Tree(this_tree));
            } else {
                bail!("Unsupported file type: {}", entry.path().display());
            }
        }
        Ok(Self {
            root,
            tree_oid: tree_entries
                .get(&PathBuf::from(""))
                .ok_or_else(|| anyhow!("Missing root tree entry"))?
                .oid,
            objects,
        })
    }

    /// Applies the captured tree to the filesystem, creating files and directories as needed.
    pub fn apply(mut self, normalize_content: impl Fn(&Path, Vec<u8>) -> Result<Vec<u8>>,) -> Result<()> {
        let mut stack = vec![(self.root.clone(), self.tree_oid)];
        while let Some((path, oid)) = stack.pop() {
            let obj = self.objects.remove(&oid).ok_or_else(|| {
                anyhow!("Missing object for OID {} at path {}", oid, path.display())
            })?;
            match obj {
                Object::Blob(blob) => {
                    fs::create_dir_all(path.parent().unwrap())?;
                    fs::write(&path, normalize_content(&path, blob.data)? )?;
                }
                Object::Tree(tree) => {
                    fs::create_dir_all(&path)?;
                    for entry in &tree.entries {
                        let entry_path = path.join(std::str::from_utf8(&entry.filename)?);
                        stack.push((entry_path, entry.oid.into()));
                    }
                }
                _ => bail!("Unexpected object type for OID {} at path {}", oid, path.display()),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_rooted_tree_roundtrip() -> Result<()> {
        // 1. Setup the source temporary directory with a specific file structure
        let source_dir = tempdir()?;
        let source_root = source_dir.path().to_path_buf();

        // Create a file at the root
        let file1_path = source_root.join("root_file.txt");
        fs::write(&file1_path, b"Hello from the root")?;

        // Create a nested directory with a file
        let nested_dir = source_root.join("nested");
        fs::create_dir(&nested_dir)?;
        let file2_path = nested_dir.join("nested_file.txt");
        fs::write(&file2_path, b"Hello from the nest")?;

        // Create a deeply nested empty directory just to ensure structural integrity
        let deep_dir = nested_dir.join("deep_empty");
        fs::create_dir(&deep_dir)?;

        // 2. Capture the directory state into our RootedTree
        let mut captured_tree = RootedTree::capture(
            source_root.clone(),
            |_path, content| Ok(content), // Pass-through normalization
            |_path| true,                 // Accept all files and directories
        )?;

        // Assert that the initial capture grabbed our objects
        assert!(!captured_tree.objects.is_empty(), "Tree should have captured objects");

        // 3. Setup the destination temporary directory to test the `apply` method
        let dest_dir = tempdir()?;
        let dest_root = dest_dir.path().to_path_buf();

        // Mutate the root of the captured tree so it extracts to our new temporary directory
        captured_tree.root = dest_root.clone();

        // 4. Apply the tree to the filesystem
        captured_tree.apply(|_path, content| Ok(content))?;

        // 5. Verify the roundtripped filesystem matches the original
        let dest_file1 = dest_root.join("root_file.txt");
        assert!(dest_file1.exists(), "root_file.txt should exist");
        assert_eq!(fs::read(&dest_file1)?, b"Hello from the root");

        let dest_file2 = dest_root.join("nested").join("nested_file.txt");
        assert!(dest_file2.exists(), "nested_file.txt should exist");
        assert_eq!(fs::read(&dest_file2)?, b"Hello from the nest");

        let dest_deep_dir = dest_root.join("nested").join("deep_empty");
        assert!(dest_deep_dir.exists() && dest_deep_dir.is_dir(), "deep_empty directory should exist");

        Ok(())
    }

    #[test]
    fn test_rooted_tree_predicate_filtering() -> Result<()> {
        let source_dir = tempdir()?;
        let source_root = source_dir.path().to_path_buf();

        // Create two files, we will filter one out
        fs::write(source_root.join("keep.txt"), b"keep me")?;
        fs::write(source_root.join("ignore.txt"), b"ignore me")?;

        // Capture, ignoring "ignore.txt"
        let mut captured_tree = RootedTree::capture(
            source_root.clone(),
            |_path, content| Ok(content),
            |path| !path.to_string_lossy().contains("ignore.txt"),
        )?;

        let dest_dir = tempdir()?;
        let dest_root = dest_dir.path().to_path_buf();
        captured_tree.root = dest_root.clone();
        captured_tree.apply(|_path, content| Ok(content))?;

        assert!(dest_root.join("keep.txt").exists());
        assert!(!dest_root.join("ignore.txt").exists(), "The ignored file should not have roundtripped");

        Ok(())
    }
}
