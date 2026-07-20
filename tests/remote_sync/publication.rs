use std::fs;

use porchetta::engine::resolver::PanickingResolver;
use porchetta::store::{Branch, PorchettaStore};

use super::support::*;

#[test]
fn origin_and_named_remote_handle_same_heads_fast_forwards_and_local_ahead() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, BASIC_MANIFEST);
    commit_topic_files(&origin, "test", &[("config.txt", "base\n")], "seed test");
    commit_topic_files(&origin, "same", &[("config.txt", "same\n")], "seed same");
    clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    assert!(
        local
            .remotes()
            .unwrap()
            .iter()
            .any(|r| r.name() == "origin"),
        "clone should retain origin compatibility"
    );
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);

    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();
    assert_eq!(read(&home.join(".config/test/config.txt")), "base\n");
    let unchanged = topic_head(engine.store(), "same");

    let backup = PorchettaStore::load_at(&backup_path).unwrap();
    let backup_ahead = commit_topic_files(
        &backup,
        "test",
        &[("config.txt", "from backup\n")],
        "backup ahead",
    );
    engine.sync(false, false).unwrap();

    let local = engine.store();
    assert_eq!(topic_head(local, "test"), backup_ahead);
    assert_eq!(read(&home.join(".config/test/config.txt")), "from backup\n");
    assert_eq!(
        topic_head(local, "same"),
        unchanged,
        "same heads must stay unchanged"
    );
    assert_eq!(topic_head(&origin, "test"), backup_ahead);
    fs::write(home.join(".config/test/config.txt"), "local ahead\n").unwrap();
    engine.sync(false, false).unwrap();
    let local_ahead = topic_head(engine.store(), "test");
    assert_ne!(local_ahead, backup_ahead);
    assert_eq!(topic_head(&origin, "test"), local_ahead);
    assert_eq!(topic_head(&backup, "test"), local_ahead);
}

#[test]
fn undeclared_and_system_refs_are_never_transferred_and_clone_ignores_system() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, SINGLE_TOPIC_MANIFEST);
    let test_head = commit_topic_files(&origin, "test", &[("config.txt", "test\n")], "seed test");
    let orphan_base = commit_topic_files(
        &origin,
        "orphan",
        &[("config.txt", "orphan\n")],
        "seed orphan",
    );
    origin
        .update_heads(&[(Branch::system("source-host", "test"), test_head)])
        .unwrap();

    let backup = clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    assert_eq!(
        local.head(&Branch::system("source-host", "test")).unwrap(),
        None,
        "clone must ignore system refs"
    );
    assert_eq!(
        backup.head(&Branch::system("source-host", "test")).unwrap(),
        None
    );
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);

    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();
    let orphan_new = commit_topic_files(
        &origin,
        "orphan",
        &[("config.txt", "origin changed orphan\n")],
        "advance undeclared",
    );
    engine.sync(false, false).unwrap();

    let local = engine.store();
    assert_ne!(orphan_new, orphan_base);
    assert_eq!(topic_head(local, "orphan"), orphan_base);
    assert_eq!(topic_head(&backup, "orphan"), orphan_base);
    assert_eq!(topic_head(&origin, "orphan"), orphan_new);

    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let origin_remote = local
        .remotes()
        .unwrap()
        .into_iter()
        .find(|remote| remote.name() == "origin")
        .unwrap();
    assert!(
        local
            .head(&Branch::system(&hostname, "test"))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        local
            .remote_head(&origin_remote, &Branch::system(&hostname, "test"))
            .unwrap(),
        None
    );
    assert_eq!(
        origin.head(&Branch::system(&hostname, "test")).unwrap(),
        None
    );
    assert_eq!(
        backup.head(&Branch::system(&hostname, "test")).unwrap(),
        None
    );
}

#[test]
fn empty_remote_is_bootstrapped_by_fanout() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let remote_path = path(&temp, "empty-remote");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");
    fs::create_dir_all(&home).unwrap();

    run_git(
        path(&temp, ".").as_path(),
        &["init", "--bare", remote_path.as_str()],
    );
    let local = init_store(&local_path, BASIC_MANIFEST);
    local.add_remote("backup", remote_path.as_str()).unwrap();
    drop(local);

    let engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let remote = PorchettaStore::load_at(&remote_path).unwrap();
    assert_eq!(
        remote.head(&Branch::Manifest).unwrap(),
        engine.store().head(&Branch::Manifest).unwrap()
    );
}
