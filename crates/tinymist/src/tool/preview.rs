//! Document preview tool for Typst

pub use compile::{PreviewCompileView, ProjectPreviewHandler};
pub use error_overlay::{
    cursor_overlay, diagnostics_payload, doc_is_dark, overlay_js, overlay_js_path, BlockExtent,
    DiagRx, DiagTx, OverlayPayload, EARLY_ERROR_JS,
};
pub use http::{make_http_server, HttpServer};

pub mod open;

mod compile;
mod error_overlay;
pub(crate) mod http;

use std::{collections::HashMap, path::Path, sync::Arc};

use clap::{Parser, ValueEnum};
use futures::{SinkExt, TryStreamExt};
use hyper_tungstenite::{tungstenite::Message, HyperWebsocket, HyperWebsocketStream};
use lsp_types::notification::Notification;
use lsp_types::Url;
use reflexo_typst::error::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sync_ls::just_ok;
use tinymist_assets::TYPST_PREVIEW_HTML;
use tinymist_preview::{
    frontend_html, ControlPlaneMessage, ControlPlaneRx, ControlPlaneTx, DocToSrcJumpInfo,
    PreviewBuilder, PreviewConfig, PreviewMode, Previewer, ViewerWindowState, WsMessage,
};
use tinymist_query::{LspPosition, LspRange};
use tinymist_std::error::IgnoreLogging;
use tinymist_task::ExportTarget;
use tokio::sync::{mpsc, oneshot};

use crate::actor::preview::{PreviewActor, PreviewRequest, PreviewTab};
use crate::project::{ProjectInsId, ProjectPreviewState};
use crate::world::vfs::ImmutPath;
use crate::*;

/// The kind of the preview.
pub enum PreviewKind {
    /// Previews a specific file.
    Regular,
    /// Walks through the project and previews the main file related to the
    /// current focused file.
    Browsing,
    /// Runs a browsing preview in background.
    Background,
}

/// The refresh style for the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum RefreshStyle {
    /// Refresh preview on save
    #[cfg_attr(feature = "clap", clap(name = "onSave"))]
    OnSave,

    /// Refresh preview on type
    #[cfg_attr(feature = "clap", clap(name = "onType"))]
    #[default]
    OnType,
}

impl From<RefreshStyle> for TaskWhen {
    fn from(style: RefreshStyle) -> Self {
        match style {
            RefreshStyle::OnSave => TaskWhen::OnSave,
            RefreshStyle::OnType => TaskWhen::OnType,
        }
    }
}

/// Specify arguments related to the preview service.
#[derive(Debug, Clone, clap::Parser)]
pub struct PreviewArgs {
    /// Configure the preview output format.
    ///
    /// A preview writes no output file, so this selects the Typst compilation
    /// target the live preview renders from: `paged` draws the document as
    /// pages, which is what a printed copy looks like; `html` reflows and is
    /// much faster to update, which is what technical writing wants.
    /// Unset means the caller did not say: the CLI draws pages, and the editor
    /// draws whatever `preview.mode` asks for.
    #[clap(long = "format", value_name = "FORMAT")]
    pub format: Option<ExportTarget>,

    /// Preview the document as HTML. The default, and named so a command can
    /// say what it means; the same spelling `talimist serve` uses.
    #[clap(long = "html")]
    pub html: bool,

    /// Preview the document as pages rendered to SVG, the way an editor's own
    /// preview does. Shorthand for `--format=paged`.
    #[clap(long = "paged-svg", alias = "paged")]
    pub paged_svg: bool,

    /// Configure the preview mode.
    #[clap(long = "preview-mode", default_value = "document", value_name = "MODE")]
    pub preview_mode: PreviewMode,

    /// Set the preview page title.
    ///
    /// If not specified, the title falls back to the input filename when
    /// available, or otherwise to `"Typst Preview"`.
    #[clap(long = "page-title", value_name = "TITLE")]
    pub page_title: Option<String>,

    /// Only render visible part of the document.
    ///
    /// This can improve performance but still being experimental.
    #[clap(long = "partial-rendering")]
    pub enable_partial_rendering: Option<bool>,

    /// Configure the way to invert colors of the preview.
    ///
    /// This is useful for dark themes without cost.
    ///
    /// Please note you could see the original colors when you hover elements in
    /// the preview.
    ///
    /// It is also possible to specify strategy to each element kind by an
    /// object map in JSON format.
    ///
    /// Possible element kinds:
    /// - `image`: Images in the preview.
    /// - `rest`: Rest elements in the preview.
    ///
    /// By default, the preview will never invert colors.
    ///
    /// ## Example
    ///
    /// By string:
    ///
    /// ```shell
    /// --invert-colors=auto
    /// ```
    ///
    /// By element:
    ///
    /// ```shell
    /// --invert-colors='{"rest": "always", "image": "never"}'
    /// ```
    #[clap(long)]
    pub invert_colors: Option<String>,

    /// Used by lsp for controlling the preview refresh style.
    ///
    /// This is hidden from the CLI.
    #[clap(long, hide(true))]
    pub refresh_style: Option<RefreshStyle>,
}

/// Resolves the browser page title for preview HTML.
pub fn resolve_page_title(page_title: Option<&str>, input: Option<&str>) -> String {
    if let Some(title) = page_title {
        return title.to_owned();
    }

    input
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Typst Preview".to_string())
}

impl PreviewArgs {
    /// The compilation target the preview runs on, with the `--html`
    /// shorthand folded in.
    pub fn export_target(&self) -> ExportTarget {
        // Said outright wins over the default, and pages win over HTML: a
        // command that names both meant the narrower one.
        if self.paged_svg {
            ExportTarget::Paged
        } else if self.html {
            ExportTarget::Html
        } else {
            self.format.unwrap_or(ExportTarget::Paged)
        }
    }

    /// Whether the caller said which rendering they wanted at all.
    pub fn names_a_target(&self) -> bool {
        self.paged_svg || self.html || self.format.is_some()
    }

    /// Get the configuration for the preview.
    pub fn config(&self, config: &PreviewConfig) -> PreviewConfig {
        PreviewConfig {
            format: self.export_target(),
            enable_partial_rendering: self
                .enable_partial_rendering
                .unwrap_or(config.enable_partial_rendering),
            refresh_style: self
                .refresh_style
                .map(From::from)
                .unwrap_or_else(|| config.refresh_style.clone()),
            invert_colors: match &self.invert_colors {
                Some(s) => s.clone(),
                None => config.invert_colors.clone(),
            },
        }
    }
}

/// Specify arguments related to the preview CLI.
#[derive(Debug, Clone, clap::Parser)]
pub struct PreviewCliArgs {
    /// Configure the preview service.
    #[clap(flatten)]
    pub preview: PreviewArgs,

    /// Specify common arguments to create a world (environment) to run typst
    /// tasks.
    #[clap(flatten)]
    pub compile: CompileOnceArgs,

    /// Used by lsp for identifying the task.
    ///
    /// This is hidden from the CLI.
    #[clap(
        long = "task-id",
        default_value = "default_preview",
        value_name = "TASK_ID",
        hide(true)
    )]
    pub task_id: String,

    /// Configure the data plane server address.
    ///
    /// Note: if it equals to `static_file_host`, same address will be used.
    #[clap(
        long = "data-plane-host",
        default_value = "127.0.0.1:23625",
        value_name = "HOST",
        hide(true)
    )]
    pub data_plane_host: String,

    /// The tile colour for this server's web-app icon, as `#rrggbb`. Without
    /// it the colour comes from the port, which is stable for a given project
    /// but arbitrary; name one when a project should be recognisable.
    #[clap(long = "icon-color", value_name = "HEX")]
    pub icon_color: Option<String>,

    /// What this server calls the thing it serves, used in the web app's name
    /// ("Typst Server: Foo"). Without it the name falls back to the port.
    #[clap(long = "root-name", value_name = "NAME")]
    pub root_name: Option<String>,

    /// Which face this server wears: `preview` for an editor's own live
    /// preview, `serve` for a document served to be read, `annotate` for one
    /// served to be annotated. It decides the name, the icon, and the URL
    /// prefix everything is served under. `--annotate` implies `annotate`.
    #[clap(long = "role", value_name = "ROLE", default_value = "preview")]
    pub role: crate::tool::webapp::icons::IconRole,

    /// Also answer agents, at `/m/`: an MCP endpoint over the same port, with
    /// tools for hearing about annotations, reading the source they point at,
    /// rewriting it, and replying. The pages are unaffected.
    #[clap(long = "mcp")]
    pub mcp: bool,

    /// Give the document a window of its own: a web app installed from this
    /// URL if there is one, else the nearest thing the browser offers.
    #[clap(long = "open-isolated")]
    pub open_isolated: bool,

    /// Expose a debugging port on the opened window, so it can be inspected
    /// from outside. `auto` derives one from the server's port. Chrome only.
    #[clap(long = "open-cdp", value_name = "PORT")]
    pub open_cdp: Option<String>,

    /// Keep running after the process that started this one goes away.
    /// Without it the server exits when it is orphaned, so closing the editor
    /// (or the terminal) that launched it does not leave a server behind.
    #[clap(long = "daemon", default_value = "false", action = clap::ArgAction::SetTrue)]
    pub daemon: bool,

    /// Exit once the last browser disconnects, instead of staying up for the
    /// next one. Off by default; the editor-hosted preview is expected to
    /// outlive a closed tab, a standalone one usually is not.
    #[clap(
        long = "shutdown-on-last-client",
        alias = "exit-with-client",
        default_value = "false",
        action = clap::ArgAction::SetTrue
    )]
    pub shutdown_on_last_client: bool,

    /// Configure the control plane server address.
    #[clap(
        long = "control-plane-host",
        default_value = "127.0.0.1:23626",
        value_name = "HOST",
        hide(true)
    )]
    pub control_plane_host: String,

    /// (Deprecated) Configure (File) Host address for the preview server.
    ///
    /// Note: if it equals to `data_plane_host`, same address will be used.
    #[clap(
        long = "host",
        value_name = "HOST",
        default_value = "",
        alias = "static-file-host"
    )]
    pub static_file_host: String,

    /// Let it not be the primary instance.
    ///
    /// This is hidden from the CLI.
    #[clap(long = "not-primary", hide(true))]
    pub not_as_primary: bool,

    /// Open the preview in the browser after compilation. If `--no-open` is
    /// set, this flag will be ignored.
    #[clap(long = "open")]
    pub open: bool,

    /// Don't open the preview in the browser after compilation. If `--open` is
    /// set as well, this flag will win.
    #[clap(long = "no-open")]
    pub no_open: bool,

    /// Application to open the preview with (e.g. a Safari web app) instead of
    /// the default browser, falling back to the default browser if opening
    /// with the application fails. Defaults to an app named after the mode —
    /// "Typst Preview" or "Typst Annotate" — so a web app added to the Dock is
    /// picked up without configuration.
    #[clap(long = "open-in")]
    pub open_in: Option<String>,

    /// Emit INFO level logging. The default is WARN.
    #[clap(long = "verbose")]
    pub verbose: bool,

    /// Open the preview in annotation mode: the web view is locked to
    /// reading and writing annotations (plain click annotates; no editor
    /// following or click-to-jump).
    #[clap(long = "annotate")]
    pub annotate: bool,

    /// An extra browser origin to accept, such as `http://typst` when this
    /// server sits behind `tailscale serve`. Loopback is always accepted; the
    /// check exists to stop a random web page reaching a loopback server, so
    /// naming the origins that may reach this one is the whole relaxation.
    /// Repeat the flag for more than one.
    #[clap(long = "allowed-origin", value_name = "ORIGIN")]
    pub allowed_origins: Vec<String>,
}

impl PreviewCliArgs {
    /// Determines whether to open the preview in the browser after compilation.
    pub fn open_in_browser(&self, default: bool) -> bool {
        !self.no_open && (self.open || default)
    }

}

/// Exits when this process is orphaned — when whatever launched it is gone,
/// so is the reason to keep serving.
///
/// Polled rather than driven by the OS: the native mechanisms are per-platform
/// (kqueue's `EVFILT_PROC`/`NOTE_EXIT` on macOS and the BSDs, `PR_SET_PDEATHSIG`
/// on Linux, job objects on Windows) and this costs one `getppid` every couple
/// of seconds. Reparenting to pid 1 is the portable signal that the parent
/// died; a process already started by pid 1 (launchd, systemd) is exempt, or it
/// would exit immediately.
pub fn exit_when_orphaned() {
    #[cfg(unix)]
    {
        let original = std::os::unix::process::parent_id();
        if original <= 1 {
            log::info!("started detached: not watching for an orphaning parent");
            return;
        }
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if std::os::unix::process::parent_id() != original {
                    log::info!(
                        target: crate::PREVIEW_COMPAT_LOG_TARGET,
                        "the process that started this one is gone, shutting down"
                    );
                    std::process::exit(0);
                }
            }
        });
    }
}

/// Response for starting a preview instance.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPreviewResponse {
    static_server_port: Option<u16>,
    static_server_addr: Option<String>,
    data_plane_port: Option<u16>,
    is_primary: bool,
}

impl ServerState {
    /// Starts a background preview instance.
    pub fn background_preview(&mut self) {
        if !self.config.preview.background.enabled {
            return;
        }

        let args = self.config.preview.background.args.clone();
        let mut args = args.unwrap_or_else(|| {
            vec![
                "--data-plane-host=127.0.0.1:23635".to_string(),
                "--invert-colors=auto".to_string(),
            ]
        });

        let root = self.config.entry_resolver.root(None);

        // `--data-plane-host=HOST:auto` binds a stable per-project port derived
        // from the workspace root, so multiple editor windows each get their
        // own preview server and a project's URL survives editor restarts. If
        // the derived port is taken (e.g. the same project open in two
        // windows), fall back to an OS-assigned port.
        for arg in args.iter_mut() {
            let Some(host) = arg.strip_prefix("--data-plane-host=") else {
                continue;
            };
            let Some(host_base) = host.strip_suffix(":auto") else {
                continue;
            };
            let port = root
                .as_deref()
                .map(derive_preview_port)
                .filter(|port| std::net::TcpListener::bind((host_base, *port)).is_ok())
                .unwrap_or(0);
            *arg = format!("--data-plane-host={host_base}:{port}");
        }

        // The editor's preview follows whatever file has focus, so naming it
        // after a file would be wrong a moment later. It is the project's
        // window: name it after the project.
        if !args.iter().any(|arg| arg.starts_with("--root-name")) {
            if let Some(name) = root
                .as_deref()
                .and_then(|root| std::fs::canonicalize(root).ok())
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
            {
                args.push(format!("--root-name={name}"));
            }
        }

        let res = self.start_preview(args, PreviewKind::Background);

        // todo: looks ugly
        self.client.handle.spawn(async move {
            let fut = match res {
                Ok(fut) => fut,
                Err(e) => {
                    log::error!("failed to start background preview: {e:?}");
                    return;
                }
            };
            tokio::pin!(fut);
            let () = fut.as_mut().await;

            match fut.as_mut().take_output() {
                Some(Err(e)) => {
                    log::error!("failed to start background preview: {e:?}");
                }
                Some(Ok(resp)) => {
                    if let (Some(root), Some(addr)) = (root, resp.static_server_addr.as_deref()) {
                        announce_preview(&root, addr);
                    }
                }
                None => {}
            }
        });
    }

    /// Starts a preview instance.
    pub fn start_preview(
        &mut self,
        cli_args: Vec<String>,
        kind: PreviewKind,
    ) -> SchedulableResponse<StartPreviewResponse> {
        // clap parse
        let cli_args = ["preview"]
            .into_iter()
            .chain(cli_args.iter().map(|e| e.as_str()));
        let mut cli_args =
            PreviewCliArgs::try_parse_from(cli_args).map_err(|e| invalid_params(e.to_string()))?;
        // Unsaid means the editor's own setting: a preview started from a
        // command that names no rendering is the preview this editor asked for.
        if !cli_args.preview.names_a_target() {
            cli_args.preview.format = Some(self.config.preview.mode.into());
        }
        // default configs
        let config = cli_args.preview.config(&self.config.preview());

        // todo: preview specific arguments are not used
        let entry = cli_args.compile.input.as_ref();
        let entry = entry
            .map(|input| {
                let input = Path::new(&input);
                if !input.is_absolute() {
                    // std::env::current_dir().unwrap().join(input)
                    return Err(invalid_params("entry file must be absolute path"));
                };

                Ok(input.into())
            })
            .transpose()?;

        let task_id = cli_args.task_id.clone();
        if task_id == "primary" {
            return Err(invalid_params("task id 'primary' is reserved"));
        }

        if cli_args.not_as_primary && matches!(kind, PreviewKind::Background) {
            return Err(invalid_params(
                "cannot start background preview as non-primary",
            ));
        }

        let previewer = tinymist_preview::PreviewBuilder::new(config);
        let watcher = previewer.compile_watcher(task_id.clone());

        let primary = &mut self.project.compiler.primary;
        // todo: recover pin status reliably
        let is_browsing = matches!(kind, PreviewKind::Browsing | PreviewKind::Background);
        let is_background = matches!(kind, PreviewKind::Background);

        // An HTML preview cannot ride on the primary instance: the primary is
        // what language features are answered from, and it is paged. HTML gets
        // an instance of its own, compiled through the shim wrapper — and
        // follows the editor by being re-pointed, rather than by riding along.
        let html_mode = matches!(cli_args.preview.export_target(), ExportTarget::Html);
        let registered_as_primary = !html_mode
            && !cli_args.not_as_primary
            && (is_browsing || entry.is_some())
            && self.preview.watchers.register(&primary.id, watcher);
        if matches!(kind, PreviewKind::Background) && !registered_as_primary && !html_mode {
            return Err(invalid_params(
                "failed to register background preview to the primary instance",
            ));
        }
        if html_mode && !is_browsing && entry.is_none() {
            return Err(invalid_params("a preview needs the file to preview"));
        }

        if registered_as_primary {
            let id = primary.id.clone();

            if let Some(entry) = entry {
                self.change_main_file(Some(entry)).map_err(internal_error)?;
            }
            self.set_pin_by_preview(true, is_browsing);

            self.preview.start(
                cli_args,
                previewer,
                id,
                true,
                is_background,
                self.project.last_art.clone(),
            )
        } else if html_mode {
            // A browsing HTML preview starts on whatever has focus — which may
            // be nothing yet, in an editor that has only just opened — and is
            // re-pointed from `follow_html_previews` as focus moves.
            let entry = if is_browsing {
                entry.or_else(|| self.focusing.clone())
            } else {
                entry
            };
            let id = self
                .restart_dedicate_html(&task_id, entry.clone())
                .map_err(internal_error)?;

            if !self.project.preview.register(&id, watcher) {
                return Err(invalid_params(
                    "cannot register preview to the compiler instance",
                ));
            }
            if is_browsing {
                self.preview
                    .html_followers
                    .lock()
                    .insert(task_id.clone(), entry);
            }

            self.preview.start(
                cli_args,
                previewer,
                id,
                false,
                is_background,
                self.project.last_art.clone(),
            )
        } else if let Some(entry) = entry {
            let id = self
                .restart_dedicate(&task_id, Some(entry))
                .map_err(internal_error)?;

            if !self.project.preview.register(&id, watcher) {
                return Err(invalid_params(
                    "cannot register preview to the compiler instance",
                ));
            }

            self.preview.start(
                cli_args,
                previewer,
                id,
                false,
                is_background,
                self.project.last_art.clone(),
            )
        } else {
            Err(internal_error("entry file must be provided"))
        }
    }
}

/// Derives a stable preview port in 23700..24000 from the workspace root, so
/// each project maps to the same port across editor restarts.
fn derive_preview_port(root: &Path) -> u16 {
    // Canonical first: `/tmp/x` and `/private/tmp/x` are one project, as are
    // two spellings of the same path on a case-insensitive filesystem, and a
    // project that hashed two ways would own two ports and two dock apps.
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // FNV-1a, fixed here so ports never move across builds.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    23700 + (hash % 300) as u16
}

/// Leaves a note saying where this project's preview answers, so that anything
/// else — `talimist open-preview`, an editor task, an agent — can find it
/// without knowing how the port was arrived at.
///
/// The same register the document servers write to: one place to look for
/// "what is running", whatever kind of thing it is.
fn announce_preview(root: &Path, addr: &str) {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let Some(port) = addr.rsplit(':').next().and_then(|port| port.parse().ok()) else {
        log::warn!("cannot read a port out of the preview address {addr}");
        return;
    };
    let note = crate::tool::registry::ServerNote {
        fork: false,
        shared: None,
        server: crate::tool::registry::slug_for(&canonical),
        path: canonical.display().to_string(),
        url: format!("http://{addr}/"),
        port,
        role: "preview".into(),
        mcp: false,
        // A project, not a document: the editor's preview follows whatever file
        // has focus.
        directory: true,
        pid: std::process::id(),
        ppid: crate::tool::registry::parent_pid(),
        // The language server's own process, which is answering the editor as
        // well as drawing this: not a process to be reaped.
        hosted: true,
        started: tinymist_project::iso_now(),
    };
    match crate::tool::registry::announce_server(&note) {
        Ok(_) => log::info!("preview for {} answers at {}", canonical.display(), note.url),
        Err(err) => log::warn!("cannot announce the preview: {err}"),
    }
}

/// The global state of the preview tool.
pub struct PreviewState {
    /// Connection to the LSP client.
    client: TypedLspClient<PreviewState>,
    /// The backend running actor.
    preview_tx: mpsc::UnboundedSender<PreviewRequest>,
    /// the watchers for the preview
    pub(crate) watchers: ProjectPreviewState,
    /// Whether to send show document requests with customized notification.
    pub customized_show_document: bool,
    /// Whether the overlay channel (error overlay and/or cursor indicator) is
    /// enabled.
    pub overlay_enabled: bool,
    /// The editor's position encoding, used when preparing annotation edits.
    pub position_encoding: tinymist_query::PositionEncoding,
    /// Previews that draw HTML and follow the focused file: the task each one
    /// is, and the file it is showing.
    ///
    /// A paged preview follows the editor by riding the primary compile, which
    /// is paged because that is what language features are answered from. An
    /// HTML preview cannot ride it, so it is a project of its own — and
    /// following has to be done by hand: when the editor moves, the project is
    /// re-pointed at the file it moved to.
    pub(crate) html_followers: Arc<parking_lot::Mutex<HashMap<String, Option<ImmutPath>>>>,
}

impl PreviewState {
    /// Create a new preview state.
    pub fn new(
        config: &Config,
        watchers: ProjectPreviewState,
        client: TypedLspClient<PreviewState>,
    ) -> Self {
        let (preview_tx, preview_rx) = mpsc::unbounded_channel();

        client.handle.spawn(
            PreviewActor {
                client: client.clone().to_untyped(),
                tabs: HashMap::default(),
                preview_rx,
                watchers: watchers.clone(),
            }
            .run(),
        );

        Self {
            client,
            preview_tx,
            watchers,
            customized_show_document: config.customized_show_document,
            overlay_enabled: config.preview.error_overlay
                || config.preview.cursor_indicator
                || config.preview().invert_colors.contains("smart"),
            position_encoding: config.const_config.position_encoding,
            html_followers: Arc::default(),
        }
    }

    pub(crate) fn stop_all(&mut self) {
        log::info!("Stopping all preview tasks");

        let mut watchers = self.watchers.inner.lock();
        for (_, watcher) in watchers.iter_mut() {
            self.preview_tx
                .send(PreviewRequest::Kill(
                    watcher.task_id().to_owned(),
                    oneshot::channel().0,
                ))
                .log_error_with(|| format!("failed to send kill request({:?})", watcher.task_id()));
        }
        watchers.clear();
        self.watchers.diag.lock().clear();
    }
}

impl PreviewState {
    /// Start a preview on a given compiler.
    pub fn start(
        &self,
        args: PreviewCliArgs,
        previewer: PreviewBuilder,
        // compile_handler: Arc<CompileHandler>,
        project_id: ProjectInsId,
        is_primary: bool,
        is_background: bool,
        // Held by the caller for the compile pipeline; the previewer itself has
        // no use for it now that annotations are the document server's.
        last_art: Arc<parking_lot::Mutex<Option<tinymist_project::LspCompiledArtifact>>>,
    ) -> SchedulableResponse<StartPreviewResponse> {
        // Two renderings, two pages. Pages are drawn by the previewer at the
        // other end of the websocket; HTML is a document the page fetches, and
        // its page is the reading client — the annotator with nothing to
        // annotate.
        let html_mode = matches!(args.preview.export_target(), ExportTarget::Html);

        // Dev asset watching: reload connected pages when the overlay script
        // changes on disk.
        if self.overlay_enabled {
            let watchers = self.watchers.clone();
            let poll_id = project_id.clone();
            self.client.handle.spawn(async move {
                let mut js_mtime = None;
                let mut first = true;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let Some(diag_tx) = watchers.diag_tx(&poll_id) else {
                        break;
                    };
                    let mtime = std::fs::metadata(error_overlay::overlay_js_path())
                        .and_then(|m| m.modified())
                        .ok();
                    if mtime != js_mtime {
                        js_mtime = mtime;
                        if !first {
                            diag_tx.send_modify(|state| state.asset_version += 1);
                        }
                    }
                    first = false;
                }
            });
        }

        // The HTML page has no websocket: the compile reaches it as an event on
        // this stream, so it is not optional there.
        let diag_rx = (self.overlay_enabled || html_mode).then(|| {
            let (diag_tx, diag_rx) = tokio::sync::watch::channel(OverlayPayload::default());
            self.watchers.register_diag(&project_id, diag_tx);
            diag_rx
        });

        let compile_handler = Arc::new(ProjectPreviewHandler {
            project_id,
            client: Box::new(self.client.clone().to_untyped()),
        });

        let task_id = args.task_id.clone();
        #[cfg(feature = "open")]
        let open_in_browser = args.open_in_browser(false);
        log::info!("PreviewTask({task_id}): arguments: {args:#?}");

        if !args.static_file_host.is_empty() && (args.static_file_host != args.data_plane_host) {
            return Err(internal_error("--static-file-host is removed"));
        }

        let (lsp_tx, lsp_rx) = ControlPlaneTx::new(false);
        let ControlPlaneRx {
            resp_rx,
            ctl_tx,
            mut shutdown_rx,
        } = lsp_rx;

        let (websocket_tx, websocket_rx) = mpsc::unbounded_channel();

        let previewer = previewer.build(lsp_tx, compile_handler.clone());

        // Forward preview responses to lsp client
        let tid = task_id.clone();
        let client = self.client.clone();
        let customized_show_document = self.customized_show_document;
        self.client.handle.spawn(async move {
            let mut resp_rx = resp_rx;
            while let Some(resp) = resp_rx.recv().await {
                use tinymist_preview::ControlPlaneResponse::*;

                match resp {
                    // ignoring compile status per task.
                    CompileStatus(..) => {}
                    SyncEditorChanges(..) => {
                        log::warn!("PreviewTask({tid}): is sending SyncEditorChanges in lsp mode");
                    }
                    EditorScrollTo(s) => {
                        if customized_show_document {
                            client.send_notification::<ScrollSource>(&s)
                        } else {
                            send_show_document(&client, &s, &tid);
                        }
                    }
                    Outline(s) => client.send_notification::<NotifDocumentOutline>(&s),
                    ViewerWindowState(s) => client.send_notification::<NotifViewerWindowState>(
                        &ViewerWindowStateParams {
                            task_id: tid.clone(),
                            schema_version: s.schema_version,
                            window: s.window,
                        },
                    ),
                }
            }

            log::info!("PreviewTask({tid}): response channel closed");
        });

        // Process preview shutdown
        let tid = task_id.clone();
        let preview_tx = self.preview_tx.clone();
        self.client.handle.spawn(async move {
            // shutdown_rx
            let Some(()) = shutdown_rx.recv().await else {
                return;
            };

            log::info!("PreviewTask({tid}): internal killing");
            let (tx, rx) = oneshot::channel();
            preview_tx.send(PreviewRequest::Kill(tid.clone(), tx)).ok();
            rx.await.ok();
            log::info!("PreviewTask({tid}): internal killed");
        });

        let preview_tx = self.preview_tx.clone();
        just_future(async move {
            let mut previewer = previewer.await;
            bind_streams(&mut previewer, websocket_rx);

            // Put a fence to ensure the previewer can receive the first compilation.
            // The fence must be put after the previewer is initialized.
            compile_handler.flush_compile();

            // Replace the data plane port in the html to self
            let page_title = resolve_page_title(
                args.preview.page_title.as_deref(),
                args.compile.input.as_deref(),
            );
            let body: Option<Arc<dyn crate::tool::render::html::HtmlBody>> = html_mode.then(|| {
                Arc::new(crate::tool::render::html::ArtifactHtmlServer { last_art }) as Arc<_>
            });
            let mut frontend_html = if html_mode {
                crate::tool::render::html::shell_html()
            } else {
                frontend_html(
                    TYPST_PREVIEW_HTML,
                    args.preview.preview_mode,
                    "/",
                    &page_title,
                )
            };
            if diag_rx.is_some() && !html_mode {
                let early = format!("<script>{EARLY_ERROR_JS}</script>");
                frontend_html = match frontend_html.find("<head>") {
                    Some(at) => {
                        let mut html = frontend_html.clone();
                        html.insert_str(at + "<head>".len(), &early);
                        html
                    }
                    None => format!("{early}{frontend_html}"),
                };
                // Served per request so the script can be edited in the
                // source tree and picked up on a browser reload.
                let script = "<script src=\"/dev/overlay.js\"></script>";
                if frontend_html.contains("</body>") {
                    frontend_html = frontend_html.replace("</body>", &format!("{script}</body>"));
                } else {
                    frontend_html.push_str(script);
                }
            }

            let srv = make_http_server(
                frontend_html,
                args.data_plane_host,
                websocket_tx,
                diag_rx,
                body,
                args.allowed_origins.clone(),
            )
            .await;
            let addr = srv.addr;
            log::info!(
                target: crate::PREVIEW_COMPAT_LOG_TARGET,
                "PreviewTask({task_id}): preview server listening on: {addr}"
            );

            let resp = StartPreviewResponse {
                static_server_port: Some(addr.port()),
                static_server_addr: Some(addr.to_string()),
                data_plane_port: Some(addr.port()),
                is_primary,
            };

            #[cfg(feature = "open")]
            if open_in_browser {
                let identity = crate::tool::webapp::WebAppIdentity {
                    role: crate::tool::webapp::icons::IconRole::Lsp,
                    color: None,
                    name: args.root_name.clone(),
                };
                open::open(
                    &format!("http://127.0.0.1:{}", addr.port()),
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
                            .and_then(|spec| open::parse_cdp_port(spec, addr.port())),
                        app_title: identity.short_title(addr.port()),
                        key: addr.port(),
                    },
                );
            }

            let sent = preview_tx.send(PreviewRequest::Started(PreviewTab {
                task_id,
                previewer,
                srv,
                ctl_tx,
                compile_handler,
                is_primary,
                is_background,
            }));
            sent.map_err(|_| internal_error("failed to register preview tab"))?;

            Ok(resp)
        })
    }

    /// Kill a preview task. Ignore if the task is not found.
    pub fn kill(&self, task_id: String) -> AnySchedulableResponse {
        let (tx, rx) = oneshot::channel();

        let sent = self.preview_tx.send(PreviewRequest::Kill(task_id, tx));
        sent.map_err(|_| internal_error("failed to send kill request"))?;

        just_future(async move { rx.await.map_err(|_| internal_error("cancelled"))? })
    }

    /// Kill all preview tasks.
    pub fn kill_all(&self) -> AnySchedulableResponse {
        let (tx, rx) = oneshot::channel();

        let sent = self.preview_tx.send(PreviewRequest::KillAll(tx));
        sent.map_err(|_| internal_error("failed to send kill request"))?;

        just_future(async move { rx.await.map_err(|_| internal_error("cancelled"))? })
    }

    /// Scroll the preview to a given position.
    pub fn scroll(&self, task_id: String, req: ControlPlaneMessage) -> AnySchedulableResponse {
        let sent = self.preview_tx.send(PreviewRequest::Scroll(task_id, req));
        sent.map_err(|_| internal_error("failed to send scroll request"))?;

        just_ok(JsonValue::Null)
    }

    /// Scroll all preview panels to a given position.
    pub fn scroll_all(&self, req: ControlPlaneMessage) -> AnySchedulableResponse {
        let sent = self.preview_tx.send(PreviewRequest::ScrollAll(req));
        sent.map_err(|_| internal_error("failed to send scroll request"))?;

        just_ok(JsonValue::Null)
    }
}

struct ScrollSource;

impl Notification for ScrollSource {
    type Params = DocToSrcJumpInfo;
    const METHOD: &'static str = "tinymist/preview/scrollSource";
}

struct NotifDocumentOutline;

impl Notification for NotifDocumentOutline {
    type Params = tinymist_preview::Outline;
    const METHOD: &'static str = "tinymist/documentOutline";
}

#[derive(Serialize, Deserialize)]
struct ViewerWindowStateParams {
    task_id: String,
    schema_version: u32,
    window: ViewerWindowState,
}

struct NotifViewerWindowState;

impl Notification for NotifViewerWindowState {
    type Params = ViewerWindowStateParams;
    const METHOD: &'static str = "tinymist/preview/windowState";
}

fn send_show_document(client: &TypedLspClient<PreviewState>, s: &DocToSrcJumpInfo, tid: &str) {
    let range_start = s.start.map(|(l, c)| LspPosition {
        line: l as u32,
        character: c as u32,
    });
    let range_end = s.end.map(|(l, c)| LspPosition {
        line: l as u32,
        character: c as u32,
    });
    let range = match (range_start, range_end) {
        (Some(start), Some(end)) => Some(LspRange { start, end }),
        (Some(start), None) | (None, Some(start)) => Some(LspRange { start, end: start }),
        _ => None,
    };

    // todo: resolve uri if any
    let uri = match Url::from_file_path(Path::new(&s.filepath)) {
        Ok(uri) => uri,
        Err(e) => {
            log::error!(
                "PreviewTask({tid}): failed to convert path to URI: {e:?}, path {:?}",
                s.filepath
            );
            return;
        }
    };

    client.send_lsp_request::<lsp_types::request::ShowDocument>(
        lsp_types::ShowDocumentParams {
            uri,
            external: None,
            take_focus: Some(true),
            selection: range,
        },
        |_, resp| {
            if let Some(err) = resp.error {
                log::error!("failed to send ShowDocument request: {err:?}");
            }
        },
    );
}

/// Bind the hyper websocket streams to the previewer.
pub fn bind_streams(
    previewer: &mut Previewer,
    websocket_rx: mpsc::UnboundedReceiver<HyperWebsocket>,
) {
    previewer.start_data_plane(
        websocket_rx,
        |conn: Result<HyperWebsocketStream, hyper_tungstenite::tungstenite::Error>| {
            let conn: hyper_tungstenite::WebSocketStream<
                hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>,
            > = conn.map_err(error_once_map_string!("cannot receive websocket"))?;

            Ok(conn
                .sink_map_err(|e| error_once!("cannot serve_with websocket", err: e.to_string()))
                .map_err(|e| error_once!("cannot serve_with websocket", err: e.to_string()))
                .with(|msg| {
                    Box::pin(async move {
                        Ok(match msg {
                            WsMessage::Text(msg) => Message::text(msg),
                            WsMessage::Binary(msg) => Message::Binary(msg),
                            WsMessage::Ping(msg) => Message::Ping(msg),
                            WsMessage::Pong(msg) => Message::Pong(msg),
                        })
                    })
                })
                .map_ok(|msg| match msg {
                    Message::Text(msg) => WsMessage::Text(msg.as_str().to_owned()),
                    Message::Binary(msg) => WsMessage::Binary(msg),
                    Message::Ping(msg) => WsMessage::Ping(msg),
                    Message::Pong(msg) => WsMessage::Pong(msg),
                    Message::Close(..) => WsMessage::Text("bad_client_msg: Close".to_owned()),
                    Message::Frame(..) => WsMessage::Text("bad_client_msg: Frame".to_owned()),
                }))
        },
    );
}
