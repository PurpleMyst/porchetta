use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use porchetta::engine::PorchettaEngine;
use porchetta::manifest::Manifest;
use porchetta::store::PorchettaStore;
use porchetta::ui;

#[derive(Parser)]
#[command(name = "porchetta")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase output verbosity
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Decrease output verbosity
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Clone)]
enum Command {
    Init,
    Edit,
    Show,
    Sync {
        /// Preview changes without applying them
        #[arg(long)]
        dry_run: bool,
    },
    Clone {
        /// URL of the remote Porchetta store
        url: String,
    },
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
}

#[derive(Subcommand, Clone)]
enum MigrateCommand {
    Chezmoi {
        /// Path to chezmoi source directory (default: ~/.local/share/chezmoi)
        #[arg(long, value_name = "DIR")]
        source_dir: Option<Utf8PathBuf>,
        /// Do not prompt for confirmation before overwriting the manifest
        #[arg(long)]
        yes: bool,
    },
}

fn init_logging(quiet: bool, verbose: bool) -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .context("Could not determine local data directory")?
        .join("porchetta")
        .join("logs");

    let spec = match (quiet, verbose) {
        (true, _) => "warn",
        (_, true) => "debug",
        _ => "info",
    };

    flexi_logger::Logger::try_with_str(spec)?
        .log_to_file(flexi_logger::FileSpec::default().directory(log_dir))
        .duplicate_to_stderr(flexi_logger::Duplicate::Warn)
        .append()
        .start()
        .context("Failed to initialize logger")?;

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(cli.quiet, cli.verbose)?;

    match cli.command {
        Command::Init => {
            let _store = PorchettaStore::init().context("Failed to initialize store")?;
            ui::success("Initialized Porchetta store");
            ui::muted(&format!("  {}", PorchettaStore::store_path()?));
        }
        Command::Show => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let manifest_bytes = store.read_manifest().context("Failed to load manifest")?;
            ui::header("Manifest");
            ui::manifest_block(&String::from_utf8_lossy(&manifest_bytes));

            if cli.verbose {
                let manifest = Manifest::load(&manifest_bytes).context("Failed to parse manifest")?;
                ui::info(&format!("{} topics", manifest.topics.len()));
                for (name, topic) in &manifest.topics {
                    ui::bullet(&format!("{name} ({} paths)", topic.paths.len()));
                    for path in &topic.paths {
                        ui::muted(&format!("    {path}"));
                    }
                }
            }
        }
        Command::Edit => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store);

            let editor = porchetta::util::get_editor()
                .context("Failed to determine editor")?;

            engine
                .edit_manifest(|content| -> Result<Vec<u8>> {
                    let mut temp_file = tempfile::Builder::new()
                        .prefix("porchetta_manifest")
                        .suffix(".lua")
                        .tempfile()
                        .context("Failed to create temp file")?;
                    std::io::Write::write_all(&mut temp_file, content)
                        .context("Failed to write manifest to temp file")?;
                    let status = std::process::Command::new(&editor)
                        .arg(temp_file.path())
                        .status()
                        .context("Failed to launch editor")?;
                    if !status.success() {
                        anyhow::bail!("Editor exited with non-zero status");
                    }
                    std::fs::read(temp_file.path()).context("Failed to read edited manifest")
                })
                .context("Failed to edit manifest")?;
            ui::success("Manifest updated");
        }
        Command::Sync { dry_run } => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store);
            if dry_run {
                ui::header("Syncing topics (dry run)");
            } else {
                ui::header("Syncing topics");
            }
            engine.sync(cli.verbose, dry_run).context("Failed to sync")?;
            if dry_run {
                ui::success("Dry run complete");
            } else {
                ui::success("All topics synchronized");
            }
        }
        Command::Clone { url } => {
            let store_path = PorchettaStore::store_path().context("Failed to determine store path")?;
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
