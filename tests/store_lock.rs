use std::sync::{Mutex, MutexGuard, PoisonError};

use camino::{Utf8Path, Utf8PathBuf};
use porchetta::store::PorchettaStore;
#[cfg(unix)]
use std::io::Write;

// Spawned subprocesses can briefly inherit lock descriptors from other test threads before exec.
static STORE_LOCK_TEST_LOCK: Mutex<()> = Mutex::new(());

fn store_lock_test_guard() -> MutexGuard<'static, ()> {
    STORE_LOCK_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn second_open_errors() {
    let _guard = store_lock_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let store_path = utf8(temp.path().join("store"));
    let _store = PorchettaStore::init_at(&store_path).unwrap();

    let error = PorchettaStore::load_at(&store_path).unwrap_err();

    assert!(
        error.to_string().contains("already in use"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn drop_then_reopen_succeeds() {
    let _guard = store_lock_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let store_path = utf8(temp.path().join("store"));
    let store = PorchettaStore::init_at(&store_path).unwrap();

    drop(store);

    let reopened = PorchettaStore::load_at(&store_path).unwrap();
    reopened.read_manifest().unwrap();
}

#[test]
fn different_stores_can_be_open_together() {
    let _guard = store_lock_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let first_path = utf8(temp.path().join("first"));
    let second_path = utf8(temp.path().join("second"));

    let first = PorchettaStore::init_at(&first_path).unwrap();
    let second = PorchettaStore::init_at(&second_path).unwrap();

    first.read_manifest().unwrap();
    second.read_manifest().unwrap();
}

#[cfg(unix)]
#[test]
fn symlink_and_target_contend_for_the_same_lock() {
    use std::os::unix::fs::symlink;

    let _guard = store_lock_test_guard();

    let temp = tempfile::tempdir().unwrap();
    let target_path = utf8(temp.path().join("store"));
    let symlink_path = utf8(temp.path().join("store-link"));
    let _store = PorchettaStore::init_at(&target_path).unwrap();
    symlink(&target_path, &symlink_path).unwrap();

    let error = PorchettaStore::load_at(&symlink_path).unwrap_err();

    assert!(
        error.to_string().contains("already in use"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn failed_constructor_releases_the_lock() {
    let _guard = store_lock_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let store_path = utf8(temp.path().join("store"));

    let error = PorchettaStore::load_at(&store_path).unwrap_err();
    assert!(!error.to_string().is_empty());

    let store = PorchettaStore::init_at(&store_path).unwrap();
    store.read_manifest().unwrap();
}

#[cfg(unix)]
#[test]
fn process_termination_releases_the_lock() {
    let _guard = store_lock_test_guard();
    let fixture = ProcessFixture::new();
    let store = PorchettaStore::init_at(&fixture.store_path).unwrap();
    drop(store);

    let (mut child, mut editor_stdin) = fixture.spawn_manifest_edit();
    wait_for_file(&fixture.ready_file);

    let contention_error = lock_error(&fixture.store_path);
    let status = child.terminate();
    let reopened = PorchettaStore::load_at(&fixture.store_path);

    writeln!(editor_stdin).unwrap();
    drop(editor_stdin);
    wait_for_file(&fixture.done_file);

    assert!(contention_error.contains("already in use"));
    assert!(
        !status.success(),
        "terminated process unexpectedly succeeded"
    );
    reopened.expect("store should reopen after the lock-owning process is terminated");
}

#[cfg(unix)]
#[test]
fn manifest_edit_holds_the_lock_while_the_editor_runs() {
    let _guard = store_lock_test_guard();
    let fixture = ProcessFixture::new();
    let store = PorchettaStore::init_at(&fixture.store_path).unwrap();
    drop(store);

    let (mut child, mut editor_stdin) = fixture.spawn_manifest_edit();
    wait_for_file(&fixture.ready_file);

    let contention_error = lock_error(&fixture.store_path);

    writeln!(editor_stdin).unwrap();
    drop(editor_stdin);
    let status = child.wait();
    let reopened = PorchettaStore::load_at(&fixture.store_path);

    assert!(contention_error.contains("already in use"));
    assert!(status.success(), "manifest edit failed with {status}");
    reopened.expect("store should reopen after manifest edit exits");
}

fn utf8(path: std::path::PathBuf) -> Utf8PathBuf {
    Utf8PathBuf::try_from(path).expect("test path should be valid UTF-8")
}

fn lock_error(store_path: &Utf8Path) -> String {
    match PorchettaStore::load_at(store_path) {
        Ok(store) => {
            drop(store);
            String::new()
        }
        Err(error) => error.to_string(),
    }
}

#[cfg(unix)]
const PROCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + PROCESS_TIMEOUT;
    while !path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

#[cfg(unix)]
fn wait_for_exit(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("failed to poll child process") {
            return Some(status);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
struct ChildGuard {
    child: std::process::Child,
}

#[cfg(unix)]
impl ChildGuard {
    fn wait(&mut self) -> std::process::ExitStatus {
        wait_for_exit(&mut self.child).expect("timed out waiting for porchetta process")
    }

    fn terminate(&mut self) -> std::process::ExitStatus {
        self.child
            .kill()
            .expect("failed to terminate porchetta process");
        self.wait()
    }
}

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }
}

#[cfg(unix)]
struct ProcessFixture {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    store_path: Utf8PathBuf,
    editor: std::path::PathBuf,
    ready_file: std::path::PathBuf,
    done_file: std::path::PathBuf,
}

#[cfg(unix)]
impl ProcessFixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let editor = temp.path().join("blocking-editor.sh");
        let ready_file = temp.path().join("editor-ready");
        let done_file = temp.path().join("editor-done");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            &editor,
            "#!/bin/sh\nset -eu\n: > \"$PORCHETTA_EDITOR_READY\"\nIFS= read -r _ || true\n: > \"$PORCHETTA_EDITOR_DONE\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&editor).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&editor, permissions).unwrap();
        let store_path = PorchettaStore::store_path_for(&home).unwrap();

        Self {
            _temp: temp,
            home,
            store_path,
            editor,
            ready_file,
            done_file,
        }
    }

    fn spawn_manifest_edit(&self) -> (ChildGuard, std::process::ChildStdin) {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_porchetta"))
            .args(["manifest", "edit"])
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("EDITOR", &self.editor)
            .env("PORCHETTA_EDITOR_READY", &self.ready_file)
            .env("PORCHETTA_EDITOR_DONE", &self.done_file)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn porchetta manifest edit");
        let stdin = child.stdin.take().expect("child stdin should be piped");

        (ChildGuard { child }, stdin)
    }
}
