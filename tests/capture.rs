use camino::Utf8PathBuf;
use porchetta::engine::PorchettaEngine;
use porchetta::store::PorchettaStore;

#[test]
fn test_capture_missing_manifest_file_path_removes_file() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = PorchettaStore::store_path_for(temp.path()).unwrap();
    let store = PorchettaStore::init_at(&store_path).unwrap();

    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test");
    std::fs::create_dir_all(&topic_dir).unwrap();
    let config_path = topic_dir.join("config.txt");
    std::fs::write(&config_path, "initial").unwrap();

    let mut engine = PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    std::fs::remove_file(&config_path).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        store,
        home,
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    let store = PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("topic head should exist");
    let tree = store.find_object(head).unwrap().peel_to_tree().unwrap();

    assert!(
        tree.find_entry("config.txt").is_none(),
        "missing manifest file path should be captured as a deletion"
    );
}
