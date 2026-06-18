use camino::Utf8PathBuf;
use porchetta::engine::PorchettaEngine;
use porchetta::store::PorchettaStore;

#[test]
fn test_should_include_filters_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"."},
                should_include = function(path)
                    return path:sub(-4) ~= ".log"
                end,
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "hello").unwrap();
    std::fs::write(topic_dir.join("debug.log"), "log data").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("topic head should exist");
    let tree = store.find_object(head).unwrap().peel_to_tree().unwrap();

    assert!(
        tree.find_entry("config.txt").is_some(),
        "config.txt should be captured"
    );
    assert!(
        tree.find_entry("debug.log").is_none(),
        "debug.log should be excluded by should_include"
    );

    // Excluded file should still exist on disk.
    assert!(topic_dir.join("debug.log").exists());
}

#[test]
fn test_should_include_filters_directory_recursion() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            test = {
                root = ".config/test",
                paths = {"."},
                should_include = function(path)
                    return not path:match("^cache")
                end,
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "hello").unwrap();
    std::fs::create_dir_all(topic_dir.join("cache")).unwrap();
    std::fs::write(topic_dir.join("cache").join("file.txt"), "cached").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("topic head should exist");
    let tree = store.find_object(head).unwrap().peel_to_tree().unwrap();

    assert!(
        tree.find_entry("config.txt").is_some(),
        "config.txt should be captured"
    );
    assert!(
        tree.find_entry("cache/file.txt").is_none(),
        "files inside excluded directory should not be captured"
    );
}

#[test]
fn test_sync_skips_disabled_topics() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest = br#"return {
        topics = {
            enabled = {
                root = ".config/enabled",
                paths = {"."},
            },
            disabled = {
                enabled = false,
                root = ".config/disabled",
                paths = {"."},
            }
        }
    }"#;
    store.write_manifest(manifest).unwrap();

    let enabled_dir = home.join(".config/enabled");
    std::fs::create_dir_all(&enabled_dir).unwrap();
    std::fs::write(enabled_dir.join("config.txt"), "enabled").unwrap();

    let disabled_dir = home.join(".config/disabled");
    std::fs::create_dir_all(&disabled_dir).unwrap();
    std::fs::write(disabled_dir.join("config.txt"), "disabled").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    assert!(store.get_topic_head("enabled").unwrap().is_some());
    assert!(store.get_topic_head("disabled").unwrap().is_none());
}
