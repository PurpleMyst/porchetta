use std::fs;
use std::sync::atomic::Ordering;

use porchetta::engine::merge::is_ancestor;
use porchetta::engine::resolver::PanickingResolver;
use porchetta::store::{Branch, PorchettaStore};

use super::support::*;

#[test]
fn reconciles_independent_conflicting_and_three_head_topics() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, RECONCILIATION_MANIFEST);
    commit_topic_files(
        &origin,
        "clean",
        &[("config.txt", "one\nmiddle\nthree\n")],
        "seed clean",
    );
    commit_topic_files(
        &origin,
        "conflict",
        &[("config.txt", "base\n")],
        "seed conflict",
    );
    commit_topic_files(
        &origin,
        "three",
        &[("config.txt", "one\ntwo\nthree\nfour\nfive\n")],
        "seed three",
    );
    clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);
    sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    fs::write(home.join(".config/conflict/config.txt"), "local\n").unwrap();
    fs::write(
        home.join(".config/three/config.txt"),
        "LOCAL\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    sync_with(&local_path, &home, PanickingResolver, false, true).unwrap();

    commit_topic_files(
        &origin,
        "clean",
        &[("config.txt", "ORIGIN\nmiddle\nthree\n")],
        "origin clean",
    );
    commit_topic_files(
        &origin,
        "conflict",
        &[("config.txt", "remote\n")],
        "remote conflict",
    );
    commit_topic_files(
        &origin,
        "three",
        &[("config.txt", "one\ntwo\nORIGIN\nfour\nfive\n")],
        "origin three",
    );
    let backup = PorchettaStore::load_at(&backup_path).unwrap();
    commit_topic_files(
        &backup,
        "clean",
        &[("config.txt", "one\nmiddle\nBACKUP\n")],
        "backup clean",
    );
    commit_topic_files(
        &backup,
        "three",
        &[("config.txt", "one\ntwo\nthree\nfour\nBACKUP\n")],
        "backup three",
    );

    let (resolver, calls) = ReplacingResolver::new(b"resolved\n");
    let engine = sync_with(&local_path, &home, resolver, false, false).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        read(&home.join(".config/clean/config.txt")),
        "ORIGIN\nmiddle\nBACKUP\n"
    );
    assert_eq!(
        read(&home.join(".config/conflict/config.txt")),
        "resolved\n"
    );
    assert_eq!(
        read(&home.join(".config/three/config.txt")),
        "LOCAL\ntwo\nORIGIN\nfour\nBACKUP\n"
    );

    for topic in ["clean", "conflict", "three"] {
        let head = topic_head(engine.store(), topic);
        assert_eq!(topic_head(&origin, topic), head);
        assert_eq!(topic_head(&backup, topic), head);
    }
}

#[test]
fn reconciles_clean_manifest_changes() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");
    init_store(&origin_path, MANIFEST_RECONCILIATION_BASE);
    clone_store(&origin_path, &local_path);

    PorchettaStore::load_at(&local_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CLEAN_LOCAL)
        .unwrap();
    PorchettaStore::load_at(&origin_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CLEAN_REMOTE)
        .unwrap();

    let engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let local = engine.store();
    let merged = String::from_utf8(local.read_manifest().unwrap()).unwrap();
    assert!(merged.contains("alpha-local"));
    assert!(merged.contains("beta-remote"));
    assert_eq!(
        local.head(&Branch::Manifest).unwrap(),
        PorchettaStore::load_at(&origin_path)
            .unwrap()
            .head(&Branch::Manifest)
            .unwrap()
    );
}

#[test]
fn reconciles_conflicting_manifests() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");
    init_store(&origin_path, MANIFEST_RECONCILIATION_BASE);
    clone_store(&origin_path, &local_path);

    PorchettaStore::load_at(&local_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CONFLICT_LOCAL)
        .unwrap();
    PorchettaStore::load_at(&origin_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CONFLICT_REMOTE)
        .unwrap();
    let (resolver, calls) = ReplacingResolver::new(MANIFEST_RECONCILIATION_RESOLVED);

    let engine = sync_with(&local_path, &home, resolver, false, false).unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        engine.store().read_manifest().unwrap(),
        MANIFEST_RECONCILIATION_RESOLVED
    );
}

#[test]
fn rejects_invalid_reconciled_manifest_atomically() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");
    init_store(&origin_path, MANIFEST_RECONCILIATION_BASE);
    clone_store(&origin_path, &local_path);

    PorchettaStore::load_at(&local_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CONFLICT_LOCAL)
        .unwrap();
    PorchettaStore::load_at(&origin_path)
        .unwrap()
        .write_manifest(MANIFEST_RECONCILIATION_CONFLICT_REMOTE)
        .unwrap();
    let local_before = PorchettaStore::load_at(&local_path)
        .unwrap()
        .head(&Branch::Manifest)
        .unwrap();
    let remote_before = PorchettaStore::load_at(&origin_path)
        .unwrap()
        .head(&Branch::Manifest)
        .unwrap();
    let (resolver, _) = ReplacingResolver::new(MANIFEST_RECONCILIATION_INVALID);

    let error = sync_with(&local_path, &home, resolver, false, false)
        .err()
        .expect("sync should fail");

    assert!(error.to_string().contains("Reconciled manifest is invalid"));
    assert_eq!(
        PorchettaStore::load_at(&local_path)
            .unwrap()
            .head(&Branch::Manifest)
            .unwrap(),
        local_before,
        "invalid reconciliation must not move the local canonical ref"
    );
    assert_eq!(
        PorchettaStore::load_at(&origin_path)
            .unwrap()
            .head(&Branch::Manifest)
            .unwrap(),
        remote_before,
        "invalid reconciliation must not publish"
    );
}

#[test]
fn disabled_topics_are_reconciled_and_pushed_without_being_applied() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, DISABLED_TOPIC_MANIFEST);
    let base = commit_topic_files(
        &origin,
        "disabled",
        &[("a.txt", "a\n"), ("b.txt", "b\n")],
        "seed disabled",
    );
    clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);
    fs::create_dir_all(home.join(".config/disabled")).unwrap();
    fs::write(home.join(".config/disabled/a.txt"), "do not apply\n").unwrap();

    let origin_head = commit_topic_files(
        &origin,
        "disabled",
        &[("a.txt", "origin\n")],
        "origin disabled",
    );
    let backup_head = commit_topic_files(
        &PorchettaStore::load_at(&backup_path).unwrap(),
        "disabled",
        &[("b.txt", "backup\n")],
        "backup disabled",
    );
    assert_ne!(origin_head, base);
    assert_ne!(backup_head, base);

    let engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();
    assert_eq!(read(&home.join(".config/disabled/a.txt")), "do not apply\n");
    assert!(!home.join(".config/disabled/b.txt").exists());
    let local = engine.store();
    let merged = topic_head(local, "disabled");
    assert!(is_ancestor(local.repo(), origin_head, merged).unwrap());
    assert!(is_ancestor(local.repo(), backup_head, merged).unwrap());
    assert_eq!(topic_head(&origin, "disabled"), merged);
    assert_eq!(
        topic_head(&PorchettaStore::load_at(&backup_path).unwrap(), "disabled"),
        merged
    );
    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        local.head(&Branch::system(&hostname, "disabled")).unwrap(),
        None
    );
}

#[test]
fn divergent_remote_dry_run_preserves_state_and_offline_sync_stays_local() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, SINGLE_TOPIC_MANIFEST);
    commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "one\ntwo\nthree\nfour\nfive\n")],
        "seed",
    );
    clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);
    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let origin_head = commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "one\ntwo\nthree\nfour\nORIGIN\n")],
        "origin ahead",
    );
    let backup = PorchettaStore::load_at(&backup_path).unwrap();
    let backup_head = commit_topic_files(
        &backup,
        "test",
        &[("config.txt", "one\ntwo\nBACKUP\nfour\nfive\n")],
        "backup ahead",
    );
    fs::write(
        home.join(".config/test/config.txt"),
        "LOCAL\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();

    let manifest_before = engine.store().head(&Branch::Manifest).unwrap();
    let topic_before = topic_head(engine.store(), "test");
    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let system_before = engine
        .store()
        .head(&Branch::system(&hostname, "test"))
        .unwrap();
    let config_before = run_git(&local_path, &["config", "--local", "--list"]);
    let origin_manifest = origin.head(&Branch::Manifest).unwrap();
    let backup_manifest = backup.head(&Branch::Manifest).unwrap();

    engine.sync(true, false).unwrap();

    assert!(!is_ancestor(engine.store().repo(), origin_head, backup_head).unwrap());
    assert!(!is_ancestor(engine.store().repo(), backup_head, origin_head).unwrap());
    assert_eq!(
        engine.store().head(&Branch::Manifest).unwrap(),
        manifest_before
    );
    assert_eq!(topic_head(engine.store(), "test"), topic_before);
    assert_eq!(
        engine
            .store()
            .head(&Branch::system(&hostname, "test"))
            .unwrap(),
        system_before
    );
    assert_eq!(
        read(&home.join(".config/test/config.txt")),
        "LOCAL\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(
        run_git(&local_path, &["config", "--local", "--list"]),
        config_before
    );
    assert_eq!(origin.head(&Branch::Manifest).unwrap(), origin_manifest);
    assert_eq!(backup.head(&Branch::Manifest).unwrap(), backup_manifest);
    assert_eq!(topic_head(&origin, "test"), origin_head);
    assert_eq!(topic_head(&backup, "test"), backup_head);

    engine.sync(false, true).unwrap();
    let offline_head = topic_head(engine.store(), "test");
    assert_ne!(
        offline_head, topic_before,
        "offline sync should still capture locally"
    );
    assert_eq!(topic_head(&origin, "test"), origin_head);
    assert_eq!(topic_head(&backup, "test"), backup_head);
    assert_eq!(
        read(&home.join(".config/test/config.txt")),
        "LOCAL\ntwo\nthree\nfour\nfive\n"
    );
}

#[test]
fn reconciles_criss_cross_heads_with_multiple_merge_bases() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, SINGLE_TOPIC_MANIFEST);
    let base = commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "one\ntwo\nthree\n")],
        "base",
    );
    let base_tree = topic_tree_oid(&origin, "test");
    let a1 = commit_tree(
        &origin,
        base_tree,
        &[("config.txt", "A\ntwo\nthree\n")],
        &[base],
        "a1",
    );
    let b1 = commit_tree(
        &origin,
        base_tree,
        &[("config.txt", "one\ntwo\nB\n")],
        &[base],
        "b1",
    );
    let merged_tree = {
        let mut editor = origin.repo().edit_tree(base_tree).unwrap();
        let blob = origin.repo().write_blob("A\ntwo\nB\n").unwrap();
        editor
            .upsert("config.txt", gix::objs::tree::EntryKind::Blob, blob)
            .unwrap();
        editor.write().unwrap()
    };
    let a2 = origin.commit_tree(merged_tree, [a1, b1], "a2").unwrap();
    origin.update_heads(&[(Branch::topic("test"), a2)]).unwrap();

    let backup = clone_store(&origin_path, &backup_path);
    let b2 = backup.commit_tree(merged_tree, [b1, a1], "b2").unwrap();
    backup.update_heads(&[(Branch::topic("test"), b2)]).unwrap();
    let local = clone_store(&origin_path, &local_path);
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);
    let engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let bases = run_git(
        &local_path,
        &["merge-base", "--all", &a2.to_string(), &b2.to_string()],
    );
    assert_eq!(
        bases.lines().count(),
        2,
        "fixture must have two merge bases"
    );
    let local = engine.store();
    let reconciled = topic_head(local, "test");
    assert!(is_ancestor(local.repo(), a2, reconciled).unwrap());
    assert!(is_ancestor(local.repo(), b2, reconciled).unwrap());
    assert_eq!(read(&home.join(".config/test/config.txt")), "A\ntwo\nB\n");
    assert_eq!(topic_head(&origin, "test"), reconciled);
    assert_eq!(topic_head(&backup, "test"), reconciled);
}

#[test]
fn dry_run_reports_reconciliation_conflicts_without_failing() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let backup_path = path(&temp, "backup");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, SINGLE_TOPIC_MANIFEST);
    commit_topic_files(&origin, "test", &[("config.txt", "base\n")], "seed");
    clone_store(&origin_path, &backup_path);
    let local = clone_store(&origin_path, &local_path);
    local.add_remote("backup", backup_path.as_str()).unwrap();
    drop(local);
    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    // Diverge both remotes on the same line so reconciliation must conflict.
    commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "origin\n")],
        "origin conflicting",
    );
    let backup = PorchettaStore::load_at(&backup_path).unwrap();
    commit_topic_files(
        &backup,
        "test",
        &[("config.txt", "backup\n")],
        "backup conflicting",
    );

    let topic_before = topic_head(engine.store(), "test");
    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let system_before = engine
        .store()
        .head(&Branch::system(&hostname, "test"))
        .unwrap();

    // PanickingResolver proves no interactive resolution is attempted.
    engine.sync(true, false).unwrap();

    assert_eq!(topic_head(engine.store(), "test"), topic_before);
    assert_eq!(
        engine
            .store()
            .head(&Branch::system(&hostname, "test"))
            .unwrap(),
        system_before
    );
    assert_eq!(read(&home.join(".config/test/config.txt")), "base\n");
}
