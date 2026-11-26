//! Build and index the workspace's rustdoc JSON using the installed nightly.

mod document;
mod source;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use argh::FromArgs;
use serde_json::{Value, json};

use crate::step::{describe, workspace_root};

/// Arguments for the workspace API index.
#[derive(FromArgs)]
#[argh(
    subcommand,
    name = "api-index",
    description = "index workspace API declarations from rustdoc JSON"
)]
pub(crate) struct Args {
    /// include APIs behind non-default features.
    #[argh(switch)]
    all_features: bool,
    /// also index private items for source navigation.
    #[argh(switch)]
    document_private_items: bool,
}

const FORMAT_VERSION: u64 = 61;

pub(crate) fn run(args: &Args) -> Result<(), String> {
    let root = workspace_root()?;
    let toolchain = Command::new("rustup")
        .args(["run", "nightly", "rustc", "--version", "--verbose"])
        .output()
        .map_err(|error| format!("cannot run rustup: {error}; install Rust through rustup"))?;
    if !toolchain.status.success() {
        return Err(format!(
            "cannot run nightly rustc; install it with `rustup toolchain install nightly`:\n{}",
            String::from_utf8_lossy(&toolchain.stderr).trim()
        ));
    }

    let mut cargo = Command::new("cargo");
    cargo.current_dir(&root).args([
        "+nightly",
        "doc",
        "--workspace",
        "--no-deps",
        "--locked",
        "-Z",
        "unstable-options",
        "--output-format",
        "json",
        "--message-format=json",
        "--target-dir",
        "target/api-index-rustdoc",
    ]);
    if args.all_features {
        cargo.arg("--all-features");
    }
    if args.document_private_items {
        cargo.arg("--document-private-items");
    }
    println!("==> {}", describe(&cargo));
    let mut child = cargo
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("starting nightly cargo doc: {error}"))?;
    let stdout = child.stdout.take().expect("cargo stdout is piped");
    let mut paths = BTreeSet::new();
    let mut diagnostic_error = None;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                diagnostic_error = Some(format!("reading cargo's artifact stream: {error}"));
                break;
            }
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            diagnostic_error = Some(format!("invalid JSON in cargo's artifact stream: {line}"));
            continue;
        };
        match event["reason"].as_str() {
            Some("compiler-artifact") => {
                paths.extend(
                    event["filenames"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|file| {
                            let path = PathBuf::from(file.as_str()?);
                            (path.extension()?.to_str()? == "json").then_some(path)
                        }),
                );
            }
            Some("compiler-message") => {
                if let Some(message) = event["message"]["rendered"].as_str() {
                    eprint!("{message}");
                }
            }
            _ => {}
        }
    }
    let status = child
        .wait()
        .map_err(|error| format!("waiting for cargo doc: {error}"))?;
    if !status.success() {
        return Err(format!(
            "nightly cargo doc exited with {status}; see the diagnostics above"
        ));
    }
    if let Some(error) = diagnostic_error {
        return Err(error);
    }
    if paths.is_empty() {
        return Err("cargo doc reported no rustdoc JSON artifacts".to_owned());
    }

    let mut sources = source::Sources::new(&root);
    let mut entries = BTreeSet::new();
    for path in &paths {
        let file =
            std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let value: Value = serde_json::from_reader(BufReader::new(file))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let document = document::Document::parse(&value)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        document.collect(&mut sources, args.document_private_items, &mut entries)?;
    }

    let output = root.join("target/api-index.json");
    let staged = root.join(format!("target/api-index.{}.tmp", std::process::id()));
    let value = json!({
        "format_version": FORMAT_VERSION,
        "toolchain": String::from_utf8_lossy(&toolchain.stdout).trim(),
        "all_features": args.all_features,
        "document_private_items": args.document_private_items,
        "entries": entries.iter().map(document::Entry::to_value).collect::<Vec<_>>(),
    });
    std::fs::write(&staged, format!("{value:#}\n"))
        .and_then(|()| std::fs::rename(&staged, &output))
        .map_err(|error| format!("publishing {}: {error}", output.display()))?;
    println!(
        "Indexed {} declarations in {}",
        entries.len(),
        output.display()
    );
    Ok(())
}
