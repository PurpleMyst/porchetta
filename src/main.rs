use anyhow::{Context, Result};
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
    Sync,
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
            ui::muted(&format!("  {}", PorchettaStore::store_path()?.display()));
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
                        ui::muted(&format!("    {}", path.display()));
                    }
                }
            }
        }
        Command::Edit => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store);

            let editor =
                std::env::var("EDITOR").context("EDITOR environment variable not set")?;

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
        Command::Sync => {
            let store = PorchettaStore::load().context("Failed to load store")?;
            let mut engine = PorchettaEngine::new(store);
            ui::header("Syncing topics");
            engine.sync(cli.verbose).context("Failed to sync")?;
            ui::success("All topics synchronized");
        }
    }

    Ok(())
}
