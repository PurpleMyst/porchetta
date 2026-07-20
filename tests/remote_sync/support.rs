use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use porchetta::engine::PorchettaEngine;
use porchetta::engine::resolver::{ConflictResolver, TreeConflictResolution};
use porchetta::store::{Branch, PorchettaStore};

// Git subprocesses can briefly inherit lock descriptors from other test threads before exec.
static REMOTE_SYNC_TEST_LOCK: Mutex<()> = Mutex::new(());

pub fn remote_sync_test_guard() -> MutexGuard<'static, ()> {
    REMOTE_SYNC_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub const BASIC_MANIFEST: &[u8] = br#"return {
    topics = {
        test = {
            root = ".config/test",
            paths = {"config.txt"},
        },
        same = {
            root = ".config/same",
            paths = {"config.txt"},
        },
    },
}
"#;

pub const RECONCILIATION_MANIFEST: &[u8] = br#"return { topics = {
    clean = { root = ".config/clean", paths = {"config.txt"} },
    conflict = { root = ".config/conflict", paths = {"config.txt"} },
    three = { root = ".config/three", paths = {"config.txt"} },
} }
"#;

pub const MANIFEST_RECONCILIATION_BASE: &[u8] = br#"return {
    topics = {
        alpha = {
            enabled = false,
            root = ".config/alpha",
            paths = {"a.txt"},
        },
        beta = {
            enabled = false,
            root = ".config/beta",
            paths = {"b.txt"},
        },
    },
}
"#;

pub const MANIFEST_RECONCILIATION_CLEAN_LOCAL: &[u8] = br#"return {
    topics = {
        alpha = {
            enabled = false,
            root = ".config/alpha-local",
            paths = {"a.txt"},
        },
        beta = {
            enabled = false,
            root = ".config/beta",
            paths = {"b.txt"},
        },
    },
}
"#;

pub const MANIFEST_RECONCILIATION_CLEAN_REMOTE: &[u8] = br#"return {
    topics = {
        alpha = {
            enabled = false,
            root = ".config/alpha",
            paths = {"a.txt"},
        },
        beta = {
            enabled = false,
            root = ".config/beta-remote",
            paths = {"b.txt"},
        },
    },
}
"#;

pub const MANIFEST_RECONCILIATION_CONFLICT_LOCAL: &[u8] =
    b"return { topics = { alpha = { enabled = false, root = '.config/local', paths = {'a.txt'} } } }\n";
pub const MANIFEST_RECONCILIATION_CONFLICT_REMOTE: &[u8] =
    b"return { topics = { alpha = { enabled = false, root = '.config/remote', paths = {'a.txt'} } } }\n";
pub const MANIFEST_RECONCILIATION_RESOLVED: &[u8] =
    b"return { topics = { alpha = { enabled = false, root = '.config/resolved', paths = {'a.txt'} } } }\n";
pub const MANIFEST_RECONCILIATION_INVALID: &[u8] =
    b"return { topics = { broken = { root = '../outside', paths = {'x'} } } }\n";

pub const DISABLED_TOPIC_MANIFEST: &[u8] =
    b"return { topics = { disabled = { enabled = false, root = '.config/disabled', paths = {'.'} } } }\n";
pub const SINGLE_TOPIC_MANIFEST: &[u8] =
    b"return { topics = { test = { root = '.config/test', paths = {'config.txt'} } } }\n";

pub struct ReplacingResolver {
    replacement: Vec<u8>,
    calls: Arc<AtomicUsize>,
}

impl ReplacingResolver {
    pub fn new(replacement: impl Into<Vec<u8>>) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                replacement: replacement.into(),
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

impl ConflictResolver for ReplacingResolver {
    fn resolve_tree_conflict(&self, _prompt: &str) -> Result<TreeConflictResolution> {
        Ok(TreeConflictResolution::KeepOurs)
    }

    fn choose_entry_kind(
        &self,
        ours: gix::objs::tree::EntryKind,
        _theirs: gix::objs::tree::EntryKind,
    ) -> Result<gix::objs::tree::EntryKind> {
        Ok(ours)
    }

    fn edit_blob(&self, content: &[u8], _path: &str) -> Result<Vec<u8>> {
        assert!(
            content.windows(7).any(|window| window == b"<<<<<<<"),
            "resolver should only be called with conflict markers"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.replacement.clone())
    }
}

pub fn path(root: &tempfile::TempDir, name: &str) -> Utf8PathBuf {
    Utf8PathBuf::try_from(root.path().join(name)).unwrap()
}

pub fn init_store(path: &Utf8Path, manifest: &[u8]) -> PorchettaStore {
    let store = PorchettaStore::init_at(path).unwrap();
    store.write_manifest(manifest).unwrap();
    store
}

pub fn topic_head(store: &PorchettaStore, topic: &str) -> gix::ObjectId {
    store
        .head(&Branch::topic(topic))
        .unwrap()
        .unwrap_or_else(|| panic!("topic/{topic} should have a head"))
}

pub fn topic_tree_oid(store: &PorchettaStore, topic: &str) -> gix::ObjectId {
    match store.head(&Branch::topic(topic)).unwrap() {
        Some(head) => store
            .repo()
            .find_object(head)
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .id()
            .into(),
        None => store.repo().empty_tree().id().into(),
    }
}

pub fn write_tree_with(
    store: &PorchettaStore,
    base_tree: gix::ObjectId,
    changes: &[(&str, &str)],
) -> gix::ObjectId {
    let mut editor = store.repo().edit_tree(base_tree).unwrap();
    for (file, content) in changes {
        let blob = store.repo().write_blob(content).unwrap();
        editor
            .upsert(*file, gix::objs::tree::EntryKind::Blob, blob)
            .unwrap();
    }
    editor.write().unwrap().into()
}

pub fn commit_topic_files(
    store: &PorchettaStore,
    topic: &str,
    changes: &[(&str, &str)],
    message: &str,
) -> gix::ObjectId {
    let parent = store.head(&Branch::topic(topic)).unwrap();
    let tree = write_tree_with(store, topic_tree_oid(store, topic), changes);
    let commit = store.commit_tree(tree, parent, message).unwrap();
    store
        .update_heads(&[(Branch::topic(topic), commit)])
        .unwrap();
    commit
}

pub fn commit_tree(
    store: &PorchettaStore,
    base_tree: gix::ObjectId,
    changes: &[(&str, &str)],
    parents: &[gix::ObjectId],
    message: &str,
) -> gix::ObjectId {
    let tree = write_tree_with(store, base_tree, changes);
    store
        .commit_tree(tree, parents.iter().copied(), message)
        .unwrap()
}

pub fn sync_with(
    store_path: &Utf8Path,
    home: &Utf8Path,
    resolver: impl ConflictResolver + 'static,
    dry_run: bool,
    offline: bool,
) -> Result<PorchettaEngine> {
    let store = PorchettaStore::load_at(store_path)?;
    let mut engine = PorchettaEngine::with_home(store, home.to_owned(), resolver);
    engine.sync(dry_run, offline)?;
    Ok(engine)
}

pub fn clone_store(source: &Utf8Path, destination: &Utf8Path) -> PorchettaStore {
    PorchettaStore::clone_from(source.as_str(), destination).unwrap()
}

pub fn read(path: &Utf8Path) -> String {
    fs::read_to_string(path).unwrap()
}

pub fn run_git(repo: &Utf8Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn install_reject_topic_hook(repo: &Utf8Path, topic: &str) -> Utf8PathBuf {
    let hook = repo.join("hooks/update");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"refs/heads/topic/{topic}\" ]; then\n  echo intentional rejection >&2\n  exit 1\nfi\nexit 0\n"
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();
    hook
}

pub fn install_receive_pack_race(
    temp: &tempfile::TempDir,
    origin_path: &Utf8Path,
    raced: gix::ObjectId,
) -> Utf8PathBuf {
    let wrapper = path(temp, "receive-pack-race.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ngit --git-dir='{origin_path}' update-ref refs/heads/topic/test {raced}\nexec git-receive-pack \"$@\"\n"
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    wrapper
}
