#![cfg(unix)]

use std::process::{Command, Output};

use camino::Utf8PathBuf;
use porchetta::store::PorchettaStore;

struct Fixture {
    temp: tempfile::TempDir,
    home: Utf8PathBuf,
    store: Utf8PathBuf,
}

impl Fixture {
    fn new(manifest: &[u8]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::try_from(temp.path().join("home")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let store = PorchettaStore::store_path_for(home.as_std_path()).unwrap();
        PorchettaStore::init_at(&store)
            .unwrap()
            .write_manifest(manifest)
            .unwrap();
        Self { temp, home, store }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_porchetta"));
        command
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.temp.path().join("data"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn success(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

const MANIFEST: &[u8] = br#"return {
    topics = {
        pi_agent = {
            root = ".pi/agent",
            paths = {"AGENTS.md", "skills"},
        },
        agy = {
            root = ".gemini",
            paths = {"config.json"},
            symlinks = {
                ["GEMINI.md"] = porchetta.topic("pi_agent") / "AGENTS.md",
                ["skills"] = porchetta.topics.pi_agent / "skills",
            },
        },
    },
}"#;

#[test]
fn test_symlink_creation_and_idempotence() {
    let f = Fixture::new(MANIFEST);

    // Setup initial files
    std::fs::create_dir_all(f.home.join(".pi/agent/skills")).unwrap();
    std::fs::write(f.home.join(".pi/agent/AGENTS.md"), "pi agents\n").unwrap();
    std::fs::write(
        f.home.join(".pi/agent/skills/git.md"),
        "git skill content\n",
    )
    .unwrap();
    std::fs::create_dir_all(f.home.join(".gemini")).unwrap();
    std::fs::write(f.home.join(".gemini/config.json"), "{}\n").unwrap();

    // First sync creates the symlinks
    let output = f.success(&["sync", "--offline"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("link"));
    assert!(stderr.contains("GEMINI.md"));
    assert!(stderr.contains("skills"));

    // Verify GEMINI.md symlink
    let gemini_md = f.home.join(".gemini/GEMINI.md");
    let meta = std::fs::symlink_metadata(&gemini_md).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(&gemini_md).unwrap().to_str().unwrap(),
        "../.pi/agent/AGENTS.md"
    );
    assert_eq!(std::fs::read_to_string(&gemini_md).unwrap(), "pi agents\n");

    // Verify skills directory symlink
    let gemini_skills = f.home.join(".gemini/skills");
    let meta = std::fs::symlink_metadata(&gemini_skills).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(&gemini_skills)
            .unwrap()
            .to_str()
            .unwrap(),
        "../.pi/agent/skills"
    );
    assert_eq!(
        std::fs::read_to_string(gemini_skills.join("git.md")).unwrap(),
        "git skill content\n"
    );

    // Second sync is idempotent
    let output2 = f.success(&["sync", "--offline"]);
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(stderr2.contains("unchanged"));
}

#[test]
fn test_symlink_dry_run_does_not_create_links() {
    let f = Fixture::new(MANIFEST);

    std::fs::create_dir_all(f.home.join(".pi/agent")).unwrap();
    std::fs::write(f.home.join(".pi/agent/AGENTS.md"), "pi agents\n").unwrap();
    std::fs::create_dir_all(f.home.join(".gemini")).unwrap();
    std::fs::write(f.home.join(".gemini/config.json"), "{}\n").unwrap();

    let output = f.success(&["sync", "--dry-run", "--offline"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("link"));
    assert!(stderr.contains("GEMINI.md"));

    // Check link was NOT created on disk
    let gemini_md = f.home.join(".gemini/GEMINI.md");
    assert!(!gemini_md.exists());
    assert!(std::fs::symlink_metadata(&gemini_md).is_err());
}

#[test]
fn test_symlink_conflict_with_regular_file() {
    let f = Fixture::new(MANIFEST);

    std::fs::create_dir_all(f.home.join(".pi/agent")).unwrap();
    std::fs::write(f.home.join(".pi/agent/AGENTS.md"), "pi agents\n").unwrap();
    std::fs::create_dir_all(f.home.join(".gemini")).unwrap();
    std::fs::write(f.home.join(".gemini/config.json"), "{}\n").unwrap();

    // Create a real file where the symlink should go
    std::fs::write(f.home.join(".gemini/GEMINI.md"), "real file conflict\n").unwrap();

    let output = f.run(&["sync", "--offline"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("conflict"));

    // Real file is untouched!
    assert_eq!(
        std::fs::read_to_string(f.home.join(".gemini/GEMINI.md")).unwrap(),
        "real file conflict\n"
    );
}

#[test]
fn test_symlink_update_when_target_changes() {
    let f = Fixture::new(MANIFEST);

    std::fs::create_dir_all(f.home.join(".pi/agent")).unwrap();
    std::fs::write(f.home.join(".pi/agent/AGENTS.md"), "pi agents\n").unwrap();
    std::fs::write(f.home.join(".pi/agent/NEW.md"), "new target\n").unwrap();
    std::fs::create_dir_all(f.home.join(".gemini")).unwrap();
    std::fs::write(f.home.join(".gemini/config.json"), "{}\n").unwrap();

    f.success(&["sync", "--offline"]);

    // Update manifest pointing to NEW.md
    let new_manifest = br#"return {
        topics = {
            pi_agent = {
                root = ".pi/agent",
                paths = {"AGENTS.md", "NEW.md"},
            },
            agy = {
                root = ".gemini",
                paths = {"config.json"},
                symlinks = {
                    ["GEMINI.md"] = porchetta.topic("pi_agent") / "NEW.md",
                },
            },
        },
    }"#;
    PorchettaStore::load_at(&f.store)
        .unwrap()
        .write_manifest(new_manifest)
        .unwrap();

    let output = f.success(&["sync", "--offline"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("link"));

    let gemini_md = f.home.join(".gemini/GEMINI.md");
    assert_eq!(
        std::fs::read_link(&gemini_md).unwrap().to_str().unwrap(),
        "../.pi/agent/NEW.md"
    );
    assert_eq!(std::fs::read_to_string(&gemini_md).unwrap(), "new target\n");
}
