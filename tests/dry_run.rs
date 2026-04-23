use camino::Utf8PathBuf;

#[test]
fn test_sync_dry_run_does_not_create_commits() {
    let temp = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::try_from(temp.path().to_path_buf()).unwrap();
    let store_path = porchetta::store::PorchettaStore::store_path_for(temp.path()).unwrap();

    let store = porchetta::store::PorchettaStore::init_at(&store_path).unwrap();

    let manifest =
        b"return { topics = { test = { root = '.config/test-topic', paths = {'config.txt'} } } }\n";
    store.write_manifest(manifest).unwrap();

    let topic_dir = home.join(".config/test-topic");
    std::fs::create_dir_all(&topic_dir).unwrap();
    std::fs::write(topic_dir.join("config.txt"), "original").unwrap();

    // Dry run sync should not create a topic head.
    let mut engine = porchetta::engine::PorchettaEngine::with_home(store, home.clone());
    engine.sync(false, true).unwrap();

    let store = porchetta::store::PorchettaStore::load_at(&store_path).unwrap();
    assert!(
        store.get_topic_head("test").unwrap().is_none(),
        "dry run should not create a topic head"
    );

    // Real sync should create a topic head.
    let mut engine = porchetta::engine::PorchettaEngine::with_home(store, home.clone());
    engine.sync(false, false).unwrap();

    let store = porchetta::store::PorchettaStore::load_at(&store_path).unwrap();
    let head = store
        .get_topic_head("test")
        .unwrap()
        .expect("real sync should create a topic head");

    // Manually create a new commit on the topic branch with different content.
    let old_tree_id = store
        .repo
        .find_object(head)
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id();
    let new_blob = store.repo.write_blob("modified").unwrap();
    let mut tree_editor = store.repo.edit_tree(old_tree_id).unwrap();
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
        .repo
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

    // File on disk should still be "original".
    let content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(content, "original");

    // Dry run sync again — filesystem and refs must stay untouched.
    let mut engine = porchetta::engine::PorchettaEngine::with_home(store, home.clone());
    engine.sync(false, true).unwrap();

    let content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(content, "original");

    let store = porchetta::store::PorchettaStore::load_at(&store_path).unwrap();
    assert_eq!(
        store.get_topic_head("test").unwrap(),
        Some(new_commit),
        "dry run should not move topic head"
    );

    // Real sync should apply the pending change.
    let mut engine = porchetta::engine::PorchettaEngine::with_home(store, home.clone());
    engine.sync(false, false).unwrap();

    let content = std::fs::read_to_string(topic_dir.join("config.txt")).unwrap();
    assert_eq!(
        content, "modified",
        "real sync should apply remote changes to filesystem"
    );
}
