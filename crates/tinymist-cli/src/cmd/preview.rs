use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use hyper_tungstenite::tungstenite::Message;
use tinymist::{
    PREVIEW_COMPAT_LOG_TARGET,
    project::ProjectPreviewState,
    tool::{
        preview::{PreviewCliArgs, ProjectPreviewHandler, bind_streams, make_http_server},
        project::{ProjectOpts, StartProjectResult, start_project},
    },
};
use tinymist_assets::TYPST_PREVIEW_HTML;
use tinymist_preview::{
    ControlPlaneMessage, ControlPlaneTx, PreviewBuilder, PreviewConfig, frontend_html,
};
use tinymist_project::WorldProvider;
use tinymist_std::error::prelude::*;
use tinymist_task::ExportTarget;
use tokio::sync::mpsc;


/// Entry point of the preview tool.
pub async fn preview_main(mut args: PreviewCliArgs) -> Result<()> {
    log::info!("Arguments: {args:#?}");
    let handle = tokio::runtime::Handle::current();

    // Render only the visible pages unless asked otherwise. A 24-page document
    // otherwise puts its whole self in the DOM — measured at 104k SVG elements,
    // 72k of them <use>, and 7.9MB of markup, which Safari cannot scroll
    // smoothly; with partial rendering the same document is 4.5k elements and
    // every frame lands in 17ms.
    // Annotation included: without it the renderer re-renders the whole
    // document on every scroll (`render_in_window ... 0 0 1e33 1e33`, hundreds
    // of milliseconds a time), which repaints the view and reads as flashing.
    // Page groups keep their DOM position and their number under partial
    // rendering — the off-screen ones become canvas-backed dummies — so the
    // overlay's page lookup is unaffected.
    if args.preview.enable_partial_rendering.is_none() {
        args.preview.enable_partial_rendering = Some(true);
    }
    let config = args.preview.config(&PreviewConfig::default());
    #[cfg(feature = "open")]
    // `preview` is usually run to look at something now; `annotate` is
    // typically driven by an editor task that opens its own window.
    let open_in_browser = args.open_in_browser(!args.annotate);
    #[cfg(feature = "open")]
    let open_in_override = args.open_in.clone();
    #[cfg(feature = "open")]
    let open_isolated = args.open_isolated;
    #[cfg(feature = "open")]
    let open_cdp = args.open_cdp.clone();
    let static_file_host =
        if args.static_file_host == args.data_plane_host || !args.static_file_host.is_empty() {
            Some(args.static_file_host)
        } else {
            None
        };

    tinymist::tool::webapp::note_build_stamp();
    // Ctrl-C and `kill` are how a server is usually stopped, and neither runs a
    // destructor: caught so that what this one leaves behind — its note in the
    // register, the site it rendered — goes with it.
    crate::utils::tidy_up_on_signals();

    let shutdown_on_last_client = args.shutdown_on_last_client;
    let mcp = args.mcp;
    // `annotate` and `preview` are the same server wearing different faces.
    let cli_role = if args.annotate {
        tinymist::tool::webapp::icons::IconRole::Annotate
    } else {
        args.role
    };
    let identity = tinymist::tool::webapp::WebAppIdentity {
        role: cli_role,
        color: args
            .icon_color
            .as_deref()
            .and_then(tinymist::tool::webapp::icons::parse_hex),
        // Falling back to the document's own name: a port tells nobody which
        // project a window belongs to.
        name: args.root_name.clone().or_else(|| {
            args.compile
                .input
                .as_deref()
                .and_then(|path| std::path::Path::new(path).file_name())
                .map(|name| name.to_string_lossy().into_owned())
        }),
    };
    if args.icon_color.is_some() && identity.color.is_none() {
        log::warn!("--icon-color is not a hex colour; falling back to the port's");
    }
    if !args.daemon {
        tinymist::tool::preview::exit_when_orphaned();
    }
    // Which rendering, independent of who is driving: an editor following a
    // maths paper wants the pages a PDF would have, and one following technical
    // documentation wants the HTML, which is faster to compile and to draw.
    let preview_target = args.preview.export_target();
    let html_mode = matches!(preview_target, ExportTarget::Html);
    let mut verse = args.compile.resolve()?;
    if html_mode {
        // The same shims a served document gets: what HTML export drops, put
        // back in front of the document rather than in it.
        if let Err(err) = tinymist::tool::render::html::install_shims(&mut verse) {
            log::warn!("previewing without the HTML export shims: {err}");
        }
    }
    let previewer = PreviewBuilder::new(config);

    let (service, handle, diag_rx, body) = {
        let preview_state = ProjectPreviewState::default();
        let last_art = Arc::new(parking_lot::Mutex::default());
        // In HTML mode the page fetches the document rather than being drawn
        // into over the websocket, and this is what answers it.
        let body: Option<Arc<dyn tinymist::tool::render::html::HtmlBody>> =
            html_mode.then(|| {
                Arc::new(tinymist::tool::render::html::ArtifactHtmlServer {
                    last_art: last_art.clone(),
                }) as Arc<_>
            });
        let mut opts = ProjectOpts {
            handle: Some(handle),
            preview: preview_state.clone(),
            export_target: preview_target,
            last_art: last_art.clone(),
            ..ProjectOpts::default()
        };
        // Propagate `--invert-colors=smart` into the shared config so the
        // compile handler reports the rendered page appearance per compile.
        if args.preview.invert_colors.as_deref() == Some("smart") {
            opts.config.preview.invert_colors = tinymist_preview::PreviewInvertColors::Enum(
                tinymist_preview::PreviewInvertColor::Smart,
            );
        }

        let StartProjectResult {
            service,
            intr_tx,
            mut editor_rx,
        } = start_project(verse, Some(opts), |compiler, intr, next| {
            next(compiler, intr)
        });

        // Consume editor_rx
        tokio::spawn(async move { while editor_rx.recv().await.is_some() {} });

        let id = service.compiler.primary.id.clone();
        let registered = preview_state.register(&id, previewer.compile_watcher(args.task_id));
        if !registered {
            tinymist_std::bail!("failed to register preview");
        }

        // The error overlay: compile diagnostics are pushed to the frontend
        // over an SSE channel and shown on the last successful render.
        let (diag_tx, diag_rx) = tokio::sync::watch::channel(
            tinymist::tool::preview::OverlayPayload::default(),
        );
        preview_state.register_diag(&id, diag_tx);

        // The overlay script is read from the source tree on every request, so
        // a page reloads when it changes on disk.
        {
            let watchers = preview_state.clone();
            let poll_id = id.clone();
            tokio::spawn(async move {
                let mut js_mtime = None;
                let mut first = true;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let Some(diag_tx) = watchers.diag_tx(&poll_id) else {
                        break;
                    };
                    let mtime = std::fs::metadata(tinymist::tool::preview::overlay_js_path())
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

        let handle: Arc<ProjectPreviewHandler> = Arc::new(ProjectPreviewHandler {
            project_id: id,
            client: Box::new(intr_tx),
        });

        (service, handle, diag_rx, body)
    };

    let (lsp_tx, mut lsp_rx) = ControlPlaneTx::new(true);

    let control_plane_server_handle = tokio::spawn(async move {
        let (control_sock_tx, mut control_sock_rx) = mpsc::unbounded_channel();

        let srv =
            make_http_server(
                String::default(),
                args.control_plane_host,
                control_sock_tx,
                None,
                // The control plane carries no document: it is the editor's
                // channel, and it serves no pages.
                None,
                // It serves the editor over loopback, so the origins a browser
                // might reach the data plane by are none of its business.
                Vec::new(),
            )
            .await;
        log::info!(
            target: PREVIEW_COMPAT_LOG_TARGET,
            "Control panel server listening on: {}",
            srv.addr
        );

        let control_websocket = control_sock_rx.recv().await.unwrap();
        let ws = control_websocket.await.unwrap();

        tokio::pin!(ws);

        loop {
            tokio::select! {
                Some(resp) = lsp_rx.resp_rx.recv() => {
                    let r = ws
                        .send(Message::text(serde_json::to_string(&resp).unwrap()))
                        .await;
                    let Err(err) = r else {
                        continue;
                    };

                    log::warn!("failed to send response to editor {err:?}");
                    break;

                }
                msg = ws.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(msg))) => Some(msg),
                        Some(Ok(Message::Binary(..))) =>{
                            log::error!("unsupported binary message");
                            break;
                        }
                        Some(Ok(Message::Ping(..))) =>{
                            log::error!("unsupported ping message");
                            break;
                        }
                        Some(Ok(Message::Pong(..))) =>{
                            log::error!("unsupported pong message");
                            break;
                        }
                        Some(Ok(Message::Close(..))) =>{
                            log::error!("unsupported close message");
                            break;
                        }
                        Some(Ok(Message::Frame(..))) =>{
                            log::error!("unsupported frame message");
                            break;
                        }
                        Some(Err(e)) => {
                            log::error!("failed to receive message: {e}");
                            break;
                        }
                        _ => None,
                    };

                    if let Some(msg) = msg {
                        let Ok(msg) = serde_json::from_str::<ControlPlaneMessage>(&msg) else {
                            log::warn!("failed to parse control plane request: {msg:?}");
                            break;
                        };

                        lsp_rx.ctl_tx.send(msg).unwrap();
                    } else {
                        // todo: inform the editor that the connection is closed.
                        break;
                    }
                }

            }
        }

        let _ = srv.shutdown_tx.send(());
        let _ = srv.join.await;
    });

    let (websocket_tx, websocket_rx) = mpsc::unbounded_channel();
    let mut previewer = previewer.build(lsp_tx, handle.clone()).await;
    tokio::spawn(service.run());

    bind_streams(&mut previewer, websocket_rx);

    let page_title = tinymist::tool::preview::resolve_page_title(
        args.preview.page_title.as_deref(),
        args.compile.input.as_deref(),
    );
    // Two renderings, two pages. Pages are drawn into by the renderer at the
    // other end of the websocket; HTML is a document the page fetches, and its
    // page is the reading client.
    let mut frontend_html = if html_mode {
        tinymist::tool::render::html::shell_html()
    } else {
        frontend_html(
            TYPST_PREVIEW_HTML,
            args.preview.preview_mode,
            "/",
            &page_title,
        )
    };
    if !html_mode {
        // A trap for a render that never arrives: without it a page that fails
        // early is a blank one, with nothing to say why.
        let early = format!(
            "<script>{}</script>",
            tinymist::tool::preview::EARLY_ERROR_JS
        );
        frontend_html = match frontend_html.find("<head>") {
            Some(at) => {
                let mut html = frontend_html.clone();
                html.insert_str(at + "<head>".len(), &early);
                html
            }
            None => format!("{early}{frontend_html}"),
        };
        let script = "<script src=\"/dev/overlay.js\"></script>";
        if frontend_html.contains("</body>") {
            frontend_html = frontend_html.replace("</body>", &format!("{script}</body>"));
        } else {
            frontend_html.push_str(script);
        }
    }

    // Bound once: `args` is partially moved into the servers below, and both
    // of them accept the same origins.
    let allowed_origins = args.allowed_origins.clone();

    let static_server = if let Some(static_file_host) = static_file_host {
        log::warn!(
            "--static-file-host is deprecated, which will be removed in the future. Use --data-plane-host instead."
        );
        let html = frontend_html.clone();
        Some(
            make_http_server(
                html,
                static_file_host,
                websocket_tx.clone(),
                Some(diag_rx.clone()),
                body.clone(),
                allowed_origins.clone(),
            )
            .await,
        )
    } else {
        None
    };

    let srv = make_http_server(
        frontend_html,
        args.data_plane_host,
        websocket_tx,
        Some(diag_rx),
        body,
        allowed_origins,
    )
    .await;
    log::info!(
        target: PREVIEW_COMPAT_LOG_TARGET,
        "Data plane server listening on: {}",
        srv.addr
    );

    let static_server_addr = static_server.as_ref().map(|s| s.addr).unwrap_or(srv.addr);
    log::info!(
        target: PREVIEW_COMPAT_LOG_TARGET,
        "Static file server listening on: {static_server_addr}"
    );



    #[cfg(feature = "open")]
    if open_in_browser {
        let path = tinymist::tool::webapp::role_prefix(identity.role);
        // The app to look for is the one this server would be added to the
        // Dock as — its manifest's short name — so opening lands in the window
        // that already belongs to this document rather than a stray browser
        // tab. Falls back to the plain browser, and then to the system
        // default, rather than failing.
        use tinymist::tool::preview::open;
        let port = static_server_addr.port();
        open::open(
            &format!("http://{static_server_addr}{path}"),
            &open::OpenOptions {
                browser: open_in_override
                    .as_deref()
                    .map(open::Browser::parse)
                    .unwrap_or(open::Browser::Default),
                isolated: open_isolated,
                cdp_port: open_cdp
                    .as_deref()
                    .and_then(|spec| open::parse_cdp_port(spec, port)),
                app_title: identity.short_title(port),
                key: port,
            },
        );
    }

    // Printed rather than logged, and printed last: this process binds two
    // ports and only one of them is meant for a person. A log line for each is
    // how the wrong one gets copied.
    {
        let path = tinymist::tool::webapp::role_prefix(identity.role);
        let port = static_server_addr.port();
        println!();
        println!("{}", identity.title(port));
        println!("  document  http://{static_server_addr}{path}");
        if mcp {
            // The line to paste into an agent's configuration.
            println!("  agents    http://{static_server_addr}/m/");
        }
        #[cfg(feature = "open")]
        if let Some(cdp) = open_cdp
            .as_deref()
            .and_then(|spec| tinymist::tool::preview::open::parse_cdp_port(spec, port))
        {
            println!("  devtools  http://127.0.0.1:{cdp}");
        }
        println!();
    }

    let _ = tokio::join!(previewer.join(), srv.join, control_plane_server_handle);
    // Assert that the static server's lifetime is longer than the previewer.
    let _s = static_server;

    Ok(())
}
