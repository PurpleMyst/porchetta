use camino::Utf8PathBuf;
use porchetta::engine::PorchettaEngine;
use porchetta::store::PorchettaStore;

#[test]
fn test_capture_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
                to_repo = function(path, content)
                    return content:gsub("SECRET", "REDACTED")
                end,
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "my SECRET value").unwrap();

    let mut engine = PorchettaEngine::with_home(store, home.clone(), porchetta::resolver::PanickingResolver);
    engine.sync(false, false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("topic head should exist");
    let tree = store.find_object(head).unwrap().peel_to_tree().unwrap();
    let entry = tree
        .find_entry("config.txt")
        .expect("config.txt should be in tree");
    let blob = entry.object().unwrap().try_into_blob().unwrap();
    let content = String::from_utf8(blob.data.clone()).unwrap();
    assert_eq!(content, "my REDACTED value");

    // System file should remain unchanged because no remote changes exist.
    let system_content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(system_content, "my SECRET value");
}

#[test]
fn test_apply_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
                to_system = function(path, content)
                    return content:gsub("REPO", "SYSTEM")
                end,
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "initial").unwrap();

    // First sync: capture initial state and push it.
    let mut engine = PorchettaEngine::with_home(store, home.clone(), porchetta::resolver::PanickingResolver);
    engine.sync(false, false, true).unwrap();

    // Manually create a new commit on the topic branch with repo-specific content.
    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("topic head should exist");
    let old_tree_id = store
        .find_object(head)
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id();
    let new_blob = store.write_blob("hello REPO").unwrap();
    let mut tree_editor = store.edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();

    let signature = gix::actor::Signature {
        name: "Test".into(),
        email: "".into(),
        time: gix::date::Time::now_utc(),
    };
    let new_commit = store
        .write_object(gix::objs::Commit {
            tree: new_tree_id.into(),
            parents: [head].into(),
            message: "Modified".into(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            extra_headers: vec![],
        })
        .unwrap()
        .into();
    store.update_topic_head("test", new_commit).unwrap();

    // Sync again: remote change should be transformed by to_system.
    let mut engine = PorchettaEngine::with_home(store, home.clone(), porchetta::resolver::PanickingResolver);
    engine.sync(false, false, true).unwrap();

    let system_content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(system_content, "hello SYSTEM");
}

#[test]
fn test_rewrite_idempotence() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"config.txt"},
                to_repo = function(path, content)
                    return content:gsub("SYSTEM", "REPO")
                end,
                to_system = function(path, content)
                    return content:gsub("REPO", "SYSTEM")
                end,
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "hello SYSTEM").unwrap();

    let mut engine = PorchettaEngine::with_home(store, home.clone(), porchetta::resolver::PanickingResolver);
    engine.sync(false, false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head_after_first = store.get_topic_head("test").unwrap().unwrap();

    // Sync a second time with identical system state.
    let mut engine = PorchettaEngine::with_home(store, home.clone(), porchetta::resolver::PanickingResolver);
    engine.sync(false, false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head_after_second = store.get_topic_head("test").unwrap().unwrap();

    assert_eq!(
        head_after_first, head_after_second,
        "second sync should not create a new commit"
    );

    let system_content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(system_content, "hello SYSTEM");
}
