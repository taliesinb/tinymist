//! `talimist open-preview`: open the preview the language server is already
//! running.
//!
//! The preview an editor follows lives inside the language server: it compiles
//! from the buffers being typed in, so it updates without a save, and it follows
//! whatever file has focus. Nothing needs to be started to look at it — only
//! found. The server leaves a note in the register saying where it answers, and
//! this reads that note and opens a window on it.
//!
//! It exists so an editor task can be a command rather than a shell script
//! guessing at ports and cache files.

use std::path::PathBuf;

use tinymist::tool::preview::open;
use tinymist::tool::registry;
use tinymist_std::error::prelude::*;

/// Which project's preview to open, and how.
#[derive(Debug, Clone, clap::Parser)]
pub struct OpenPreviewArgs {
    /// The workspace root, as the editor knows it. The current directory when
    /// absent.
    #[clap(value_name = "ROOT")]
    pub root: Option<PathBuf>,

    /// How long to wait for the preview to appear, in seconds.
    ///
    /// An editor that has only just opened is still starting its language
    /// server, and a task run in that moment would otherwise find nothing.
    #[clap(long = "wait", value_name = "SECONDS", default_value_t = 10)]
    pub wait: u64,

    /// Print the URL and exit, without opening anything.
    #[clap(long = "print-url")]
    pub print_url: bool,

    /// Which browser to open it in. The system default when absent.
    #[clap(long = "open-in", value_name = "BROWSER")]
    pub open_in: Option<String>,

    /// Give the preview a window of its own rather than a tab among others.
    #[clap(long = "open-isolated")]
    pub open_isolated: bool,

    /// Expose a debugging port on that window: `auto` for one derived from the
    /// preview's own port, or a number. Chrome only.
    #[clap(long = "open-cdp", value_name = "PORT")]
    pub open_cdp: Option<String>,
}

/// Finds the preview for a project and opens it.
pub fn open_preview_main(args: OpenPreviewArgs) -> Result<()> {
    let root = match &args.root {
        Some(root) => root.clone(),
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    let canonical = std::fs::canonicalize(&root).unwrap_or(root.clone());
    let wanted = canonical.display().to_string();

    // Polled rather than waited on: the note appears when the language server
    // has bound its port, and there is no other way to hear about that from
    // outside the process.
    let began = std::time::Instant::now();
    let note = loop {
        let found = registry::running_servers()
            .into_iter()
            .find(|note| note.role == "preview" && note.path == wanted);
        if let Some(note) = found {
            break note;
        }
        if began.elapsed() >= std::time::Duration::from_secs(args.wait) {
            bail!(
                "no preview is running for {wanted}. The editor's preview is served by the \
                 language server: enable it with `preview.background.enabled` in the tinymist \
                 settings, or serve the document on its own with `talimist serve`."
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    };

    if args.print_url {
        println!("{}", note.url);
        return Ok(());
    }

    let identity = tinymist::tool::webapp::WebAppIdentity {
        role: tinymist::tool::webapp::icons::IconRole::Lsp,
        color: None,
        // The project's name: the window follows the focused file, so naming it
        // after a file would be wrong a moment later.
        name: canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    open::open(
        &note.url,
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
                .and_then(|spec| open::parse_cdp_port(spec, note.port)),
            app_title: identity.short_title(note.port),
            key: note.port,
        },
    );
    Ok(())
}
