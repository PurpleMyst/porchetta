use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};

use porchetta::engine::PorchettaEngine;
use porchetta::engine::resolver::{ConflictResolver, TreeConflictResolution};
use porchetta::store::PorchettaStore;
use porchetta::ui;

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
        _prompt: &str,
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

    /// Decrease output verbosity
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Initialize a new Porchetta store
    Init,
    /// Manage the manifest
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
    },
    /// Clone a Porchetta store from a remote URL
    Clone {
        /// URL of the remote Porchetta store
        url: String,
    },
    /// Migrate configuration from another dotfile manager
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
}

#[derive(Subcommand, Clone)]
enum ManifestCommand {
    /// Open the manifest in your default editor
    Edit,
    /// Print the current manifest
    View,
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

const PORCHETTA_LUALS_STUB: &str = include_str!("../contrib/luals/porchetta.lua");

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

fn init_logging(_quiet: bool) -> Result<()> {
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(cli.quiet)?;

    match cli.command {
        Command::Init => {
            let _store = PorchettaStore::init().context("Failed to initialize store")?;
            ui::success("Initialized Porchetta store");
            ui::muted(&format!("  {}", PorchettaStore::store_path()?));
        }
        Command::Manifest {
            command: ManifestCommand::View,
        } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let manifest_bytes = store.read_manifest().context("Failed to load manifest")?;
            let mut stdout = std::io::stdout().lock();
            std::io::Write::write_all(&mut stdout, &manifest_bytes)
                .context("Failed to write manifest to stdout")?;
        }
        Command::Manifest {
            command: ManifestCommand::Edit,
        } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store, InteractiveResolver)
                .context("Failed to create engine")?;

            let editor = porchetta::util::get_editor().context("Failed to determine editor")?;

            engine
                .edit_manifest(|content| edit_manifest_with_workspace(&editor, content))
                .context("Failed to edit manifest")?;
            ui::success("Manifest updated");
        }
        Command::Sync { dry_run, offline } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store, InteractiveResolver)
                .context("Failed to create engine")?;
            if dry_run {
                ui::header("Syncing topics (dry run)");
            } else {
                ui::header("Syncing topics");
            }
            engine.sync(dry_run, offline).context("Failed to sync")?;
            if dry_run {
                ui::success("Dry run complete");
            } else {
                ui::success("All topics synchronized");
            }
        }
        Command::Clone { url } => {
            let store_path =
                PorchettaStore::store_path().context("Failed to determine store path")?;
            if store_path.exists() {
                anyhow::bail!(
                    "Porchetta store already exists at {store_path}\n\
                     Remove it first or run `porchetta init` if this is a new machine."
                );
            }
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
            let source_dir = source_dir.unwrap_or_else(|| {
                let home = dirs::home_dir().expect("home directory");
                Utf8PathBuf::try_from(home.join(".local").join("share").join("chezmoi"))
                    .expect("chezmoi source path is not valid UTF-8")
            });
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
                command: ManifestCommand::View
            }
        ));
    }

    #[test]
    fn rejects_top_level_edit_command() {
        let err = match Cli::try_parse_from(["porchetta", "edit"]) {
            Ok(_) => panic!("top-level edit command should be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }
}
