//! `talimist annos`: reading a document's annotations without a server.
//!
//! The sidecar is JSON and the anchors are labels in the document, so both can
//! be read directly. This exists to answer questions about a document's
//! annotations from a script or a terminal, and to check that a sidecar and its
//! document agree.

use std::path::PathBuf;

use tinymist_annos::{anchor, audit, dangling, Sidecar};
use tinymist_std::error::prelude::*;

/// What to report about a document's annotations.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum AnnosCommands {
    /// List a document's annotations
    List(AnnosArgs),
    /// Report anchors and annotations that do not match up
    Audit(AnnosArgs),
}

/// Which document to read.
#[derive(Debug, Clone, clap::Parser)]
pub struct AnnosArgs {
    /// The document, or its sidecar.
    #[clap(value_name = "FILE")]
    pub path: PathBuf,

    /// Print the annotations as JSON rather than as text.
    #[clap(long = "json")]
    pub json: bool,
}

/// Runs an `annos` subcommand.
pub fn annos_main(cmd: AnnosCommands) -> Result<()> {
    match cmd {
        AnnosCommands::List(args) => list(args),
        AnnosCommands::Audit(args) => check(args),
    }
}

/// The document and its sidecar, from either of their paths.
fn read(args: &AnnosArgs) -> Result<(PathBuf, String, Sidecar)> {
    let document = if tinymist_annos::is_sidecar(&args.path) {
        let name = args
            .path
            .file_name()
            .map(|name| name.to_string_lossy().replace(".annos.json", ".typ"))
            .context("the sidecar has no name")?;
        args.path.with_file_name(name)
    } else {
        args.path.clone()
    };
    let sidecar_path = tinymist_annos::sidecar_path(&document);
    let sidecar = Sidecar::read(&sidecar_path).map_err(|err| error_once!("cannot read", err: err))?;
    let text = std::fs::read_to_string(&document).unwrap_or_default();
    Ok((document, text, sidecar))
}

/// Prints a document's annotations.
fn list(args: AnnosArgs) -> Result<()> {
    let (_, _, sidecar) = read(&args)?;
    if args.json {
        println!("{}", sidecar.to_json().trim_end());
        return Ok(());
    }
    if sidecar.annotations.is_empty() {
        println!("no annotations");
        return Ok(());
    }
    for anno in &sidecar.annotations {
        let flags = match (anno.claimed, anno.resolved) {
            (_, true) => "resolved",
            (true, false) => "claimed",
            _ => "open",
        };
        println!(
            "{letter:<3} {kind:<8} {flags:<8} {location:<12} {labels:<24} {author:<8} {content}",
            letter = anno.letter,
            kind = anno.kind,
            location = anno.location.kind(),
            labels = anno.labels().join(","),
            author = anno.author,
            content = anno.content.lines().next().unwrap_or_default(),
        );
    }
    Ok(())
}

/// Reports anchors and annotations that do not match up.
fn check(args: AnnosArgs) -> Result<()> {
    let (document, text, sidecar) = read(&args)?;
    let present: Vec<String> = anchor::anchors_in_text(&text)
        .iter()
        .map(|found| found.name())
        .collect();
    let report = audit(present.iter().map(String::as_str), &sidecar);

    println!(
        "{}: {} annotations, {} anchors",
        document.display(),
        sidecar.annotations.len(),
        present.len()
    );
    for label in &report.unused {
        println!("unused anchor    {label}");
    }
    for (anno, lost) in dangling(&sidecar, &report) {
        println!(
            "missing anchor   {} (annotation {}, {})",
            lost.join(","),
            anno.letter,
            anno.snapshot.as_deref().unwrap_or("no snapshot")
        );
    }
    if report.is_clean() {
        println!("the document and its sidecar agree");
    }
    Ok(())
}
