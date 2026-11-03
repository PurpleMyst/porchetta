use camino::Utf8PathBuf;
use porchetta::engine::resolver::PanickingResolver;
use porchetta::store::{Branch, PorchettaStore};

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

    store
        .sync_at(&home, false, true, &[], &PanickingResolver)
        .unwrap();

    std::fs::remove_file(&config_path).unwrap();

    store
        .sync_at(&home, false, true, &[], &PanickingResolver)
        .unwrap();

    let head = store
        .head(&Branch::topic("test"))
        .unwrap()
        .expect("topic head should exist");
    let tree = store
        .repo()
        .find_object(head)
        .unwrap()
        .peel_to_tree()
        .unwrap();

    assert!(
        tree.find_entry("config.txt").is_none(),
        "missing manifest file path should be captured as a deletion"
    );
}
