#![cfg(unix)]

use std::process::{Command, Output};

use camino::Utf8PathBuf;
use porchetta::store::{Branch, PorchettaStore};

const MANIFEST: &[u8] = br"return { topics = {
    config = { root = '.config/demo', paths = {'.'} },
    shell = { paths = {'.bashrc'} },
} }";

struct Fixture {
    temp: tempfile::TempDir,
    home: Utf8PathBuf,
    store: Utf8PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::try_from(temp.path().join("home")).unwrap();
        std::fs::create_dir_all(home.join(".config/demo")).unwrap();
        let store = PorchettaStore::store_path_for(home.as_std_path()).unwrap();
        PorchettaStore::init_at(&store)
            .unwrap()
            .write_manifest(MANIFEST)
            .unwrap();
        for name in [
            "local-update",
            "local-delete",
            "store-update",
            "store-delete",
        ] {
            std::fs::write(home.join(".config/demo").join(name), "base\n").unwrap();
        }
        std::fs::write(home.join(".bashrc"), "shell\n").unwrap();
        let fixture = Self { temp, home, store };
        fixture.success(&["sync", "--offline"]);
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_porchetta"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.temp.path().join("data"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("NO_COLOR")
            .env("TERM", "xterm-256color");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "sync commentary belongs on stderr"
        );
        String::from_utf8(output.stderr).unwrap()
    }

    fn edit_store(&self, updates: &[(&str, &str)], deletes: &[&str]) {
        let store = PorchettaStore::load_at(&self.store).unwrap();
        let head = store.head(&Branch::topic("config")).unwrap().unwrap();
        let tree = store
            .repo()
            .find_object(head)
            .unwrap()
            .peel_to_tree()
            .unwrap();
        let mut editor = store.repo().edit_tree(tree.id()).unwrap();
        for (name, content) in updates {
            let blob = store.repo().write_blob(content).unwrap();
            editor
                .upsert(*name, gix::objs::tree::EntryKind::Blob, blob)
                .unwrap();
        }
        for name in deletes {
            editor.remove(*name).unwrap();
        }
        let tree = editor.write().unwrap();
        store
            .commit_topic_tree("config", "other-host", tree, "Change stored files")
            .unwrap();
    }

    fn mixed_changes(&self) {
        std::fs::write(self.home.join(".config/demo/local-update"), "local\n").unwrap();
        std::fs::remove_file(self.home.join(".config/demo/local-delete")).unwrap();
        std::fs::write(self.home.join(".config/demo/local-add"), "new local\n").unwrap();
        self.edit_store(
            &[("store-update", "stored\n"), ("store-add", "new stored\n")],
            &["store-delete"],
        );
    }
}

#[test]
fn sync_and_preview_report_each_change_in_the_correct_direction() {
    let fixture = Fixture::new();
    fixture.mixed_changes();
    for dry_run in [true, false] {
        let args = if dry_run {
            vec!["sync", "--offline", "--dry-run"]
        } else {
            vec!["sync", "--offline"]
        };
        let report = fixture.success(&args);
        assert!(
            !report.contains('\x1b'),
            "redirected output must not contain ANSI codes"
        );
        let capture = if dry_run { "Would capture" } else { "Captured" };
        let apply = if dry_run { "Would apply" } else { "Applied" };
        assert!(
            report.contains(&format!("{capture} 3 file changes into store")),
            "{report}"
        );
        assert!(
            report.contains(&format!("{apply} 3 file changes to machine")),
            "{report}"
        );
        let changes: Vec<Vec<&str>> = report
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .filter(|words| matches!(words.first(), Some(&"Store" | &"Machine")))
            .collect();
        for (destination, action, path) in [
            ("Store", "add", "local-add"),
            ("Store", "update", "local-update"),
            ("Store", "delete", "local-delete"),
            ("Machine", "add", "~/.config/demo/store-add"),
            ("Machine", "update", "~/.config/demo/store-update"),
            ("Machine", "delete", "~/.config/demo/store-delete"),
        ] {
            assert!(
                changes.contains(&vec![destination, action, path]),
                "{report}"
            );
        }
        assert_eq!(changes.len(), 6, "{report}");
        assert!(
            report
                .lines()
                .any(|line| line.contains("shell") && line.contains("Unchanged"))
        );
        assert!(report.contains("2 topics checked · 1 changed · 1 unchanged"));
        assert!(report.contains("Store: 3 file changes · machine: 3 file changes"));
        assert!(report.contains(if dry_run {
            "Preview complete"
        } else {
            "Local sync complete"
        }));
    }
}

#[test]
fn color_and_quiet_controls_preserve_command_data() {
    let fixture = Fixture::new();
    fixture.mixed_changes();
    let colored = fixture
        .command(&["sync", "--offline", "--dry-run", "--color", "always"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(colored.status.success());
    let colored = String::from_utf8(colored.stderr).unwrap();
    // Store and machine labels have distinct colors; additions and deletions stand out.
    for escape in [
        "\x1b[34mStore",
        "\x1b[36mMachine",
        "\x1b[32madd",
        "\x1b[33;1mdelete",
    ] {
        assert!(colored.contains(escape), "missing {escape:?}: {colored:?}");
    }
    assert!(
        !fixture
            .success(&["sync", "--offline", "--dry-run", "--color", "never"])
            .contains('\x1b')
    );
    assert!(
        fixture
            .success(&["sync", "--offline", "--quiet"])
            .is_empty()
    );
    let manifest = fixture.run(&["manifest", "view", "--quiet"]);
    assert!(manifest.status.success());
    assert_eq!(manifest.stdout, MANIFEST);
    assert!(manifest.stderr.is_empty());
}

#[test]
fn conflicting_preview_is_incomplete_even_in_quiet_mode() {
    let fixture = Fixture::new();
    std::fs::write(fixture.home.join(".config/demo/local-update"), "local\n").unwrap();
    fixture.edit_store(&[("local-update", "stored\n")], &[]);
    for quiet in [false, true] {
        let mut args = vec!["sync", "--offline", "--dry-run"];
        if quiet {
            args.push("--quiet");
        }
        let report = fixture.success(&args);
        assert!(
            report.contains("config") && report.contains("Needs resolution"),
            "{report}"
        );
        assert!(
            report.contains("Preview incomplete · 1 conflict(s)"),
            "{report}"
        );
        assert!(report.contains("Run `porchetta sync`"));
        assert!(!report.contains("Preview complete") && !report.contains("Local sync complete"));
    }
}

#[test]
fn failed_apply_reports_capture_without_claiming_machine_success() {
    let fixture = Fixture::new();
    fixture.mixed_changes();
    let store = PorchettaStore::load_at(&fixture.store).unwrap();
    store
        .write_manifest(
            br"return { topics = {
        config = { root = '.config/demo', paths = {'.'},
            to_system = function() error('intentional apply failure') end },
    } }",
        )
        .unwrap();
    drop(store);
    let output = fixture.run(&["sync", "--offline"]);
    assert!(!output.status.success());
    let report = String::from_utf8(output.stderr).unwrap();
    assert!(
        report.contains("Captured 3 file changes into store"),
        "{report}"
    );
    assert!(
        report.contains("Store    add") && report.contains("local-add"),
        "{report}"
    );
    assert!(
        report.contains("incomplete") && report.contains("intentional apply failure"),
        "{report}"
    );
    assert!(
        !report.contains("Applied") && !report.contains("Local sync complete"),
        "{report}"
    );
}

#[test]
fn publication_failure_is_distinct_from_local_success_and_can_be_retried() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let remote_path = Utf8PathBuf::try_from(fixture.temp.path().join("remote")).unwrap();
    drop(PorchettaStore::init_at(&remote_path).unwrap());
    let hook = remote_path.join("hooks/pre-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\necho publication-rejected >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    PorchettaStore::load_at(&fixture.store)
        .unwrap()
        .add_remote("backup", remote_path.as_str())
        .unwrap();
    std::fs::write(fixture.home.join(".bashrc"), "changed shell\n").unwrap();

    let output = fixture.run(&["sync"]);
    assert!(!output.status.success());
    let report = String::from_utf8(output.stderr).unwrap();
    assert!(
        report.contains("Captured 1 file change into store"),
        "{report}"
    );
    assert!(report.contains("Local sync complete"), "{report}");
    assert!(
        report.contains("Publication incomplete. Local changes are retained."),
        "{report}"
    );
    assert!(
        report.contains("backup") && report.contains("publication-rejected"),
        "{report}"
    );
    assert!(report.contains("Run `porchetta sync` to retry publication."));

    std::fs::remove_file(hook).unwrap();
    let retry = fixture.success(&["sync"]);
    assert!(retry.contains("Published"), "{retry}");
    assert!(!retry.contains("Publication incomplete"), "{retry}");
    let local = PorchettaStore::load_at(&fixture.store).unwrap();
    let remote = PorchettaStore::load_at(&remote_path).unwrap();
    assert_eq!(
        local.head(&Branch::topic("shell")).unwrap(),
        remote.head(&Branch::topic("shell")).unwrap()
    );
}
