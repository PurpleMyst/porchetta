use std::fs;

use porchetta::engine::resolver::PanickingResolver;
use porchetta::store::{Branch, PorchettaStore};

use super::support::*;

#[test]
fn fetch_failure_happens_before_canonical_or_filesystem_changes() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let local_path = path(&temp, "local");
    let missing_path = path(&temp, "missing");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, BASIC_MANIFEST);
    commit_topic_files(&origin, "test", &[("config.txt", "base\n")], "seed test");
    commit_topic_files(&origin, "same", &[("config.txt", "same\n")], "seed same");
    let local = clone_store(&origin_path, &local_path);
    drop(local);
    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();
    engine
        .store()
        .add_remote("a-broken", missing_path.as_str())
        .unwrap();

    commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "remote\n")],
        "remote ahead",
    );
    fs::write(home.join(".config/test/config.txt"), "pending local\n").unwrap();
    let canonical_before = topic_head(engine.store(), "test");
    let hostname = hostname::get().unwrap().to_string_lossy().into_owned();
    let system_before = engine
        .store()
        .head(&Branch::system(&hostname, "test"))
        .unwrap();
    let remote_before = topic_head(&origin, "test");

    let error = engine.sync(false, false).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Failed to fetch remote 'a-broken'")
    );
    assert_eq!(topic_head(engine.store(), "test"), canonical_before);
    assert_eq!(
        engine
            .store()
            .head(&Branch::system(&hostname, "test"))
            .unwrap(),
        system_before
    );
    assert_eq!(
        read(&home.join(".config/test/config.txt")),
        "pending local\n"
    );
    assert_eq!(topic_head(&origin, "test"), remote_before);
}

#[test]
fn push_failure_continues_fanout_and_retry_repairs_only_the_failed_remote() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let reject_path = path(&temp, "reject");
    let good_path = path(&temp, "good");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let reject = init_store(&reject_path, BASIC_MANIFEST);
    commit_topic_files(
        &reject,
        "test",
        &[("config.txt", "test base\n")],
        "seed test",
    );
    commit_topic_files(
        &reject,
        "same",
        &[("config.txt", "same base\n")],
        "seed same",
    );
    clone_store(&reject_path, &good_path);
    let local = clone_store(&reject_path, &local_path);
    local.remove_remote("origin").unwrap();
    local.add_remote("a-reject", reject_path.as_str()).unwrap();
    local.add_remote("z-good", good_path.as_str()).unwrap();
    drop(local);
    let mut engine = sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let old_manifest = reject.head(&Branch::Manifest).unwrap().unwrap();
    let old_test = topic_head(&reject, "test");
    let old_same = topic_head(&reject, "same");
    let hook = install_reject_topic_hook(&reject_path, "test");
    drop(reject);

    engine.store().write_manifest(BASIC_MANIFEST).unwrap();
    fs::write(home.join(".config/test/config.txt"), "test fanout\n").unwrap();
    fs::write(home.join(".config/same/config.txt"), "same fanout\n").unwrap();
    let error = engine.sync(false, false).unwrap_err();
    assert!(error.to_string().contains("a-reject"));

    let local = engine.store();
    let new_manifest = local.head(&Branch::Manifest).unwrap().unwrap();
    let new_test = topic_head(local, "test");
    let new_same = topic_head(local, "same");
    assert_ne!(new_manifest, old_manifest);
    assert_ne!(new_test, old_test);
    assert_ne!(new_same, old_same);

    let rejected = PorchettaStore::load_at(&reject_path).unwrap();
    assert_eq!(
        rejected.head(&Branch::Manifest).unwrap(),
        Some(old_manifest)
    );
    assert_eq!(topic_head(&rejected, "test"), old_test);
    assert_eq!(topic_head(&rejected, "same"), old_same);

    let good = PorchettaStore::load_at(&good_path).unwrap();
    assert_eq!(good.head(&Branch::Manifest).unwrap(), Some(new_manifest));
    assert_eq!(topic_head(&good, "test"), new_test);
    assert_eq!(topic_head(&good, "same"), new_same);

    fs::remove_file(hook).unwrap();
    drop(rejected);
    engine.sync(false, false).unwrap();
    let rejected = PorchettaStore::load_at(&reject_path).unwrap();
    assert_eq!(
        rejected.head(&Branch::Manifest).unwrap(),
        Some(new_manifest)
    );
    assert_eq!(topic_head(&rejected, "test"), new_test);
    assert_eq!(topic_head(&rejected, "same"), new_same);
}

#[test]
fn non_fast_forward_race_is_reported_without_forcing() {
    let _guard = remote_sync_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let origin_path = path(&temp, "origin");
    let local_path = path(&temp, "local");
    let home = path(&temp, "home");

    let origin = init_store(&origin_path, SINGLE_TOPIC_MANIFEST);
    let base = commit_topic_files(&origin, "test", &[("config.txt", "base\n")], "seed");
    clone_store(&origin_path, &local_path);
    sync_with(&local_path, &home, PanickingResolver, false, false).unwrap();

    let raced = commit_topic_files(
        &origin,
        "test",
        &[("config.txt", "raced\n")],
        "racing writer",
    );
    origin
        .update_heads(&[(Branch::topic("test"), base)])
        .unwrap();
    let wrapper = install_receive_pack_race(&temp, &origin_path, raced);
    run_git(
        &local_path,
        &["config", "remote.origin.receivepack", wrapper.as_str()],
    );

    fs::write(home.join(".config/test/config.txt"), "local writer\n").unwrap();
    let error = sync_with(&local_path, &home, PanickingResolver, false, false)
        .err()
        .expect("sync should fail");
    let message = error.to_string();
    assert!(message.contains("publication failed") || message.contains("origin"));
    assert_eq!(topic_head(&origin, "test"), raced);
    assert_ne!(
        topic_head(&PorchettaStore::load_at(&local_path).unwrap(), "test"),
        raced
    );
}
