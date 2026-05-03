//! Tests for merge behavior, particularly around conflict markers.
//!
//! These tests verify that when git produces merge conflicts, the conflict
//! markers are minimal — only lines that genuinely have conflicts should be
//! wrapped in markers, while unchanged/common regions remain clean.

use camino::Utf8PathBuf;
use gix::bstr::ByteSlice;
use porchetta::engine::PorchettaEngine;
use porchetta::engine::resolver::ConflictResolver;
use porchetta::store::PorchettaStore;
use std::sync::{Arc, Mutex};

struct PassthroughBlobResolver {
    edited_path: Option<Arc<Mutex<Option<String>>>>,
}

struct RecordingTreeResolver {
    resolution: fn() -> porchetta::engine::resolver::TreeConflictResolution,
    prompt: Arc<Mutex<Option<String>>>,
}

impl ConflictResolver for PassthroughBlobResolver {
    fn resolve_tree_conflict(
        &self,
        _prompt: &str,
    ) -> anyhow::Result<porchetta::engine::resolver::TreeConflictResolution> {
        Ok(porchetta::engine::resolver::TreeConflictResolution::KeepOurs)
    }

    fn choose_entry_kind(
        &self,
        _prompt: &str,
        ours: gix::objs::tree::EntryKind,
        _theirs: gix::objs::tree::EntryKind,
    ) -> anyhow::Result<gix::objs::tree::EntryKind> {
        Ok(ours)
    }

    fn edit_blob(&self, content: &[u8], path: &str) -> anyhow::Result<Vec<u8>> {
        assert!(
            has_conflict_markers(content),
            "blob resolver should receive conflict markers"
        );
        if let Some(edited_path) = &self.edited_path {
            *edited_path.lock().unwrap() = Some(path.to_string());
        }
        Ok(content.to_vec())
    }
}

impl ConflictResolver for RecordingTreeResolver {
    fn resolve_tree_conflict(
        &self,
        prompt: &str,
    ) -> anyhow::Result<porchetta::engine::resolver::TreeConflictResolution> {
        *self.prompt.lock().unwrap() = Some(prompt.to_string());
        Ok((self.resolution)())
    }

    fn choose_entry_kind(
        &self,
        _prompt: &str,
        ours: gix::objs::tree::EntryKind,
        _theirs: gix::objs::tree::EntryKind,
    ) -> anyhow::Result<gix::objs::tree::EntryKind> {
        Ok(ours)
    }

    fn edit_blob(&self, _content: &[u8], _path: &str) -> anyhow::Result<Vec<u8>> {
        panic!("tree-conflict test should not invoke blob editing");
    }
}

/// Returns the number of conflict marker lines in content.
fn count_conflict_markers(content: &[u8]) -> usize {
    content
        .lines()
        .filter(|l| {
            l.starts_with(b"<<<<<<<") || l.starts_with(b"=======") || l.starts_with(b">>>>>>>")
        })
        .count()
}

/// Returns true if the content contains conflict markers.
fn has_conflict_markers(content: &[u8]) -> bool {
    count_conflict_markers(content) > 0
}

fn normalize_conflict_labels(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            if line.starts_with("<<<<<<<") {
                "<<<<<<<"
            } else if line.starts_with("|||||||") {
                "|||||||"
            } else if line.starts_with(">>>>>>>") {
                ">>>>>>>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn assert_file_conflict_eq(path: &camino::Utf8Path, expected: &str) {
    let actual = std::fs::read_to_string(path).unwrap();
    assert_eq!(normalize_conflict_labels(&actual), expected);
}

// =============================================================================
// Tests for minimal conflict markers
// =============================================================================

/// Test that when two sides make non-overlapping changes, no conflict markers
/// are produced. This is the common case where auto-merge succeeds.
#[test]
fn test_non_overlapping_changes_no_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();

    // Create initial file and sync to establish base
    std::fs::write(topic_dir.join("config.txt"), "line1\nline2\nline3\n").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    // Modify local: change line 1
    std::fs::write(topic_dir.join("config.txt"), "local-line1\nline2\nline3\n").unwrap();

    // Modify remote: change line 3 (non-overlapping with local changes)
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let new_blob = store.write_blob("line1\nline2\nremote-line3\n").unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    store
        .commit_topic_tree("test", "bogus", new_tree_id, "remote change")
        .unwrap();

    // Sync local - should auto-merge without conflicts
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    let result = engine.sync(false, false);

    // Sync should succeed without conflicts
    assert!(
        result.is_ok(),
        "sync should succeed with non-overlapping changes"
    );

    // File on disk should contain merged content
    let content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert!(
        !has_conflict_markers(content.as_bytes()),
        "non-overlapping changes should not produce conflict markers"
    );
}

/// Test that when the same lines are changed identically on both sides,
/// no conflict markers are produced (auto-merge succeeds).
#[test]
fn test_identical_changes_no_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();

    // Create initial file and sync
    std::fs::write(topic_dir.join("config.txt"), "line1\nline2\nline3\n").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    // Modify local: change line 2
    std::fs::write(topic_dir.join("config.txt"), "line1\nchanged\nline3\n").unwrap();

    // Modify remote with SAME change
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let new_blob = store.write_blob("line1\nchanged\nline3\n").unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    store
        .commit_topic_tree("test", "bogus", new_tree_id, "remote change")
        .unwrap();

    // Sync local - should auto-merge
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    let result = engine.sync(false, false);

    assert!(result.is_ok(), "identical changes should auto-merge");
    let content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert!(!has_conflict_markers(content.as_bytes()));
}

/// Test that when there ARE genuine conflicts, the markers are minimal -
/// only the conflicted region is marked, not the entire file.
#[test]
fn test_conflict_markers_are_minimal() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();

    // Create initial file with a structure that allows clean conflicts
    // Base: header + common + footer
    std::fs::write(
        topic_dir.join("config.txt"),
        "header line\ncommon line\nfooter line\n",
    )
    .unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    // Local changes the header
    std::fs::write(
        topic_dir.join("config.txt"),
        "local header\ncommon line\nfooter line\n",
    )
    .unwrap();

    // Remote changes the SAME region (header) differently
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let new_blob = store
        .write_blob("remote header\ncommon line\nfooter line\n")
        .unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    store
        .commit_topic_tree("test", "bogus", new_tree_id, "remote change")
        .unwrap();

    let resolver = PassthroughBlobResolver { edited_path: None };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    assert_file_conflict_eq(
        &topic_dir.join("config.txt"),
        "<<<<<<<\nlocal header\n|||||||\nheader line\n=======\nremote header\n>>>>>>>\ncommon line\nfooter line\n",
    );
}

/// Verify a large conflict marks only the changed middle block, not surrounding lines.
#[test]
fn test_large_file_conflict_markers_surround_only_conflicted_region() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();

    // Base with many lines
    let base_lines: Vec<String> = (1..=50).map(|i| format!("line{}", i)).collect();
    let base = base_lines.join("\n") + "\n";
    std::fs::write(topic_dir.join("config.txt"), &base).unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    // Local changes lines 25-30 (the middle)
    let mut local_lines = base_lines.clone();
    for i in 25..=30 {
        local_lines[i - 1] = format!("local{}", i);
    }
    let local = local_lines.join("\n") + "\n";
    std::fs::write(topic_dir.join("config.txt"), &local).unwrap();

    // Set up remote change
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let mut remote_lines = base_lines.clone();
    for i in 25..=30 {
        remote_lines[i - 1] = format!("remote{}", i);
    }
    let remote = remote_lines.join("\n") + "\n";

    let new_blob = store.write_blob(&remote).unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    store
        .commit_topic_tree("test", "bogus", new_tree_id, "remote change")
        .unwrap();

    let resolver = PassthroughBlobResolver { edited_path: None };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    let expected = format!(
        "{}<<<<<<<\n{}|||||||\n{}=======\n{}>>>>>>>\n{}",
        (1..=24).map(|i| format!("line{i}\n")).collect::<String>(),
        (25..=30).map(|i| format!("local{i}\n")).collect::<String>(),
        (25..=30).map(|i| format!("line{i}\n")).collect::<String>(),
        (25..=30)
            .map(|i| format!("remote{i}\n"))
            .collect::<String>(),
        (31..=50).map(|i| format!("line{i}\n")).collect::<String>(),
    );
    assert_file_conflict_eq(&topic_dir.join("config.txt"), &expected);
}

#[test]
fn test_partial_line_conflict_markers_minimal() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();

    // Base with specific line patterns
    let base = "alpha\nbeta\ncharlie\ndelta\nepsilon\n";
    std::fs::write(topic_dir.join("config.txt"), base).unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    // Local changes "alpha" and "epsilon"
    let local = "ALPHA\nbeta\ncharlie\ndelta\nEPSILON\n";
    std::fs::write(topic_dir.join("config.txt"), local).unwrap();

    // Remote only changes "alpha" (subset conflict)
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let remote = "REMOTE\nbeta\ncharlie\ndelta\nepsilon\n";
    let new_blob = store.write_blob(remote).unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    store
        .commit_topic_tree("test", "bogus", new_tree_id, "remote change")
        .unwrap();

    let resolver = PassthroughBlobResolver { edited_path: None };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    assert_file_conflict_eq(
        &topic_dir.join("config.txt"),
        "<<<<<<<\nALPHA\n|||||||\nalpha\n=======\nREMOTE\n>>>>>>>\nbeta\ncharlie\ndelta\nEPSILON\n",
    );
}

#[test]
fn test_blob_conflict_passes_path_to_resolver() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"subdir/config.lua"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(topic_dir.join("subdir")).unwrap();
    std::fs::write(topic_dir.join("subdir/config.lua"), "value = 'base'\n").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    std::fs::write(topic_dir.join("subdir/config.lua"), "value = 'local'\n").unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();

    let remote_blob = store.write_blob("value = 'remote'\n").unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert(
            "subdir/config.lua",
            gix::objs::tree::EntryKind::Blob,
            remote_blob,
        )
        .unwrap();
    let remote_tree_id = tree_editor.write().unwrap();
    store
        .commit_topic_tree("test", "bogus", remote_tree_id, "remote change")
        .unwrap();

    let edited_path = Arc::new(Mutex::new(None));
    let resolver = PassthroughBlobResolver {
        edited_path: Some(Arc::clone(&edited_path)),
    };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    assert_eq!(
        edited_path.lock().unwrap().as_deref(),
        Some("subdir/config.lua")
    );
    assert_file_conflict_eq(
        &topic_dir.join("subdir/config.lua"),
        "<<<<<<<\nvalue = 'local'\n|||||||\nvalue = 'base'\n=======\nvalue = 'remote'\n>>>>>>>\n",
    );
}

fn setup_modify_delete_conflict(home: &Utf8PathBuf, store_path: &Utf8PathBuf) {
    let store = PorchettaStore::init_at(store_path).unwrap();
    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "base\n").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    let store = PorchettaStore::load_at(store_path).unwrap();
    let old_head = store.get_topic_head("test").unwrap().unwrap();
    let old_tree = store.find_object(old_head).unwrap().peel_to_tree().unwrap();
    let old_tree_id = old_tree.id();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();

    std::fs::write(topic_dir.join("config.txt"), "local\n").unwrap();
    tree_editor.remove("config.txt").unwrap();

    let remote_tree_id = tree_editor.write().unwrap();
    store
        .commit_topic_tree("test", "bogus", remote_tree_id, "remote change")
        .unwrap();
}

#[test]
fn test_modify_delete_tree_conflict_keep_local_modification() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    setup_modify_delete_conflict(&home, &store_path);

    let prompt = Arc::new(Mutex::new(None));
    let resolver = RecordingTreeResolver {
        resolution: || porchetta::engine::resolver::TreeConflictResolution::KeepOurs,
        prompt: Arc::clone(&prompt),
    };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    let prompt = prompt.lock().unwrap().clone().unwrap();
    assert!(prompt.contains("Resolve tree conflict at 'config.txt'"));
    assert!(prompt.contains("modify 'config.txt'"));
    assert!(prompt.contains("delete Blob at 'config.txt'"));
    assert_eq!(
        std::fs::read_to_string(home.join(".config/test/config.txt")).unwrap(),
        "local\n"
    );
}

#[test]
fn test_modify_delete_tree_conflict_keep_remote_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    setup_modify_delete_conflict(&home, &store_path);

    let prompt = Arc::new(Mutex::new(None));
    let resolver = RecordingTreeResolver {
        resolution: || porchetta::engine::resolver::TreeConflictResolution::KeepTheirs,
        prompt: Arc::clone(&prompt),
    };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    engine.sync(false, false).unwrap();

    let prompt = prompt.lock().unwrap().clone().unwrap();
    assert!(prompt.contains("Resolve tree conflict at 'config.txt'"));
    assert!(prompt.contains("modify 'config.txt'"));
    assert!(prompt.contains("delete Blob at 'config.txt'"));
    assert!(!home.join(".config/test/config.txt").exists());
}

#[test]
fn test_tree_conflict_abort_returns_error() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    setup_modify_delete_conflict(&home, &store_path);

    let prompt = Arc::new(Mutex::new(None));
    let resolver = RecordingTreeResolver {
        resolution: || porchetta::engine::resolver::TreeConflictResolution::Abort,
        prompt: Arc::clone(&prompt),
    };
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(store, home.clone(), resolver);

    let result = engine.sync(false, false);

    assert!(result.is_err());
    assert!(prompt.lock().unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(home.join(".config/test/config.txt")).unwrap(),
        "local\n"
    );
}
