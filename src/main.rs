use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};

use porchetta::engine::resolver::{ConflictResolver, TreeConflictResolution};
use porchetta::store::PorchettaStore;
use porchetta::ui;

mod manifest_cli;

/// Interactive resolver that prompts the user via the terminal.
struct InteractiveResolver;

impl ConflictResolver for InteractiveResolver {
    fn resolve_tree_conflict(&self, prompt: &str) -> anyhow::Result<TreeConflictResolution> {
        let choice = inquire::Select::new(
            prompt,
            vec!["Keep local (ours)", "Keep remote (theirs)", "Abort sync"],
        )
        .prompt()
        .context("User canceled conflict resolution")?;

        match choice {
            "Keep local (ours)" => Ok(TreeConflictResolution::KeepOurs),
            "Keep remote (theirs)" => Ok(TreeConflictResolution::KeepTheirs),
            "Abort sync" => Ok(TreeConflictResolution::Abort),
            _ => anyhow::bail!("Invalid conflict resolution choice"),
        }
    }

    fn choose_entry_kind(
        &self,
        ours: gix::objs::tree::EntryKind,
        theirs: gix::objs::tree::EntryKind,
    ) -> anyhow::Result<gix::objs::tree::EntryKind> {
        let choice = inquire::Select::new(
            "Local and remote entries have different kinds. Which should be used?",
            vec![format!("Local ({ours:?})"), format!("Remote ({theirs:?})")],
        )
        .prompt()
        .context("User canceled entry kind selection")?;

        if choice.starts_with("Local") {
            Ok(ours)
        } else {
            Ok(theirs)
        }
    }

    fn edit_blob(&self, content: &[u8], path: &str) -> anyhow::Result<Vec<u8>> {
        let path = Utf8Path::new(path);
        let file_stem = path.file_stem().unwrap_or("porchetta_conflict");
        let extension = path.extension().unwrap_or("tmp");
        let prefix = format!("{file_stem}-porchetta-conflict-");
        let suffix = format!(".{extension}");

        let mut temp_file = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(&suffix)
            .tempfile()
            .context("Failed to create temp file for conflict editing")?;
        std::io::Write::write_all(&mut temp_file, content)
            .context("Failed to write conflict content to temp file")?;

        let editor = porchetta::util::get_editor().context("Failed to determine editor")?;
        let status = std::process::Command::new(&editor)
            .arg(temp_file.path())
            .status()
            .context("Failed to launch editor")?;
        if !status.success() {
            anyhow::bail!("Editor exited with non-zero status");
        }

        std::fs::read(temp_file.path()).context("Failed to read edited conflict content")
    }
}

#[derive(Parser)]
#[command(
    name = "porchetta",
    about = "Dotfile manager with topics and conflict resolution"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Suppress routine output (warnings and errors remain visible)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Control colors in the report
    #[arg(long, global = true, default_value = "auto", value_parser = ["auto", "always", "never"])]
    color: String,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Initialize a new Porchetta store
    Init,
    /// Manage the manifest
    #[command(
        after_help = "Examples:\n  porchetta manifest edit\n  porchetta manifest view > manifest.lua\n  porchetta manifest validate --file manifest.lua\n  porchetta manifest set --file manifest.lua\n\nAgent workflow:\n  Read source and revision with `manifest view --json`.\n  Edit the Lua source, then validate it with `manifest validate --file manifest.lua --json`.\n  Save with `manifest set --file manifest.lua --if-revision <SHA256> --json`.\n  On revision_mismatch, reread and reconcile changes before retrying.\n\nThese commands do not sync managed files or remotes.\nJSON results go to stdout; logs and error reporting go to stderr.\nExit status: 0 success, 1 operation/validation failure, 2 CLI usage error.\nUsage errors are not JSON. Use `<command> --help` for details."
    )]
    Manifest {
        #[command(subcommand)]
        command: ManifestCommand,
    },
    /// Apply the manifest to the local machine
    Sync {
        /// Preview changes without applying them
        #[arg(long)]
        dry_run: bool,
        /// Do not fetch or push from the remote
        #[arg(long)]
        offline: bool,
        /// Limit synchronization to the named topics (repeatable)
        #[arg(short = 't', long = "topic", value_name = "NAME")]
        topics: Vec<String>,
    },
    /// Clone a Porchetta store from a remote URL
    Clone {
        /// URL of the remote Porchetta store
        url: String,
    },
    /// Manage store remotes
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Migrate configuration from another dotfile manager
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
}

#[derive(Subcommand, Clone)]
enum RemoteCommand {
    /// Add a remote
    Add {
        /// Name for the remote
        name: String,
        /// URL of the remote Porchetta store
        url: String,
    },
    /// List configured remotes
    List,
    /// Remove a remote
    Remove {
        /// Name of the remote to remove
        name: String,
    },
}

#[derive(Subcommand, Clone)]
pub(crate) enum ManifestCommand {
    /// Open the manifest in your default editor
    #[command(
        after_help = "Example: porchetta manifest edit\n\nUses EDITOR, then VISUAL, then the default editor. Validates the edited Lua\nbefore saving. For non-interactive changes, use `manifest set` instead."
    )]
    Edit,
    /// Print the current manifest
    #[command(
        after_help = "Examples:\n  porchetta manifest view > manifest.lua\n  porchetta manifest view --json\n\nDefault output is the exact stored Lua source, without headers.\nJSON output contains `source` (Lua text) and `revision` (lowercase SHA-256\nof the exact source bytes). Pass that revision to `manifest set --if-revision`\nto prevent overwriting changes made since this read. This does not evaluate Lua."
    )]
    View {
        /// Return source and its SHA-256 revision as JSON
        #[arg(long)]
        json: bool,
    },
    /// Replace the manifest without opening an editor
    #[command(
        after_help = "Examples:\n  porchetta manifest set --file manifest.lua\n  porchetta manifest set --stdin < manifest.lua\n  porchetta manifest set --file manifest.lua --if-revision <SHA256> --json\n\nChoose exactly one input: --file or --stdin. Evaluates and validates Lua\nbefore saving; invalid input leaves the stored manifest unchanged.\nLua is not sandboxed. Hooks are not executed by validation.\n\n--if-revision compares the current source's SHA-256 under the store lock.\nOn mismatch, reread with `manifest view --json`, reconcile, and retry.\nWithout --if-revision, the replacement is unconditional. No sync is performed.\n\nJSON success: {\"revision\":\"...\"}. JSON failure: an `errors` array containing\n`code` and `message`; stale writes use code `revision_mismatch`.\nExit status: 0 success, 1 failure, 2 usage error (not JSON)."
    )]
    Set {
        #[command(flatten)]
        input: manifest_cli::ManifestInput,
        /// Reject writes if the current source has a different SHA-256
        #[arg(long, value_name = "SHA256")]
        if_revision: Option<String>,
        /// Return the saved revision or structured errors as JSON
        #[arg(long)]
        json: bool,
    },
    /// Evaluate and validate Lua without saving or executing hooks
    #[command(
        after_help = "Examples:\n  porchetta manifest validate --file manifest.lua\n  porchetta manifest validate --stdin --json < manifest.lua\n\nChoose exactly one input: --file or --stdin. No initialized store is needed.\nEvaluates Lua on this machine and checks manifest fields and paths using\nthe current home directory. Does not save, sync, or execute hooks.\nLua is not sandboxed; validation does not guarantee hooks will succeed.\n\nJSON success: {\"valid\":true,\"errors\":[]}. Validation failure returns\n`valid: false` and errors with `code`, `message`, and nullable `topic`,\n`field`, and `value`. Input/read failures use code `operation_failed`.\nValidation codes: lua_syntax_error, lua_runtime_error, invalid_field, invalid_path.\nExit status: 0 valid, 1 failure, 2 usage error (not JSON)."
    )]
    Validate {
        #[command(flatten)]
        input: manifest_cli::ManifestInput,
        /// Return validity and structured diagnostics as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect the configuration evaluated on this machine
    #[command(
        after_help = "Examples:\n  porchetta manifest inspect\n  porchetta manifest inspect --json\n\nEvaluates the stored Lua on this machine. Returns `revision`, sorted `topics`\n(name, enabled, normalized root, paths, hook presence), and manifest-level\nhook presence. Disabled topics are included. Hooks are not executed.\nLua is not sandboxed. This is configuration inspection, not a filesystem preview.\n\nDefault output is pretty-printed JSON; --json emits compact JSON and structured\nerrors. Evaluated output cannot round-trip expressions, comments, or functions.\nUse `manifest view` to obtain editable Lua source.\nExit status: 0 success, 1 failure, 2 usage error (not JSON)."
    )]
    Inspect {
        /// Emit compact JSON results and structured errors
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
enum MigrateCommand {
    /// Import topics and paths from a chezmoi source directory
    Chezmoi {
        /// Path to chezmoi source directory (default: ~/.local/share/chezmoi)
        #[arg(long, value_name = "DIR")]
        source_dir: Option<Utf8PathBuf>,
        /// Do not prompt for confirmation before overwriting the manifest
        #[arg(long)]
        yes: bool,
    },
}

const PORCHETTA_LUALS_STUB: &str = include_str!("luals/porchetta.lua");

const MANIFEST_LUALS_CONFIG: &str = r#"{
  "runtime": { "version": "Lua 5.4" },
  "workspace": {
    "library": [".lua-defs"],
    "checkThirdParty": false
  }
}
"#;

fn edit_manifest_with_workspace(editor: &str, content: &[u8]) -> Result<Vec<u8>> {
    let workspace = tempfile::Builder::new()
        .prefix("porchetta-manifest-edit-")
        .tempdir()
        .context("Failed to create manifest edit workspace")?;
    let workspace_path = workspace.path();
    let defs_dir = workspace_path.join(".lua-defs");

    std::fs::create_dir(&defs_dir).context("Failed to create LuaLS definitions directory")?;
    std::fs::write(workspace_path.join("manifest.lua"), content)
        .context("Failed to write manifest to edit workspace")?;
    std::fs::write(workspace_path.join(".luarc.json"), MANIFEST_LUALS_CONFIG)
        .context("Failed to write LuaLS workspace config")?;
    std::fs::write(defs_dir.join("porchetta.lua"), PORCHETTA_LUALS_STUB)
        .context("Failed to write Porchetta LuaLS definitions")?;

    let status = std::process::Command::new(editor)
        .current_dir(workspace_path)
        .arg("manifest.lua")
        .status()
        .context("Failed to launch editor")?;
    if !status.success() {
        anyhow::bail!("Editor exited with non-zero status");
    }

    std::fs::read(workspace_path.join("manifest.lua")).context("Failed to read edited manifest")
}

fn init_logging() -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .context("Could not determine local data directory")?
        .join("porchetta")
        .join("logs");

    flexi_logger::Logger::try_with_str("trace")?
        .log_to_file(flexi_logger::FileSpec::default().directory(log_dir))
        .duplicate_to_stderr(flexi_logger::Duplicate::Warn)
        .append()
        .start()
        .context("Failed to initialize logger")?;

    Ok(())
}

fn handle_remote(command: RemoteCommand) -> Result<()> {
    let store = PorchettaStore::load().context("Failed to load store")?;
    match command {
        RemoteCommand::Add { name, url } => {
            store
                .add_remote(&name, &url)
                .with_context(|| format!("Failed to add remote '{name}'"))?;
            ui::success(&format!("Added remote '{name}'"));
        }
        RemoteCommand::List => {
            for remote in store.remotes().context("Failed to list remotes")? {
                println!("{}: {}", remote.name(), remote.url());
            }
        }
        RemoteCommand::Remove { name } => {
            store
                .remove_remote(&name)
                .with_context(|| format!("Failed to remove remote '{name}'"))?;
            ui::success(&format!("Removed remote '{name}'"));
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    ui::configure(cli.quiet, &cli.color);
    init_logging()?;

    match cli.command {
        Command::Init => {
            let _store = PorchettaStore::init().context("Failed to initialize store")?;
            ui::success("Initialized Porchetta store");
            ui::muted(&format!("  {}", PorchettaStore::store_path()?));
        }
        Command::Manifest {
            command: ManifestCommand::Edit,
        } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let editor = porchetta::util::get_editor().context("Failed to determine editor")?;
            store
                .edit_manifest(|content| edit_manifest_with_workspace(&editor, content))
                .context("Failed to edit manifest")?;
            ui::success("Manifest updated");
        }
        Command::Manifest { command } => manifest_cli::run(command)?,
        Command::Sync {
            dry_run,
            offline,
            topics,
        } => {
            PorchettaStore::load()
                .context("Failed to load store")?
                .sync_topics(dry_run, offline, &topics, &InteractiveResolver)
                .context("Failed to sync")?;
        }
        Command::Remote { command } => handle_remote(command)?,

        Command::Clone { url } => {
            let store_path =
                PorchettaStore::store_path().context("Failed to determine store path")?;
            let _store = PorchettaStore::clone_from(&url, &store_path)
                .with_context(|| format!("Failed to clone from {url}"))?;
            ui::success("Cloned Porchetta store");
            ui::muted(&format!("  {store_path}"));
            ui::info("Run `porchetta sync` to apply the configuration to this machine.");
        }
        Command::Migrate {
            command: MigrateCommand::Chezmoi { source_dir, yes },
        } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let source_dir = match source_dir {
                Some(dir) => dir,
                None => porchetta::util::home_dir()?.join(".local/share/chezmoi"),
            };
            porchetta::chezmoi::migrate(&store, &source_dir, yes)
                .context("Failed to migrate from chezmoi")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_edit_command() {
        let cli = Cli::try_parse_from(["porchetta", "manifest", "edit"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Manifest {
                command: ManifestCommand::Edit
            }
        ));
    }

    #[test]
    fn parses_manifest_view_command() {
        let cli = Cli::try_parse_from(["porchetta", "manifest", "view"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Manifest {
                command: ManifestCommand::View { json: false }
            }
        ));
    }

    #[test]
    fn rejects_top_level_edit_command() {
        let Err(err) = Cli::try_parse_from(["porchetta", "edit"]) else {
            panic!("top-level edit command should be rejected")
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn parses_remote_add_command() {
        let cli = Cli::try_parse_from([
            "porchetta",
            "remote",
            "add",
            "backup",
            "https://example.com/store.git",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Remote {
                command: RemoteCommand::Add { name, url }
            } if name == "backup" && url == "https://example.com/store.git"
        ));
    }

    #[test]
    fn parses_remote_list_command() {
        let cli = Cli::try_parse_from(["porchetta", "remote", "list"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Remote {
                command: RemoteCommand::List
            }
        ));
    }

    #[test]
    fn parses_remote_remove_command() {
        let cli = Cli::try_parse_from(["porchetta", "remote", "remove", "backup"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Remote {
                command: RemoteCommand::Remove { name }
            } if name == "backup"
        ));
    }
}
