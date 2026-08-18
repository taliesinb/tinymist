//! The preview server: the page the editor's previewer talks to.
//!
//! One document, opened by an editor, rendered as pages over a websocket. It
//! answers for exactly that: the frontend, its diagnostics stream, and the
//! overlay script that draws them.
//!
//! The document *server* — a file or a directory served to a browser, with
//! annotations, captures and an agent endpoint — is `tool/serve`, which has an
//! HTTP server of its own. The two were one for a while, and everything the
//! document server needs was steadily added here; keeping this one to what a
//! previewer needs is what keeps it recognisable.

use std::net::SocketAddr;
use std::sync::LazyLock;

use hyper::header::HeaderValue;
use hyper::service::service_fn;
use hyper_tungstenite::HyperWebsocket;
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
use lsp_types::Url;
use tinymist_std::error::IgnoreLogging;
use tokio::sync::{mpsc, oneshot};

/// created by `make_http_server`
pub struct HttpServer {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    /// The sender to shutdown the server.
    pub shutdown_tx: oneshot::Sender<()>,
    /// The join handle of the server.
    pub join: tokio::task::JoinHandle<()>,
}

/// Create a http server for the previewer.
pub async fn make_http_server(
    frontend_html: String,
    static_file_addr: String,
    websocket_tx: mpsc::UnboundedSender<HyperWebsocket>,
    diag_rx: Option<super::DiagRx>,
    // The document as HTML, when that is what is being previewed. Pages are
    // drawn by the renderer at the other end of the websocket; HTML is fetched
    // from here.
    body: Option<std::sync::Arc<dyn crate::tool::render::html::HtmlBody>>,
    allowed_origins: Vec<String>,
) -> HttpServer {
    use futures::StreamExt;
    use http_body_util::{Full, StreamBody};
    use hyper::body::{Bytes, Frame, Incoming};
    type Server = hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>;
    type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::convert::Infallible>;

    fn sse_frame(payload: &super::OverlayPayload) -> Result<Frame<Bytes>, std::convert::Infallible> {
        let payload = serde_json::to_string(payload).unwrap_or_default();
        Ok(Frame::data(Bytes::from(format!("data: {payload}\n\n"))))
    }

    let listener = tokio::net::TcpListener::bind(&static_file_addr)
        .await
        .unwrap_or_else(|err| panic!("cannot bind {static_file_addr}: {err}"));
    let addr = listener.local_addr().unwrap();
    log::info!("preview server listening on http://{addr}");

    let frontend_html = hyper::body::Bytes::from(frontend_html);
    let allowed_origins = std::sync::Arc::new(allowed_origins);
    let make_service = {
        let allowed_origins = allowed_origins.clone();
        move |_peer: std::net::SocketAddr| {
        let frontend_html = frontend_html.clone();
        let websocket_tx = websocket_tx.clone();
        let static_file_addr = static_file_addr.clone();
        let diag_rx = diag_rx.clone();
        let body = body.clone();
        let allowed_origins = allowed_origins.clone();
        service_fn(move |mut req: hyper::Request<Incoming>| {
            let frontend_html = frontend_html.clone();
            let websocket_tx = websocket_tx.clone();
            let static_file_addr = static_file_addr.clone();
            let diag_rx = diag_rx.clone();
            let body = body.clone();
            let allowed_origins = allowed_origins.clone();
            async move {
                // When a user visits a website in a browser, that website can try to connect to
                // our http / websocket server on `127.0.0.1` which may leak sensitive
                // information. We could use CORS headers to explicitly disallow
                // this. However, for Websockets, this does not work. Thus, we
                // manually check the `Origin` header. Browsers always send this
                // header for cross-origin requests.
                let origin_header = req.headers().get("Origin");
                if origin_header.is_some_and(|h| {
                    !is_valid_origin(h, &static_file_addr, addr.port(), &allowed_origins)
                }) {
                    anyhow::bail!(
                        "Connection with unexpected `Origin` header. Closing connection."
                    );
                }

                let path = req.uri().path().to_owned();
                // A preview lives at `/p/`, and a page there asks for
                // everything beside it — so the prefix is stripped before
                // anything is matched. One server, one document; the segment
                // only says what kind of window this is.
                let path = path
                    .strip_prefix("/p")
                    .filter(|rest| rest.starts_with('/'))
                    .unwrap_or(&path);

                // Check if the request is a websocket upgrade request.
                if hyper_tungstenite::is_upgrade_request(&req) {
                    if origin_header.is_none() {
                        log::error!("websocket connection is not set `Origin` header, which will be a hard error in the future.");
                    }

                    let Some((response, websocket)) = hyper_tungstenite::upgrade(&mut req, None)
                        .log_error("Error in websocket upgrade")
                    else {
                        anyhow::bail!("cannot upgrade as websocket connection");
                    };

                    let _ = websocket_tx.send(websocket);

                    // Return the response so the spawned future can continue.
                    Ok(response.map(|b| Body::new(b)))
                } else if path == "/body.html" && body.is_some() {
                    // The document itself, fetched by the page rather than
                    // pushed down the websocket: HTML is a document, not a
                    // stream of drawing commands.
                    let (html, version) = body.unwrap().body();
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .header(hyper::header::ETAG, format!("\"{version}\""))
                        .body(Body::new(Full::<Bytes>::from(html)))
                        .unwrap();
                    Ok(res)
                } else if path == "/api/html/annotate.js" || path == "/api/html/annotate.css" {
                    // The reading client, which is the annotator with nothing
                    // to annotate: an editor's preview has no sidecar, and it
                    // asks for none.
                    let (asset, mime) = if path.ends_with(".js") {
                        (
                            crate::tool::render::html::client_js(),
                            "application/javascript",
                        )
                    } else {
                        (crate::tool::render::html::client_css(), "text/css")
                    };
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, mime)
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(asset)))
                        .unwrap();
                    Ok(res)
                } else if path == "/api/html/pins" {
                    // Asked for by the same client; a preview has none.
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/json")
                        .body(Body::new(Full::<Bytes>::from(
                            "{\"ok\":true,\"pins\":[]}",
                        )))
                        .unwrap();
                    Ok(res)
                } else if path == "/api/diagnostics" && diag_rx.is_some() {
                    // Stream diagnostics updates as server-sent events.
                    let rx = diag_rx.unwrap();
                    let init = rx.borrow().clone();
                    // A comment line every few seconds. Nothing reads it: it
                    // exists so writing to a browser that has gone away fails,
                    // which is the only way this connection learns it is dead
                    // during a quiet stretch with no recompiles.
                    let stream = futures::stream::once(async move { sse_frame(&init) }).chain(
                        futures::stream::unfold(rx, |mut rx| async move {
                            loop {
                                let tick = tokio::time::sleep(std::time::Duration::from_secs(3));
                                tokio::select! {
                                    changed = rx.changed() => {
                                        if changed.is_err() {
                                            return None;
                                        }
                                        let payload = rx.borrow().clone();
                                        return Some((sse_frame(&payload), rx));
                                    }
                                    _ = tick => {
                                        let ping: Result<Frame<Bytes>, std::convert::Infallible> =
                                            Ok(Frame::data(Bytes::from(":\n\n")));
                                        return Some((ping, rx));
                                    }
                                }
                            }
                        }),
                    );
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/event-stream")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(StreamBody::new(stream)))
                        .unwrap();
                    Ok(res)
                } else if path == "/api/overlay.js" && diag_rx.is_some() {
                    // Read from the source tree per request so overlay
                    // script edits apply on browser reload, no rebuild.
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/javascript")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(super::overlay_js())))
                        .unwrap();
                    Ok(res)
                } else {
                    // Anything else is the previewer's own page.
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .body(Body::new(Full::<Bytes>::from(frontend_html.clone())))
                        .unwrap();
                    Ok(res)
                }
            }
        })
        }
    };

    let (shutdown_tx, rx) = tokio::sync::oneshot::channel();
    let (final_tx, final_rx) = tokio::sync::oneshot::channel();

    // the graceful watcher
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();

    let serve_conn = move |server: &Server, graceful: &GracefulShutdown, conn| {
        let (stream, peer_addr) = match conn {
            Ok(conn) => conn,
            Err(e) => {
                log::error!("accept error: {e}");
                return;
            }
        };

        let conn =
            server.serve_connection_with_upgrades(TokioIo::new(stream), make_service(peer_addr));
        let conn = graceful.watch(conn.into_owned());
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                log_connection_error(err.as_ref());
            }
        });
    };

    let join = tokio::spawn(async move {
        // when this signal completes, start shutdown
        let mut signal = std::pin::pin!(final_rx);

        let mut server = Server::new(hyper_util::rt::TokioExecutor::new());
        server.http1().keep_alive(true);

        loop {
            tokio::select! {
                conn = listener.accept() => serve_conn(&server, &graceful, conn),
                Ok(_) = &mut signal => {
                    log::info!("graceful shutdown signal received");
                    break;
                }
            }
        }

        tokio::select! {
            _ = graceful.shutdown() => {
                log::info!("Gracefully shutdown!");
            },
            _ = tokio::time::sleep(reflexo::time::Duration::from_secs(10)) => {
                log::info!("Waited 10 seconds for graceful shutdown, aborting...");
            }
        }
    });
    tokio::spawn(async move {
        let _ = rx.await;
        final_tx.send(()).ok();
        log::info!("Preview server joined");
    });

    HttpServer {
        addr,
        shutdown_tx,
        join,
    }
}

/// Logs how a connection ended, at a level that says whose fault it was.
///
/// A client may hang up mid-response, speak TLS to a plaintext port, or send a
/// header this server cannot parse. None of those is this server failing, and
/// reporting them all at `ERROR` buries the ones that are — a refused `Origin`
/// hid among exactly this noise once already.
fn log_connection_error(err: &(dyn std::error::Error + 'static)) {
    let mut cause = Some(err);
    while let Some(err) = cause {
        if let Some(err) = err.downcast_ref::<hyper::Error>() {
            // The client's side of the conversation ended badly. Nothing here
            // is actionable, but it is worth having under `-v`.
            if err.is_incomplete_message()
                || err.is_parse()
                || err.is_canceled()
                || err.is_closed()
                || err.is_body_write_aborted()
            {
                log::debug!(
                    target: crate::PREVIEW_COMPAT_LOG_TARGET,
                    "connection ended early: {err}"
                );
                return;
            }
            break;
        }
        cause = err.source();
    }
    log::error!("cannot serve http: {err}");
}

/// Whether a configured origin is the one the browser sent.
///
/// Configured values are normalised through `Url::origin`, so `http://typst`,
/// `http://typst/`, and `http://typst:80` name one origin — which is what a
/// person writing the flag means by them, and what the browser means too.
fn origin_allows(configured: &str, origin_header: &HeaderValue) -> bool {
    let Ok(sent) = origin_header.to_str() else {
        return false;
    };
    if configured == sent {
        return true;
    }
    match (Url::parse(configured), Url::parse(sent)) {
        (Ok(configured), Ok(sent)) => configured.origin() == sent.origin(),
        _ => false,
    }
}

/// Whether a browser at this origin may talk to this server.
///
/// Shared with the document server, which is a fork of this one: the rule is
/// upstream's and there should be exactly one of it.
pub(crate) fn is_valid_origin(
    h: &HeaderValue,
    static_file_addr: &str,
    expected_port: u16,
    allowed: &[String],
) -> bool {
    static GITPOD_ID_AND_HOST: LazyLock<Option<(String, String)>> = LazyLock::new(|| {
        let workspace_id = std::env::var("GITPOD_WORKSPACE_ID").ok();
        let cluster_host = std::env::var("GITPOD_WORKSPACE_CLUSTER_HOST").ok();
        workspace_id.zip(cluster_host)
    });
    static VSCODE_PROXY_URI: LazyLock<Option<String>> =
        LazyLock::new(|| std::env::var("VSCODE_PROXY_URI").ok());

    is_valid_origin_impl(
        h,
        static_file_addr,
        expected_port,
        &GITPOD_ID_AND_HOST,
        &VSCODE_PROXY_URI,
        allowed,
    )
}

// Separate function so we can do gitpod-related tests without relying on env
// vars.
fn is_valid_origin_impl(
    origin_header: &HeaderValue,
    static_file_addr: &str,
    expected_port: u16,
    gitpod_id_and_host: &Option<(String, String)>,
    vscode_proxy_url: &Option<String>,
    allowed: &[String],
) -> bool {
    let Ok(Ok(origin_url)) = origin_header.to_str().map(Url::parse) else {
        return false;
    };

    // Path is not allowed in Origin headers
    // https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Origin
    if origin_url.path() != "/" && origin_url.path() != "" {
        return false;
    };

    let expected_origin = {
        let expected_host = Url::parse(&format!("http://{static_file_addr}")).unwrap();
        let expected_host = expected_host.host_str().unwrap();
        // Don't take the port from `static_file_addr` (it may have a dummy port e.g.
        // `127.0.0.1:0`)
        format!("http://{expected_host}:{expected_port}")
    };

    let gitpod_expected_origin = gitpod_id_and_host
        .as_ref()
        .map(|(workspace_id, cluster_host)| {
            format!("https://{expected_port}-{workspace_id}.{cluster_host}")
        });

    let vscode_expected_origin = vscode_proxy_url.as_ref().and_then(|template| {
        let url_with_port = template.replace("{{port}}", &expected_port.to_string());
        Some(
            Url::parse(&url_with_port)
                .ok()?
                .origin()
                .unicode_serialization(),
        )
    });

    *origin_header == expected_origin
        // tmistele (PR #1382): The VSCode webview panel needs an exception: It doesn't send `http://{static_file_addr}`
        // as `Origin`. Instead it sends `vscode-webview://<random>`. Thus, we allow any
        // `Origin` starting with `vscode-webview://` as well. I think that's okay from a security
        // point of view, because I think malicious websites can't trick browsers into sending
        // `vscode-webview://...` as `Origin`.
        || origin_url.scheme() == "vscode-webview"
        // `code-server` also needs an exception: It opens `http://localhost:8080/proxy/<port>` in
        // the browser and proxies requests through to tinymist (which runs at `127.0.0.1:<port>`).
        // Thus, the `Origin` header will be `http://localhost:8080` which doesn't match what
        // we expect. Thus, just always allow anything from localhost/127.0.0.1
        // https://github.com/Myriad-Dreamin/tinymist/issues/1350
        || (
            matches!(origin_url.host_str(), Some("localhost") | Some("127.0.0.1"))
            && origin_url.scheme() == "http"
        )
        // `gitpod` also needs an exception. It loads `https://<port>-<workspace>.<host>` in the browser
        // and proxies requests through to tinymist (which runs as `127.0.0.1:<port>`).
        // We can detect this by looking at the env variables (see `GITPOD_ID_AND_HOST` in `is_valid_origin(..)`)
        || gitpod_expected_origin.is_some_and(|o| o == *origin_header)
        || vscode_expected_origin.is_some_and(|o| o == *origin_header)
        // Origins named explicitly on the command line, for the case this
        // whole check did not anticipate: a server deliberately reachable at
        // some other name, e.g. behind `tailscale serve`.
        || allowed.iter().any(|a| origin_allows(a, origin_header))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_origin(origin: &'static str, static_file_addr: &str, port: u16) -> bool {
        is_valid_origin(&HeaderValue::from_static(origin), static_file_addr, port, &[])
    }

    fn check_origin_allowed(
        origin: &'static str,
        static_file_addr: &str,
        port: u16,
        allowed: &[&str],
    ) -> bool {
        let allowed: Vec<String> = allowed.iter().map(|a| (*a).to_owned()).collect();
        is_valid_origin(
            &HeaderValue::from_static(origin),
            static_file_addr,
            port,
            &allowed,
        )
    }

    #[test]
    fn test_allowed_origin_flag() {
        // The name a tailnet reaches this server by is not loopback and has no
        // port, so nothing but the flag can admit it.
        assert!(!check_origin("http://typst", "127.0.0.1:42", 42));
        assert!(check_origin_allowed(
            "http://typst",
            "127.0.0.1:42",
            42,
            &["http://typst"]
        ));
        // Written with a trailing slash, or with the port the scheme implies:
        // one origin either way.
        assert!(check_origin_allowed(
            "http://typst",
            "127.0.0.1:42",
            42,
            &["http://typst/"]
        ));
        assert!(check_origin_allowed(
            "http://typst",
            "127.0.0.1:42",
            42,
            &["http://typst:80"]
        ));
        // Several origins, and only the ones named.
        assert!(check_origin_allowed(
            "http://tbwork",
            "127.0.0.1:42",
            42,
            &["http://typst", "http://tbwork"]
        ));
        assert!(!check_origin_allowed(
            "http://elsewhere",
            "127.0.0.1:42",
            42,
            &["http://typst", "http://tbwork"]
        ));
        // A different scheme or host is a different origin, not a near miss.
        assert!(!check_origin_allowed(
            "https://typst",
            "127.0.0.1:42",
            42,
            &["http://typst"]
        ));
        assert!(!check_origin_allowed(
            "http://typst.evil.com",
            "127.0.0.1:42",
            42,
            &["http://typst"]
        ));
    }

    #[test]
    fn test_valid_origin_localhost() {
        assert!(check_origin("http://127.0.0.1:42", "127.0.0.1:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "127.0.0.1:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "127.0.0.1:0", 42));
        assert!(check_origin("http://localhost:42", "127.0.0.1:42", 42));
        assert!(check_origin("http://localhost:42", "127.0.0.1:0", 42));
        assert!(check_origin("http://localhost", "127.0.0.1:0", 42));

        assert!(check_origin("http://127.0.0.1:42", "localhost:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "localhost:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "localhost:0", 42));
        assert!(check_origin("http://localhost:42", "localhost:42", 42));
        assert!(check_origin("http://localhost:42", "localhost:0", 42));
        assert!(check_origin("http://localhost", "localhost:0", 42));
    }

    #[test]
    fn test_invalid_origin_localhost() {
        assert!(!check_origin("https://huh.io:8080", "127.0.0.1:42", 42));
        assert!(!check_origin("http://huh.io:8080", "127.0.0.1:42", 42));
        assert!(!check_origin("https://huh.io:443", "127.0.0.1:42", 42));
        assert!(!check_origin("http://huh.io:42", "127.0.0.1:0", 42));
        assert!(!check_origin("http://huh.io", "127.0.0.1:42", 42));
        assert!(!check_origin("https://huh.io", "127.0.0.1:42", 42));

        assert!(!check_origin("https://huh.io:8080", "localhost:42", 42));
        assert!(!check_origin("http://huh.io:8080", "localhost:42", 42));
        assert!(!check_origin("https://huh.io:443", "localhost:42", 42));
        assert!(!check_origin("http://huh.io:42", "localhost:0", 42));
        assert!(!check_origin("http://huh.io", "localhost:42", 42));
        assert!(!check_origin("https://huh.io", "localhost:42", 42));
    }

    #[test]
    fn test_invalid_origin_scheme() {
        assert!(!check_origin("ftp://127.0.0.1:42", "127.0.0.1:42", 42));
        assert!(!check_origin("ftp://localhost:42", "127.0.0.1:42", 42));
        assert!(!check_origin("ftp://127.0.0.1:42", "127.0.0.1:0", 42));
        assert!(!check_origin("ftp://localhost:42", "127.0.0.1:0", 42));

        // The scheme must be specified.
        assert!(!check_origin("127.0.0.1:42", "127.0.0.1:0", 42));
        assert!(!check_origin("localhost:42", "127.0.0.1:0", 42));
        assert!(!check_origin("localhost:42", "127.0.0.1:42", 42));
        assert!(!check_origin("127.0.0.1:42", "127.0.0.1:42", 42));
    }

    #[test]
    fn test_valid_origin_vscode() {
        assert!(check_origin("vscode-webview://it", "127.0.0.1:42", 42));
        assert!(check_origin("vscode-webview://it", "127.0.0.1:0", 42));
    }

    #[test]
    fn test_origin_manually_binding() {
        assert!(!check_origin("https://huh.io:8080", "huh.io:42", 42));
        assert!(!check_origin("http://huh.io:8080", "huh.io:42", 42));
        assert!(!check_origin("https://huh.io:443", "huh.io:42", 42));
        assert!(check_origin("http://huh.io:42", "huh.io:0", 42));
        assert!(!check_origin("http://huh.io", "huh.io:42", 42));
        assert!(!check_origin("https://huh.io", "huh.io:42", 42));

        assert!(check_origin("http://127.0.0.1:42", "huh.io:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "huh.io:42", 42));
        assert!(check_origin("http://127.0.0.1:42", "huh.io:0", 42));
        assert!(check_origin("http://localhost:42", "huh.io:42", 42));
        assert!(check_origin("http://localhost:42", "huh.io:0", 42));

        assert!(!check_origin("https://huh2.io:8080", "huh.io:42", 42));
        assert!(!check_origin("http://huh2.io:8080", "huh.io:42", 42));
        assert!(!check_origin("https://huh2.io:443", "huh.io:42", 42));
        assert!(!check_origin("http://huh2.io:42", "huh.io:0", 42));
        assert!(!check_origin("http://huh2.io", "huh.io:42", 42));
        assert!(!check_origin("https://huh2.io", "huh.io:42", 42));
    }

    // https://github.com/Myriad-Dreamin/tinymist/issues/1350
    // the origin of code-server's proxy
    #[test]
    fn test_valid_origin_code_server_proxy() {
        assert!(check_origin(
            // The URL has path /proxy/45411 but that is not sent in the Origin header
            "http://localhost:8080",
            "127.0.0.1:42",
            42
        ));
        assert!(check_origin("http://localhost", "127.0.0.1:42", 42));
    }

    // the origin of gitpod
    #[test]
    fn test_valid_origin_gitpod_proxy() {
        fn check_gitpod_origin(
            origin: &'static str,
            static_file_addr: &str,
            port: u16,
            workspace: &str,
            cluster_host: &str,
        ) -> bool {
            is_valid_origin_impl(
                &HeaderValue::from_static(origin),
                static_file_addr,
                port,
                &Some((workspace.to_owned(), cluster_host.to_owned())),
                &None,
                &[],
            )
        }

        let check_gitpod_origin1 = |origin: &'static str| {
            let explicit =
                check_gitpod_origin(origin, "127.0.0.1:42", 42, "workspace_id", "gitpod.typ");
            let implicit =
                check_gitpod_origin(origin, "127.0.0.1:0", 42, "workspace_id", "gitpod.typ");

            assert_eq!(explicit, implicit, "failed port binding");
            explicit
        };

        assert!(check_gitpod_origin1("http://127.0.0.1:42"));
        assert!(check_gitpod_origin1("http://127.0.0.1:42"));
        assert!(check_gitpod_origin1("https://42-workspace_id.gitpod.typ"));
        assert!(!check_gitpod_origin1(
            // A path is not allowed in Origin header
            "https://42-workspace_id.gitpod.typ/path"
        ));
        assert!(!check_gitpod_origin1(
            // Gitpod always runs on default port
            "https://42-workspace_id.gitpod.typ:42"
        ));

        assert!(!check_gitpod_origin1("https://42-workspace_id2.gitpod.typ"));
        assert!(!check_gitpod_origin1("http://huh.io"));
        assert!(!check_gitpod_origin1("https://huh.io"));
    }

    #[test]
    fn test_valid_origin_vscode_proxy() {
        fn check_vscode_origin(
            origin: &'static str,
            static_file_addr: &str,
            port: u16,
            proxy_url: &str,
        ) -> bool {
            is_valid_origin_impl(
                &HeaderValue::from_static(origin),
                static_file_addr,
                port,
                &None,
                &Some(proxy_url.to_owned()),
                &[],
            )
        }

        let check_vscode_origin1 = |origin: &'static str, url_template: &'static str| {
            let explicit = check_vscode_origin(origin, "127.0.0.1:42", 42, url_template);
            let implicit = check_vscode_origin(origin, "127.0.0.1:0", 42, url_template);

            assert_eq!(explicit, implicit, "failed port binding");
            explicit
        };

        let url_path = "https://vscode.typ/proxy/{{port}}";
        let url_subdomain = "https://{{port}}.vscode.typ";
        let url_subdomain_port = "https://{{port}}.vscode.typ:1234";

        assert!(check_vscode_origin1("http://127.0.0.1:42", url_path));
        assert!(check_vscode_origin1("http://127.0.0.1:42", url_subdomain));

        assert!(check_vscode_origin1("https://vscode.typ", url_path));
        assert!(check_vscode_origin1("https://42.vscode.typ", url_subdomain));
        assert!(check_vscode_origin1(
            "https://42.vscode.typ:1234",
            url_subdomain_port
        ));
        // Port must match
        assert!(!check_vscode_origin1(
            "https://42.vscode.typ",
            url_subdomain_port
        ));
        assert!(!check_vscode_origin1(
            "https://42.vscode.typ:1234",
            url_subdomain
        ));

        assert!(!check_vscode_origin1(
            // A path is not allowed in Origin header
            "https://42.vscode.typ/path",
            url_subdomain
        ));

        assert!(!check_vscode_origin1("http://huh.io", url_path));
    }
}
