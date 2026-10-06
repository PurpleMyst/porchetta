// Windows resolves known folders through the OS, not HOME overrides. These
// subprocess tests must never initialize or replace the user's real store.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

struct Fixture {
    home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir(fixture.home.path().join("data")).unwrap();
        let output = fixture.run(&["init"], None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_porchetta"))
            .args(args)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("LOCALAPPDATA", self.home.path().join("data"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    }

    fn view(&self) -> Value {
        let output = self.run(&["manifest", "view", "--json"], None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

// CLI-boundary ownership: protects transport, byte fidelity, CAS and persistence
// together; existing engine tests cannot catch CLI routing or stdin regressions.
#[test]
fn manifest_write_roundtrip_and_stale_revision_protection() {
    let f = Fixture::new();
    let original = f.view();
    assert_eq!(
        original["revision"],
        "c2a3590afcdedf4167ad916f63ca7d331f4681e9972d908d0505b77611761081"
    );
    let source = "return { topics = { git = { enabled = false, root = '.config/git', paths = {'config'}, should_include = function() error('must not run') end } } }\n";
    let updated = f.run(
        &[
            "manifest",
            "set",
            "--stdin",
            "--json",
            "--if-revision",
            original["revision"].as_str().unwrap(),
        ],
        Some(source),
    );
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let current = f.view();
    assert_eq!(current["source"], source);
    assert_ne!(current["revision"], original["revision"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&updated.stdout).unwrap()["revision"],
        current["revision"]
    );
    let stale = f.run(
        &[
            "manifest",
            "set",
            "--stdin",
            "--json",
            "--if-revision",
            original["revision"].as_str().unwrap(),
        ],
        Some("return { topics = {} }"),
    );
    assert!(!stale.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&stale.stdout).unwrap()["errors"][0]["code"],
        "revision_mismatch"
    );
    assert_eq!(f.view(), current);
    let raw = f.run(&["manifest", "view"], None);
    assert_eq!(raw.stdout, source.as_bytes());
    let inspect = f.run(&["manifest", "inspect", "--json"], None);
    assert!(inspect.status.success());
    let inspect: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(
        inspect["topics"],
        json!([{"name":"git", "enabled":false, "root":".config/git", "paths":["config"], "hooks":{"to_repo":false,"to_system":false,"should_include":true}}])
    );
}

// Invalid writes must preserve stored bytes; each case guards a distinct
// diagnostic contract, with real Lua evaluation rather than mocked validation.
#[test]
fn validation_reports_errors_without_modifying_the_store() {
    let f = Fixture::new();
    let original = f.view();
    for (source, code) in [
        ("return {", "lua_syntax_error"),
        ("error('bad config')", "lua_runtime_error"),
        (
            "return { topics = { git = { paths = {'../bad'} } } }",
            "invalid_path",
        ),
        (
            "return { topics = { git = { paths = {}, enabled = 'yes' } } }",
            "invalid_field",
        ),
    ] {
        for command in ["validate", "set"] {
            let output = f.run(&["manifest", command, "--stdin", "--json"], Some(source));
            assert!(!output.status.success());
            let diagnostic: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(diagnostic["errors"][0]["code"], code);
            if command == "validate" {
                assert_eq!(diagnostic["valid"], false);
            }
            if code == "invalid_path" {
                assert_eq!(diagnostic["errors"][0]["topic"], "git");
                assert_eq!(diagnostic["errors"][0]["field"], "paths");
            }
        }
    }
    assert_eq!(f.view(), original);
    let path = f.home.path().join("candidate.lua");
    std::fs::write(&path, "return { topics = {} }\n").unwrap();
    let path = path.to_str().unwrap();
    let valid = f.run(&["manifest", "validate", "--file", path, "--json"], None);
    assert!(valid.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&valid.stdout).unwrap(),
        json!({"valid":true,"errors":[]})
    );
    assert_eq!(f.view(), original);
    assert!(
        f.run(&["manifest", "set", "--file", path], None)
            .status
            .success()
    );
    for args in [
        vec!["manifest", "set"],
        vec!["manifest", "validate", "--file", path, "--stdin"],
    ] {
        assert_eq!(f.run(&args, None).status.code(), Some(2));
    }
}
