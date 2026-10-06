use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;
use clap::Args;
use porchetta::{manifest::Manifest, store::PorchettaStore, ui};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ManifestCommand;

#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
pub(crate) struct ManifestInput {
    /// Read Lua source from a file (mutually exclusive with --stdin)
    #[arg(long, value_name = "PATH")]
    file: Option<Utf8PathBuf>,
    /// Read Lua source from standard input until EOF (mutually exclusive with --file)
    #[arg(long)]
    stdin: bool,
}

impl ManifestInput {
    fn read(&self) -> Result<Vec<u8>> {
        if let Some(path) = &self.file {
            return std::fs::read(path).with_context(|| format!("Failed to read '{path}'"));
        }
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

fn revision(source: &[u8]) -> String {
    format!("{:x}", Sha256::digest(source))
}

#[cfg(test)]
mod tests {
    use super::revision;

    #[test]
    fn revisions_are_sha256_of_exact_source_bytes() {
        assert_eq!(
            revision(b"return { topics = {} }\n"),
            "c2a3590afcdedf4167ad916f63ca7d331f4681e9972d908d0505b77611761081"
        );
        assert_ne!(
            revision(b"return { topics = {} }\n"),
            revision(b"return { topics = {} }\r\n")
        );
    }
}

fn emit(value: &Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn load(source: &[u8]) -> Result<Manifest> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let home = Utf8PathBuf::try_from(home)?;
    Manifest::load_with_home(source, Some(&home))
}

pub(crate) fn run(command: ManifestCommand) -> Result<()> {
    let json_output = match &command {
        ManifestCommand::View { json }
        | ManifestCommand::Set { json, .. }
        | ManifestCommand::Validate { json, .. }
        | ManifestCommand::Inspect { json } => *json,
        ManifestCommand::Edit => unreachable!(),
    };
    let validating = matches!(command, ManifestCommand::Validate { .. });
    match execute(command) {
        Ok(()) => Ok(()),
        Err(error) if json_output => {
            let diagnostic = error.downcast_ref::<porchetta::manifest::Diagnostic>();
            let code = diagnostic.map_or_else(
                || {
                    if error.downcast_ref::<RevisionMismatch>().is_some() {
                        "revision_mismatch"
                    } else {
                        "operation_failed"
                    }
                },
                |d| d.code,
            );
            let mut output = json!({"errors": [{"code": code, "message": format!("{error:#}")}]});
            if validating {
                output["valid"] = json!(false);
            }
            if let Some(d) = diagnostic {
                output["errors"][0]["topic"] = json!(d.topic);
                output["errors"][0]["field"] = json!(d.field);
                output["errors"][0]["value"] = json!(d.value);
            }
            emit(&output)?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug)]
struct RevisionMismatch;

impl std::fmt::Display for RevisionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Manifest revision changed; reread the manifest and retry")
    }
}

impl std::error::Error for RevisionMismatch {}

fn execute(command: ManifestCommand) -> Result<()> {
    match command {
        ManifestCommand::View { json } => {
            let source = PorchettaStore::load()?.read_manifest()?;
            if json {
                emit(
                    &json!({"source": std::str::from_utf8(&source)?, "revision": revision(&source)}),
                )?;
            } else {
                std::io::stdout().lock().write_all(&source)?;
            }
        }
        ManifestCommand::Validate { input, json } => {
            load(&input.read()?)?;
            if json {
                emit(&json!({"valid": true, "errors": []}))?;
            } else {
                ui::success("Manifest is valid");
            }
        }
        ManifestCommand::Set {
            input,
            if_revision,
            json,
        } => {
            let source = input.read()?;
            load(&source)?;
            // The store's exclusive lock covers both comparison and write.
            let store = PorchettaStore::load()?;
            if let Some(expected) = if_revision
                && revision(&store.read_manifest()?) != expected
            {
                return Err(RevisionMismatch.into());
            }
            store.write_manifest(&source)?;
            if json {
                emit(&json!({"revision": revision(&source)}))?;
            } else {
                ui::success("Manifest updated");
            }
        }
        ManifestCommand::Inspect { json: json_output } => {
            let source = PorchettaStore::load()?.read_manifest()?;
            let manifest = load(&source)?;
            let topics: Vec<Value> = manifest
                .topics
                .iter()
                .map(|topic| {
                    json!({
                        "name": topic.name,
                        "enabled": topic.enabled,
                        "root": topic.root.as_ref().map(|p| p.as_str()),
                        "paths": topic.paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                        "hooks": {
                            "to_repo": topic.to_repo.is_some(),
                            "to_system": topic.to_system.is_some(),
                            "should_include": topic.should_include.is_some()
                        }
                    })
                })
                .collect();
            let output = json!({"revision": revision(&source), "topics": topics,
                "hooks": {"should_include": manifest.should_include.is_some()}});
            if json_output {
                emit(&output)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        }
        ManifestCommand::Edit => bail!("Interactive edit is handled separately"),
    }
    Ok(())
}
