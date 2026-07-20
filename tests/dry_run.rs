use camino::Utf8PathBuf;
use porchetta::store::Branch;

#[test]
fn dry_run_does_not_move_refs_or_modify_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = porchetta::store::PorchettaStore::store_path_for(temp.path()).unwrap();

    let store = porchetta::store::PorchettaStore::init_at(&store_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test-topic', paths = {'config.txt'} } } }\n";
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test-topic");
    let config_path = topic_dir.join("config.txt");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(&config_path, "original").unwrap();

    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let manifest_before = store.head(&Branch::Manifest).unwrap();
    let config_before = std::fs::read(store_path.join("config")).unwrap();

    let mut engine = porchetta::engine::PorchettaEngine::with_home(
        store,
        home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(true, true).unwrap();

    assert_eq!(
        engine.store().head(&Branch::Manifest).unwrap(),
        manifest_before
    );
    assert_eq!(engine.store().head(&Branch::topic("test")).unwrap(), None);
    assert_eq!(
        engine
            .store()
            .head(&Branch::system(&hostname, "test"))
            .unwrap(),
        None
    );
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "original");
    assert_eq!(
        std::fs::read(store_path.join("config")).unwrap(),
        config_before
    );

    engine.sync(false, true).unwrap();

    let head = engine
        .store()
        .head(&Branch::topic("test"))
        .unwrap()
        .expect("real sync should create a topic head");
    let old_tree_id = engine
        .store()
        .repo()
        .find_object(head)
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id();
    let new_blob = engine.store().repo().write_blob("modified").unwrap();
    let mut tree_editor = engine.store().repo().edit_tree(old_tree_id).unwrap();
    tree_editor
        .upsert("config.txt", gix::objs::tree::EntryKind::Blob, new_blob)
        .unwrap();
    let new_tree_id = tree_editor.write().unwrap();
    let new_commit = engine
        .store()
        .commit_topic_tree("test", "bogus", new_tree_id, "Modified")
        .unwrap();

    let manifest_before = engine.store().head(&Branch::Manifest).unwrap();
    let system_before = engine
        .store()
        .head(&Branch::system(&hostname, "test"))
        .unwrap();
    let config_before = std::fs::read(store_path.join("config")).unwrap();
    engine.sync(true, true).unwrap();

    assert_eq!(
        engine.store().head(&Branch::Manifest).unwrap(),
        manifest_before
    );
    assert_eq!(
        engine.store().head(&Branch::topic("test")).unwrap(),
        Some(new_commit)
    );
    assert_eq!(
        engine
            .store()
            .head(&Branch::system(&hostname, "test"))
            .unwrap(),
        system_before
    );
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "original");
    assert_eq!(
        std::fs::read(store_path.join("config")).unwrap(),
        config_before
    );

    engine.sync(false, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "modified",
        "real sync should apply stored changes to the filesystem"
    );
}
