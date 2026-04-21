use clap::{Parser, Subcommand};
use log::info;

use porchetta::engine::PorchettaEngine;
use porchetta::store::PorchettaStore;

#[derive(Parser)]
#[command(name = "porchetta")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
    Init,
    Edit,
    Show,
    Sync,
}

fn main() {
    simple_logger::SimpleLogger::new()
        .with_local_timestamps()
        .with_level(log::LevelFilter::Debug)
        .init()
        .expect("Failed to initialize logger");

    let cli = Cli::parse();

    match cli.command {
        Command::Init => {
            PorchettaStore::init().expect("Failed to initialize store");
            info!("Initialized Porchetta store");
        }
        Command::Show => {
            let store = PorchettaStore::load().expect("Failed to load store");
            let manifest = store.read_manifest().expect("Failed to load manifest");
            println!("{}", String::from_utf8_lossy(&manifest));
        }
        Command::Edit => {
            let store = PorchettaStore::load().expect("Failed to load store");
            let mut engine = PorchettaEngine::new(store);

            let editor = std::env::var("EDITOR").expect("EDITOR environment variable not set");

            engine
                .edit_manifest(|content| {
                    let mut temp_file = tempfile::Builder::new()
                        .prefix("porchetta_manifest")
                        .suffix(".lua")
                        .tempfile()
                        .expect("Failed to create temp file");
                    std::io::Write::write_all(&mut temp_file, content)
                        .expect("Failed to write manifest to temp file");
                    let status = std::process::Command::new(&editor)
                        .arg(temp_file.path())
                        .status()
                        .expect("Failed to launch editor");
                    if !status.success() {
                        panic!("Editor exited with non-zero status");
                    }
                    std::fs::read(temp_file.path()).expect("Failed to read edited manifest")
                })
                .expect("Failed to edit manifest");
            info!("Edited manifest");
        }
        Command::Sync => {
            let store = PorchettaStore::load().expect("Failed to load store");
            let mut engine = PorchettaEngine::new(store);
            engine.sync().expect("Failed to sync");
            info!("Synced topics");
        }
    }
}
