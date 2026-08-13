//! `talimist-serve`: a document server with no editor attached.
//!
//! The language server has to restart when its binary changes and is tied to
//! one editor session; a document you are reading, or sharing with someone,
//! should not be. This serves `.typ` files over HTTP on a port derived from the
//! path it was given, so a document keeps its URL — and therefore its web app
//! and its icon — across restarts.

mod conn;
mod utils;
mod cmd {
    pub mod preview;
}

use std::path::{Path, PathBuf};

use clap::Parser;
use tinymist::tool::preview::icons::IconRole;
use tinymist_std::error::prelude::*;

use crate::utils::block_on;

/// Shared runtimes, as the other binary has.
pub static RUNTIMES: std::sync::LazyLock<Runtimes> = std::sync::LazyLock::new(Runtimes::default);

/// The runtimes this binary needs.
pub struct Runtimes {
    /// The tokio runtime everything async runs on.
    pub tokio_runtime: tokio::runtime::Runtime,
}

impl Default for Runtimes {
    fn default() -> Self {
        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("cannot build a tokio runtime");
        Self { tokio_runtime }
    }
}

#[derive(Debug, Parser)]
#[clap(
    name = "talimist-serve",
    author,
    version,
    about = "Serves Typst documents over HTTP, with no editor attached"
)]
struct ServeArgs {
    /// The document or directory to serve. A `.typ` file is served at `/`; a
    /// directory serves the file named by each path under it.
    #[clap(value_name = "PATH")]
    path: PathBuf,

    /// Enable the annotation UI, and with it the routes that write annotations
    /// back into the document.
    #[clap(long = "anno")]
    anno: bool,

    /// The address to bind. Loopback by default: reaching this server from
    /// elsewhere should be a decision, not an accident, because the annotation
    /// routes write to the files under the served path.
    #[clap(long = "host", default_value = "127.0.0.1", value_name = "HOST")]
    host: String,

    /// The port to bind. By default it is derived from the canonical path and
    /// the role, so a given document always answers on the same URL.
    #[clap(long = "port", value_name = "PORT")]
    port: Option<u16>,

    /// What to call the thing being served, in the web app's name.
    #[clap(long = "root-name", value_name = "NAME")]
    root_name: Option<String>,

    /// The tile colour for this server's icon, as `#rrggbb`.
    #[clap(long = "icon-color", value_name = "HEX")]
    icon_color: Option<String>,

    /// Keep running after the process that started this one goes away.
    #[clap(long = "daemon")]
    daemon: bool,

    /// Exit once the last browser disconnects.
    #[clap(long = "shutdown-on-last-client")]
    shutdown_on_last_client: bool,

    /// Emit the info logging of the compiler and the watcher. Without it only
    /// warnings, errors, and this server's own address are printed.
    #[clap(long = "verbose", short = 'v')]
    verbose: bool,

    /// Open a browser once the server is up.
    #[clap(long = "open")]
    open: bool,

    /// What to open with: `chrome`, `safari`, `firefox`, `browser` (the system
    /// default), or an application named literally, such as "Google Chrome
    /// Canary" or a path to a bundle.
    #[clap(long = "open-in", value_name = "APP")]
    open_in: Option<String>,

    /// Give the document a window of its own rather than a tab: a web app
    /// installed from this URL if there is one, else the nearest equivalent
    /// the browser offers.
    #[clap(long = "open-isolated")]
    open_isolated: bool,

    /// Expose a debugging port on the opened window. `auto` derives one from
    /// this server's port. Chrome only.
    #[clap(long = "open-cdp", value_name = "PORT")]
    open_cdp: Option<String>,
}

/// The port for a path and role.
///
/// Derived rather than assigned: a document that keeps its port keeps its URL,
/// and a web app added to the Dock keeps working across restarts. The role
/// takes part so that serving and annotating the same document are two servers
/// with two origins, and therefore two dock apps.
pub fn derive_port(canonical: &Path, role: IconRole) -> u16 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let role = match role {
        IconRole::Lsp => b"lsp".as_slice(),
        IconRole::Serve => b"serve".as_slice(),
        IconRole::Annotate => b"anno".as_slice(),
    };
    for byte in canonical.as_os_str().as_encoded_bytes().iter().chain(role) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    // Above the LSP's own range, which is 23700..24000.
    24000 + (hash % 1000) as u16
}

/// The `.typ` files directly under a directory, sorted.
pub fn documents_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "typ"))
        .collect();
    found.sort();
    found
}

/// The title a document announces, taken from its first heading.
///
/// A parse rather than a compile: this is for a name in a list, and compiling
/// every document in a directory to label a link would be absurd. Anything
/// that starts markup the heading cannot carry — a bracket, a maths run, an
/// anchor label — ends the title.
pub fn document_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("= ") else {
            continue;
        };
        let title: String = rest
            .chars()
            .take_while(|c| !matches!(c, '[' | '$' | '<' | '#' | '@'))
            .filter(|c| c.is_ascii() && !c.is_control())
            .collect();
        let title = title.trim();
        if !title.is_empty() {
            return Some(title.to_owned());
        }
    }
    None
}

fn main() -> Result<()> {
    let args = ServeArgs::parse();
    let _ = tinymist::init_log(tinymist::InitLogOpts {
        verbose: args.verbose,
        // The preview server logs its addresses at info level whatever else is
        // configured, for editors that discover it by reading stdout. This
        // binary prints its own address block instead, so in quiet mode that
        // target is turned down too — otherwise the one line worth copying
        // arrives with two decoys.
        filter: (!args.verbose).then(|| "tinymist::compat::preview=warn".to_string()),
        output: None,
    });
    // The serve binary is as long-lived as the LSP: a replaced binary is a
    // reason to stop, unless it was asked to outlive its parent.
    if !args.daemon {
        tinymist::tool::preview::exit_when_orphaned();
    }
    crate::utils::exit_when_binary_replaced();

    let canonical = std::fs::canonicalize(&args.path)
        .with_context("cannot resolve the path to serve", || None)?;
    let role = if args.anno {
        IconRole::Annotate
    } else {
        IconRole::Serve
    };
    let port = args.port.unwrap_or_else(|| derive_port(&canonical, role));

    // A directory serves its index; the rest of directory mode — a listing, and
    // a document per path — comes next.
    let (entry, root) = if canonical.is_dir() {
        let index = canonical.join("index.typ");
        if !index.exists() {
            let found = documents_in(&canonical);
            bail!(
                "no index.typ in {}; it holds {} documents ({}). Directory mode is not built yet — \
                 name a file for now.",
                canonical.display(),
                found.len(),
                found
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        (index, canonical.clone())
    } else {
        let root = canonical
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| canonical.clone());
        (canonical.clone(), root)
    };

    let name = args.root_name.clone().or_else(|| {
        document_title(&entry).or_else(|| {
            canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
    });

    // One pipeline, driven through the preview CLI's own arguments.
    let mut argv: Vec<String> = vec![
        "talimist-serve".into(),
        format!("--data-plane-host={}:{port}", args.host),
        "--control-plane-host=127.0.0.1:0".into(),
        "--invert-colors=smart".into(),
        format!("--root={}", root.display()),
    ];
    if args.anno {
        argv.push("--annotate".into());
    }
    if args.daemon {
        argv.push("--daemon".into());
    }
    if args.shutdown_on_last_client {
        argv.push("--shutdown-on-last-client".into());
    }
    if args.open {
        argv.push("--open".into());
    } else {
        argv.push("--no-open".into());
    }
    if args.verbose {
        argv.push("--verbose".into());
    }
    if let Some(app) = &args.open_in {
        argv.push(format!("--open-in={app}"));
    }
    if args.open_isolated {
        argv.push("--open-isolated".into());
    }
    if let Some(port) = &args.open_cdp {
        argv.push(format!("--open-cdp={port}"));
    }
    if let Some(name) = &name {
        argv.push(format!("--root-name={name}"));
    }
    if let Some(color) = &args.icon_color {
        argv.push(format!("--icon-color={color}"));
    }
    argv.push(entry.display().to_string());

    let mut preview_args = tinymist::tool::preview::PreviewCliArgs::parse_from(&argv);
    preview_args.annotate = args.anno;
    block_on(crate::cmd::preview::preview_main(preview_args))
}
