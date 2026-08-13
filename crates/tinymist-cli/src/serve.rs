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

    /// Serve the document as HTML rather than as pages. The export shims are
    /// installed with it, so what Typst's HTML export drops is recovered.
    #[clap(long = "html")]
    html: bool,

    /// The address to bind. Loopback by default: reaching this server from
    /// elsewhere should be a decision, not an accident, because the annotation
    /// routes write to the files under the served path.
    #[clap(long = "host", default_value = "127.0.0.1", value_name = "HOST")]
    host: String,

    /// The port to bind. By default it is derived from the canonical path and
    /// the role, so a given document always answers on the same URL.
    #[clap(long = "port", value_name = "PORT")]
    port: Option<u16>,

    /// The project root, so imports that reach outside the document's own
    /// directory resolve. Defaults to the directory the document is in.
    #[clap(long = "root", value_name = "DIR")]
    root: Option<PathBuf>,

    /// A `key=value` pair the document can read through `sys.inputs`. Repeat
    /// for several.
    #[clap(long = "input", value_name = "KEY=VALUE")]
    inputs: Vec<String>,

    /// Print the URL this document is served at and exit, without serving it.
    #[clap(long = "print-url")]
    print_url: bool,

    /// What to call the thing being served, in the web app's name.
    #[clap(long = "root-name", value_name = "NAME")]
    root_name: Option<String>,

    /// The tile colour for this server's icon, as `#rrggbb`.
    #[clap(long = "icon-color", value_name = "HEX")]
    icon_color: Option<String>,

    /// An origin to accept besides loopback, e.g. `http://typst` when this
    /// server is reached through `tailscale serve`. Without it the browser's
    /// `Origin` on the annotation websocket does not match the address this
    /// server bound, and the connection is refused. Repeat for more than one.
    #[clap(long = "allowed-origin", value_name = "ORIGIN")]
    allowed_origins: Vec<String>,

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

/// Whether a document server is already answering on this address.
///
/// A connection that opens is not enough: a server on its way out — one whose
/// binary has just been replaced — still holds its socket for a moment, and
/// handing the URL to a browser then leaves nothing behind it. Only an answer
/// counts.
fn is_live(host: &str, port: u16) -> bool {
    matches!(probe(host, port).as_deref(), Some(stamp)
        if stamp == tinymist::tool::preview::build_stamp())
}

/// Which build is answering on this address, if anything is.
///
/// A connection that opens is not enough, and neither is an answer: a server
/// whose binary has just been replaced still answers for a moment before it
/// notices and exits. Handing the URL to a browser then leaves nothing behind
/// it, so what counts is an answer from *this* build.
fn probe(host: &str, port: u16) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    let timeout = std::time::Duration::from_millis(400);
    for addr in (host, port).to_socket_addrs().ok()? {
        let Ok(mut sock) = TcpStream::connect_timeout(&addr, timeout) else {
            continue;
        };
        let _ = sock.set_read_timeout(Some(timeout));
        let _ = sock.set_write_timeout(Some(timeout));
        let request = format!("GET /dev/build HTTP/1.0\r\nHost: {host}:{port}\r\n\r\n");
        if sock.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut buf = String::new();
        if sock.take(4096).read_to_string(&mut buf).is_err() {
            continue;
        }
        let (head, body) = buf.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.") || !head.contains(" 200") {
            continue;
        }
        return Some(body.trim().to_owned());
    }
    None
}

/// Waits for a server of an older build to let go of the port it holds.
fn wait_for_port(host: &str, port: u16) {
    for _ in 0..40 {
        if probe(host, port).is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Opens a URL as the arguments ask, the same way the preview does.
fn open_url(url: &str, args: &ServeArgs, port: u16) {
    use tinymist::tool::preview::open;
    let identity = tinymist::tool::preview::WebAppIdentity {
        role: if args.anno {
            IconRole::Annotate
        } else {
            IconRole::Serve
        },
        color: args
            .icon_color
            .as_deref()
            .and_then(tinymist::tool::preview::icons::parse_hex),
        name: args.root_name.clone().or_else(|| {
            args.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }),
    };
    open::open(
        url,
        &open::OpenOptions {
            browser: args
                .open_in
                .as_deref()
                .map(open::Browser::parse)
                .unwrap_or(open::Browser::Default),
            isolated: args.open_isolated,
            cdp_port: args
                .open_cdp
                .as_deref()
                .and_then(|spec| open::parse_cdp_port(spec, port)),
            app_title: identity.short_title(port),
            key: port,
        },
    );
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
    // A document that names itself is naming itself: `#set document(title: ..)`
    // wins over the first heading, which is often just section one.
    if let Some(at) = text.find("#set document(") {
        let rest = &text[at..];
        if let Some(key) = rest.find("title:") {
            let after = &rest[key + "title:".len()..];
            if let Some(open) = after.find('"') {
                if let Some(close) = after[open + 1..].find('"') {
                    let title = after[open + 1..open + 1 + close].trim();
                    if !title.is_empty() {
                        return Some(title.to_owned());
                    }
                }
            }
        }
    }
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
    tinymist::tool::preview::note_build_stamp();
    // A person is watching this in a terminal, so it narrates: what changed on
    // disk, and who is connected. The LSP shares its streams with the editor
    // and stays silent.
    tinymist_project::ANNOUNCE_ACTIVITY.store(true, std::sync::atomic::Ordering::Relaxed);
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

    let path = if args.anno { "/annotate" } else { "/" };
    let url = format!("http://{}:{port}{path}", args.host);
    if args.print_url {
        println!("{url}");
        return Ok(());
    }
    // A document that is already being served is served: the port is derived
    // from the path for exactly this reason, and a server whose binary has been
    // replaced has already exited on its own. Asking for it again means "show
    // it to me", not "bind this port twice".
    match probe(&args.host, port) {
        Some(stamp) if stamp == tinymist::tool::preview::build_stamp() => {
            eprintln!("already serving {url}");
            if args.open {
                open_url(&url, &args, port);
            }
            return Ok(());
        }
        // An older build is still there: it has been told to stop and is about
        // to, so this one waits for its address rather than taking its place.
        Some(_) => wait_for_port(&args.host, port),
        None => {}
    }

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
    let root = args.root.clone().unwrap_or(root);

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
    if args.html {
        argv.push("--format=html".into());
    }
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
    for origin in &args.allowed_origins {
        argv.push(format!("--allowed-origin={origin}"));
    }
    for input in &args.inputs {
        argv.push(format!("--input={input}"));
    }
    argv.push(entry.display().to_string());

    let mut preview_args = tinymist::tool::preview::PreviewCliArgs::parse_from(&argv);
    preview_args.annotate = args.anno;
    block_on(crate::cmd::preview::preview_main(preview_args))
}
