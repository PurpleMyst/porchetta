use camino::Utf8PathBuf;
use porchetta::engine::PorchettaEngine;
use porchetta::store::PorchettaStore;

#[test]
fn test_sync_fetches_and_fast_forwards_topic() {
    let temp = tempfile::tempdir().unwrap();
    let remote_path = Utf8PathBuf::try_from(temp.path().join("remote")).unwrap();
    let local_path = Utf8PathBuf::try_from(temp.path().join("local")).unwrap();
    let remote_home = Utf8PathBuf::try_from(temp.path().join("remote_home")).unwrap();
    let local_home = Utf8PathBuf::try_from(temp.path().join("local_home")).unwrap();

    // Set up remote store with an initial topic commit.
    let remote_store = PorchettaStore::init_at(&remote_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    remote_store.write_manifest(manifest).unwrap();

    let remote_topic_dir = remote_home.join(".config/test");
    std::fs::create_dir_all(&remote_topic_dir).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "remote-content").unwrap();

    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Clone the remote store locally.
    let local_store = PorchettaStore::clone_from(remote_path.as_str(), &local_path).unwrap();

    // Local first sync to establish the system head.
    let local_topic_dir = local_home.join(".config/test");
    std::fs::create_dir_all(&local_topic_dir).unwrap();
    std::fs::write(local_topic_dir.join("config.txt"), "remote-content").unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, false).unwrap();

    // Modify remote topic.
    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "remote-modified").unwrap();
    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Local sync should fetch and apply remote changes.
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, false).unwrap();

    let content = std::fs::read_to_string(local_topic_dir.join("config.txt")).unwrap();
    assert_eq!(content, "remote-modified");

    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    assert_eq!(
        local_store.get_topic_head("test").unwrap(),
        remote_store.get_topic_head("test").unwrap(),
        "local topic head should match remote after fast-forward"
    );
}

#[test]
fn test_sync_pushes_topic_and_system_heads() {
    let temp = tempfile::tempdir().unwrap();
    let remote_path = Utf8PathBuf::try_from(temp.path().join("remote")).unwrap();
    let local_path = Utf8PathBuf::try_from(temp.path().join("local")).unwrap();
    let remote_home = Utf8PathBuf::try_from(temp.path().join("remote_home")).unwrap();
    let local_home = Utf8PathBuf::try_from(temp.path().join("local_home")).unwrap();

    // Set up remote store with an initial topic commit.
    let remote_store = PorchettaStore::init_at(&remote_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    remote_store.write_manifest(manifest).unwrap();

    let remote_topic_dir = remote_home.join(".config/test");
    std::fs::create_dir_all(&remote_topic_dir).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "initial").unwrap();

    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Clone the remote store locally.
    let local_store = PorchettaStore::clone_from(remote_path.as_str(), &local_path).unwrap();

    // Local first sync to establish the system head.
    let local_topic_dir = local_home.join(".config/test");
    std::fs::create_dir_all(&local_topic_dir).unwrap();
    std::fs::write(local_topic_dir.join("config.txt"), "initial").unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, false).unwrap();

    // Modify local filesystem.
    std::fs::write(local_topic_dir.join("config.txt"), "local-modified").unwrap();

    let old_remote_topic_head = PorchettaStore::load_at(&remote_path)
        .unwrap()
        .get_topic_head("test")
        .unwrap();

    // Local sync should push changes to remote.
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, false).unwrap();

    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    let new_remote_topic_head = remote_store.get_topic_head("test").unwrap();
    assert!(
        new_remote_topic_head != old_remote_topic_head,
        "remote topic head should have changed after push"
    );

    let hostname = ::hostname::get().unwrap().to_string_lossy().into_owned();
    let remote_system_head = remote_store
        .get_topic_hostname_head("test", &hostname)
        .unwrap();
    assert_eq!(
        new_remote_topic_head, remote_system_head,
        "remote system head should match topic head after push"
    );
}

#[test]
fn test_sync_aborts_on_diverged_topic() {
    let temp = tempfile::tempdir().unwrap();
    let remote_path = Utf8PathBuf::try_from(temp.path().join("remote")).unwrap();
    let local_path = Utf8PathBuf::try_from(temp.path().join("local")).unwrap();
    let remote_home = Utf8PathBuf::try_from(temp.path().join("remote_home")).unwrap();
    let local_home = Utf8PathBuf::try_from(temp.path().join("local_home")).unwrap();

    // Set up remote store with an initial topic commit.
    let remote_store = PorchettaStore::init_at(&remote_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    remote_store.write_manifest(manifest).unwrap();

    let remote_topic_dir = remote_home.join(".config/test");
    std::fs::create_dir_all(&remote_topic_dir).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "initial").unwrap();

    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Clone the remote store locally.
    let local_store = PorchettaStore::clone_from(remote_path.as_str(), &local_path).unwrap();

    // Local first sync to establish the system head.
    let local_topic_dir = local_home.join(".config/test");
    std::fs::create_dir_all(&local_topic_dir).unwrap();
    std::fs::write(local_topic_dir.join("config.txt"), "initial").unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Diverge local topic.
    std::fs::write(local_topic_dir.join("config.txt"), "local-diverged").unwrap();
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Diverge remote topic.
    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "remote-diverged").unwrap();
    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Local sync should abort because topics diverged.
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    let result = engine.sync(false, false, false);
    assert!(result.is_err(), "sync should abort on diverged topic");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("diverged"),
        "error should mention divergence: {err}"
    );
}

#[test]
fn test_sync_aborts_on_diverged_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let remote_path = Utf8PathBuf::try_from(temp.path().join("remote")).unwrap();
    let local_path = Utf8PathBuf::try_from(temp.path().join("local")).unwrap();
    let remote_home = Utf8PathBuf::try_from(temp.path().join("remote_home")).unwrap();
    let local_home = Utf8PathBuf::try_from(temp.path().join("local_home")).unwrap();

    // Set up remote store.
    let remote_store = PorchettaStore::init_at(&remote_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    remote_store.write_manifest(manifest).unwrap();

    let remote_topic_dir = remote_home.join(".config/test");
    std::fs::create_dir_all(&remote_topic_dir).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "initial").unwrap();

    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Clone the remote store locally.
    let _local_store = PorchettaStore::clone_from(remote_path.as_str(), &local_path).unwrap();

    // Diverge manifest on both sides.
    let local_manifest = b"return { topics = { test = { root = '.config/test', paths = {'config.txt', 'extra.txt'} } } }\n";
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    local_store.write_manifest(local_manifest).unwrap();

    let remote_manifest = b"return { topics = { test = { root = '.config/test', paths = {'config.txt', 'other.txt'} } } }\n";
    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    remote_store.write_manifest(remote_manifest).unwrap();

    // Local sync should abort because manifest diverged.
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    let result = engine.sync(false, false, false);
    assert!(result.is_err(), "sync should abort on diverged manifest");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("diverged"),
        "error should mention divergence: {err}"
    );
}

#[test]
fn test_sync_offline_skips_fetch_and_push() {
    let temp = tempfile::tempdir().unwrap();
    let remote_path = Utf8PathBuf::try_from(temp.path().join("remote")).unwrap();
    let local_path = Utf8PathBuf::try_from(temp.path().join("local")).unwrap();
    let remote_home = Utf8PathBuf::try_from(temp.path().join("remote_home")).unwrap();
    let local_home = Utf8PathBuf::try_from(temp.path().join("local_home")).unwrap();

    // Set up remote store with an initial topic commit.
    let remote_store = PorchettaStore::init_at(&remote_path).unwrap();
    let manifest =
        b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";
    remote_store.write_manifest(manifest).unwrap();

    let remote_topic_dir = remote_home.join(".config/test");
    std::fs::create_dir_all(&remote_topic_dir).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "initial").unwrap();

    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Clone the remote store locally.
    let local_store = PorchettaStore::clone_from(remote_path.as_str(), &local_path).unwrap();

    // Local first sync to establish the system head.
    let local_topic_dir = local_home.join(".config/test");
    std::fs::create_dir_all(&local_topic_dir).unwrap();
    std::fs::write(local_topic_dir.join("config.txt"), "initial").unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    // Modify remote topic.
    let remote_store = PorchettaStore::load_at(&remote_path).unwrap();
    std::fs::write(remote_topic_dir.join("config.txt"), "remote-modified").unwrap();
    let mut engine = PorchettaEngine::with_home(
        remote_store,
        remote_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    let cloned_topic_head = PorchettaStore::load_at(&local_path)
        .unwrap()
        .get_topic_head("test")
        .unwrap();

    // Local sync with offline=true should NOT fetch remote changes.
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let local_topic_head_after_sync = local_store.get_topic_head("test").unwrap();
    assert_eq!(
        local_topic_head_after_sync, cloned_topic_head,
        "offline sync should not fetch remote changes"
    );

    // Modify local filesystem and sync offline again.
    std::fs::write(local_topic_dir.join("config.txt"), "local-modified").unwrap();
    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let mut engine = PorchettaEngine::with_home(
        local_store,
        local_home.clone(),
        porchetta::engine::resolver::PanickingResolver,
    );
    engine.sync(false, false, true).unwrap();

    let remote_topic_head_before = PorchettaStore::load_at(&remote_path)
        .unwrap()
        .get_topic_head("test")
        .unwrap();

    let local_store = PorchettaStore::load_at(&local_path).unwrap();
    let local_topic_head_after = local_store.get_topic_head("test").unwrap();

    assert_ne!(
        local_topic_head_after, cloned_topic_head,
        "local sync should have created a new commit"
    );

    let remote_topic_head_after = PorchettaStore::load_at(&remote_path)
        .unwrap()
        .get_topic_head("test")
        .unwrap();
    assert_eq!(
        remote_topic_head_before, remote_topic_head_after,
        "offline sync should not push local changes to remote"
    );
}
