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

use crate::utils::exit_on_ctrl_c;

/// Entry point of the preview tool.
pub async fn preview_main(args: PreviewCliArgs) -> Result<()> {
    log::info!("Arguments: {args:#?}");
    let handle = tokio::runtime::Handle::current();

    let config = args.preview.config(&PreviewConfig::default());
    #[cfg(feature = "open")]
    let open_in_browser = args.open_in_browser(true);
    let static_file_host =
        if args.static_file_host == args.data_plane_host || !args.static_file_host.is_empty() {
            Some(args.static_file_host)
        } else {
            None
        };

    exit_on_ctrl_c();

    let preview_target = args.preview.format;
    if matches!(preview_target, ExportTarget::Bundle) {
        bail!("bundle export target is not supported by preview");
    }
    let verse = args.compile.resolve()?;
    let previewer = PreviewBuilder::new(config);

    let (service, handle, diag_rx, annot) = {
        let preview_state = ProjectPreviewState::default();
        let last_art = Arc::new(parking_lot::Mutex::default());
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

        // Annotations: the CLI has no editor, so all annotation edits are
        // written to disk directly.
        let annot: Arc<dyn tinymist::tool::preview::AnnotationServer> =
            Arc::new(tinymist::tool::preview::DiskAnnotationServer {
                last_art: last_art.clone(),
                watchers: preview_state.clone(),
                project_id: id.clone(),
                // In annotate mode, stream annotation events as JSON lines
                // on stdout so a driving agent can react to them.
                emit_events: args.annotate,
            });

        // The sidecar is not a compile dependency; poll its mtime so
        // external edits (e.g. an agent updating a status) refresh the pins.
        {
            let watchers = preview_state.clone();
            let poll_id = id.clone();
            let poll_art = last_art.clone();
            tokio::spawn(async move {
                let mut last_mtime = None;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let Some(diag_tx) = watchers.diag_tx(&poll_id) else {
                        break;
                    };
                    let Some(art) = poll_art.lock().clone() else {
                        continue;
                    };
                    let Some(path) = tinymist::tool::preview::sidecar_path(&art) else {
                        continue;
                    };
                    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                    if mtime == last_mtime {
                        continue;
                    }
                    last_mtime = mtime;
                    let pins = tinymist::tool::preview::annotation_pins(&art);
                    diag_tx.send_modify(|state| state.annotations = pins);
                }
            });
        }

        let handle: Arc<ProjectPreviewHandler> = Arc::new(ProjectPreviewHandler {
            project_id: id,
            client: Box::new(intr_tx),
        });

        (service, handle, diag_rx, annot)
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
                None,
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
    let mut frontend_html = frontend_html(
        TYPST_PREVIEW_HTML,
        args.preview.preview_mode,
        "/",
        &page_title,
    );
    let script = "<script src=\"/dev/overlay.js\"></script>";
    if frontend_html.contains("</body>") {
        frontend_html = frontend_html.replace("</body>", &format!("{script}</body>"));
    } else {
        frontend_html.push_str(script);
    }

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
                Some(annot.clone()),
            )
            .await,
        )
    } else {
        None
    };

    let srv =
        make_http_server(
            frontend_html,
            args.data_plane_host,
            websocket_tx,
            Some(diag_rx),
            Some(annot),
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
        let query = if args.annotate { "/?annotate" } else { "" };
        tinymist::tool::preview::open_preview_url(
            format!("http://{static_server_addr}{query}"),
            args.open_in.as_deref(),
        );
    }

    let _ = tokio::join!(previewer.join(), srv.join, control_plane_server_handle);
    // Assert that the static server's lifetime is longer than the previewer.
    let _s = static_server;

    Ok(())
}
