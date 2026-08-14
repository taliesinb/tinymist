//! Document preview tool for Typst

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
    site: std::sync::Arc<dyn crate::tool::serve::DocumentSite>,
    shutdown_on_last_client: bool,
    identity: super::WebAppIdentity,
    allowed_origins: Vec<String>,
) -> HttpServer {
    use futures::StreamExt;
    use http_body_util::{Full, StreamBody};
    use hyper::body::{Bytes, Frame, Incoming};
    type Server = hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>;
    type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::convert::Infallible>;

    /// One open page, counted for as long as its event stream lives. Page
    /// count is the honest measure of "is anyone there": a browser keeps idle
    /// TCP connections pooled long after the tab that opened them is gone, but
    /// it tears down the event stream immediately.
    struct ClientGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>, usize);
    impl Drop for ClientGuard {
        fn drop(&mut self) {
            let left = self
                .0
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
                .saturating_sub(1);
            tinymist_project::announce(
                "client_disconnected",
                &[("id", self.1.into()), ("connected", left.into())],
            );
        }
    }

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
    // Icons and manifests are keyed to the port this server answers on.
    let port = addr.port();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let served_anyone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let identity = std::sync::Arc::new(identity);
    let allowed_origins = std::sync::Arc::new(allowed_origins);
    // Clients are numbered from zero as they arrive, so the line that says one
    // has gone can name which one it was.
    let next_client = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let site = site.clone();
    let make_service = {
        let site = site.clone();
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        let identity = identity.clone();
        let allowed_origins = allowed_origins.clone();
        let next_client = next_client.clone();
        move |peer: std::net::SocketAddr| {
        let frontend_html = frontend_html.clone();
        let websocket_tx = websocket_tx.clone();
        let static_file_addr = static_file_addr.clone();
        let site = site.clone();
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        let identity = identity.clone();
        let allowed_origins = allowed_origins.clone();
        let next_client = next_client.clone();
        service_fn(move |mut req: hyper::Request<Incoming>| {
            let identity = identity.clone();
            let allowed_origins = allowed_origins.clone();
            let frontend_html = frontend_html.clone();
            let websocket_tx = websocket_tx.clone();
            let static_file_addr = static_file_addr.clone();
            let site = site.clone();
            let next_client = next_client.clone();
            let live = live.clone();
            let served_anyone = served_anyone.clone();
            async move {
                // When a user visits a website in a browser, that website can try to connect to
                // our http / websocket server on `127.0.0.1` which may leak sensitive
                // information. We could use CORS headers to explicitly disallow
                // this. However, for Websockets, this does not work. Thus, we
                // manually check the `Origin` header. Browsers always send this
                // header for cross-origin requests.
                //
                // Important: This does _not_ protect against malicious users that share the
                // same computer as us (i.e. multi- user systems where the users
                // don't trust each other). In this case, malicious attackers can _still_
                // connect to our http / websocket servers (using a browser and
                // otherwise). And additionally they can impersonate a tinymist
                // http / websocket server towards a legitimate frontend/html client.
                // This requires additional protection that may be added in the future.
                let origin_header = req.headers().get("Origin");
                if origin_header.is_some_and(|h| {
                    !is_valid_origin(h, &static_file_addr, addr.port(), &allowed_origins)
                }) {
                    anyhow::bail!(
                        "Connection with unexpected `Origin` header. Closing connection."
                    );
                }

                log::info!(
                    target: crate::PREVIEW_COMPAT_LOG_TARGET,
                    "{} {} ua={:?}",
                    req.method(),
                    req.uri().path(),
                    req.headers()
                        .get(hyper::header::USER_AGENT)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-"),
                );
                // Which document this request is about, and what it asks of
                // it. A directory names the document in the first segment
                // after the mode's prefix — `/a/paper/dev/html/doc` — and a
                // single document leaves it out, so the same dispatch serves
                // both and every endpoint sits under the page that uses it.
                let raw_path = req.uri().path().to_owned();
                let listing = site.is_listing();
                let (page_role, slug, path) = match super::role_of_path(&raw_path) {
                    // The site's own endpoints — the listing's data, chiefly —
                    // are not a document called `dev`.
                    Some((role, rest)) if listing && rest.starts_with("dev/") => {
                        (Some(role), String::new(), format!("/{rest}"))
                    }
                    Some((role, rest)) if listing => match rest.split_once('/') {
                        Some((slug, tail)) => (Some(role), slug.to_owned(), format!("/{tail}")),
                        None => (Some(role), rest.to_owned(), String::new()),
                    },
                    Some((role, rest)) => (Some(role), String::new(), format!("/{rest}")),
                    // Assets and the paged frontend's own endpoints are named
                    // absolutely, from a page that is always the only one.
                    None => (None, String::new(), raw_path.clone()),
                };
                let path = path.as_str();
                // Building a document is compiling it, so it happens when one
                // is asked for and not before: a directory of thirty papers is
                // thirty compilers otherwise, to show a list of names.
                let wants_doc = page_role.is_some() || path.starts_with("/dev/");
                let doc = if wants_doc && !(listing && slug.is_empty()) {
                    site.services(&slug).await
                } else {
                    None
                };
                let diag_rx = doc.as_ref().and_then(|d| d.diag_rx.clone());
                let annot = doc.as_ref().and_then(|d| d.annot.clone());
                let html = doc.as_ref().and_then(|d| d.html.clone());

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
                } else if let Some(icon) = super::icon_asset(&raw_path, port, &identity) {
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "image/png")
                        .header(hyper::header::CACHE_CONTROL, "max-age=3600")
                        .body(Body::new(Full::<Bytes>::from(icon)))
                        .unwrap();
                    Ok(res)
                } else if let Some(manifest) = super::web_manifest(&raw_path, port, &identity) {
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/manifest+json")
                        .body(Body::new(Full::<Bytes>::from(manifest)))
                        .unwrap();
                    Ok(res)
                } else if let (Some(role), true) = (page_role, path.is_empty() || path == "/") {
                    if tinymist_project::announcing() {
                        // The id a page will answer to: a page opens its event
                        // stream as soon as it loads, and takes the next number
                        // when it does, so a request and the client it becomes
                        // read as one story.
                        let id = next_client.load(std::sync::atomic::Ordering::SeqCst);
                        tinymist_project::announce(
                            "client_requested",
                            &[("id", id.into()), ("url", raw_path.clone().into())],
                        );
                    }
                    // The mode rides in the URL's first segment, so each has its
                    // own manifest, icon and dock app — and the rest of the path
                    // names the document, which is how one server comes to serve
                    // a directory.
                    let identity = identity.with_role(role);
                    if listing && slug.is_empty() {
                        // The directory's own front page. It holds no documents
                        // — it asks for them — so it is the same page whatever
                        // is in the directory, and can be an asset like the
                        // annotator's own script and stylesheet.
                        let body = super::mode_head(&crate::tool::serve::listing_html(), &identity, port);
                        return Ok(hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, "text/html")
                            .body(Body::new(Full::<Bytes>::from(body)))
                            .unwrap());
                    }
                    if doc.is_none() {
                        return Ok(hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from(format!(
                                "no document named {slug}\n"
                            ))))
                            .unwrap());
                    }
                    // Every endpoint a page uses sits under the page's own URL,
                    // which is only a base to resolve them against if it ends
                    // in a slash.
                    if path.is_empty() {
                        return Ok(hyper::Response::builder()
                            .status(hyper::StatusCode::MOVED_PERMANENTLY)
                            .header(hyper::header::LOCATION, format!("{raw_path}/"))
                            .body(Body::new(Full::<Bytes>::default()))
                            .unwrap());
                    }
                    let named = doc
                        .as_ref()
                        .map(|d| d.title.clone())
                        .filter(|title| !title.is_empty())
                        .map(|title| identity.with_name(title));
                    let identity = named.unwrap_or(identity);
                    let page = super::mode_head(
                        std::str::from_utf8(&frontend_html).unwrap_or_default(),
                        &identity,
                        port,
                    );
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/html")
                        .body(Body::new(Full::<Bytes>::from(page)))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/docs" && listing {
                    // What the listing page draws: the directory as it is now.
                    let began = std::time::Instant::now();
                    let entries = site.listing();
                    tinymist_project::announce(
                        "compiled_listing",
                        &[
                            ("path", site.root().display().to_string().into()),
                            ("file_count", entries.len().into()),
                            ("elapsed", began.elapsed().as_secs_f64().into()),
                        ],
                    );
                    let body =
                        crate::tool::serve::listing_json(&identity.title(port), site.root(), &entries);
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/json")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/diagnostics" && diag_rx.is_some() {
                    // Stream diagnostics updates as server-sent events.
                    let rx = diag_rx.unwrap();
                    let init = rx.borrow().clone();
                    use std::sync::atomic::Ordering::SeqCst;
                    let now = live.fetch_add(1, SeqCst) + 1;
                    served_anyone.store(true, SeqCst);
                    let id = next_client.fetch_add(1, SeqCst);
                    let guard = ClientGuard(live.clone(), id);
                    if tinymist_project::announcing() {
                        // Who, from the same header the annotations take their
                        // author from: behind `tailscale serve` that is the
                        // tailnet login, and on loopback it is whoever is
                        // running the server.
                        let headers = req.headers();
                        let name =
                            super::annotations::author_or_local(request_author(headers).as_deref());
                        let agent = headers
                            .get(hyper::header::USER_AGENT)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        // The address the request came from: a proxy's own, if
                        // one forwarded it, else the connection's far end.
                        let ip = headers
                            .get("X-Forwarded-For")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.split(',').next())
                            .map(|value| value.trim().to_owned())
                            .unwrap_or_else(|| peer.ip().to_string());
                        tinymist_project::announce(
                            "client_connected",
                            &[
                                ("id", id.into()),
                                ("name", name.into()),
                                ("useragent", agent.into()),
                                ("ip", ip.into()),
                                ("connected", now.into()),
                            ],
                        );
                    }
                    // A comment line every few seconds. Nothing reads it: it
                    // exists so writing to a browser that has gone away fails,
                    // which is the only way this connection learns it is dead
                    // during a quiet stretch with no recompiles.
                    let stream = futures::stream::once(async move { sse_frame(&init) }).chain(
                        futures::stream::unfold((rx, guard), |(mut rx, guard)| async move {
                            loop {
                                let tick = tokio::time::sleep(std::time::Duration::from_secs(3));
                                tokio::select! {
                                    changed = rx.changed() => {
                                        changed.ok()?;
                                        let payload = rx.borrow_and_update().clone();
                                        return Some((sse_frame(&payload), (rx, guard)));
                                    }
                                    _ = tick => {
                                        return Some((
                                            Ok(Frame::data(Bytes::from_static(b": ping\n\n"))),
                                            (rx, guard),
                                        ));
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
                } else if path == "/dev/overlay.js" && diag_rx.is_some() {
                    // Read from the source tree per request so overlay
                    // script edits apply on browser reload, no rebuild.
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/javascript")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(super::overlay_js())))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/build" {
                    // Which build is answering. A server whose binary has been
                    // replaced is on its way out but still holds its socket for
                    // a moment; whoever is starting up needs to know that the
                    // address it just probed is not the one it would reuse.
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .header(hyper::header::CACHE_CONTROL, "no-store")
                        .body(Body::new(Full::<Bytes>::from(super::build_stamp())))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/clientlog" {
                    // Frontend errors: logged to stderr and appended to a
                    // well-known file so they can be found after the fact.
                    use http_body_util::BodyExt;
                    let body = req.into_body().collect().await?.to_bytes();
                    let text = String::from_utf8_lossy(&body).to_string();
                    log::error!(target: "tinymist::preview::client", "{text}");
                    // A fixed, findable path (not the sandboxed per-user
                    // temp dir): /tmp/tinymist-preview-client.log.
                    let path = std::path::Path::new("/tmp/tinymist-preview-client.log");
                    if let Ok(mut file) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        use std::io::Write;
                        let _ = writeln!(file, "{text}");
                    }
                    let res = hyper::Response::builder()
                        .status(hyper::StatusCode::OK)
                        .body(Body::new(Full::<Bytes>::default()))
                        .unwrap();
                    Ok(res)
                } else if path == "/body.html" && html.is_some() {
                    // The rendered document, from disk: written once when it
                    // compiled, and read by everyone who asks for it. The
                    // version it was written at is its entity tag, so a browser
                    // that already has this rendering is told so and keeps it.
                    let html = html.unwrap();
                    let (body, version) = html.body();
                    let tag = format!("\"{version}\"");
                    let known = req
                        .headers()
                        .get(hyper::header::IF_NONE_MATCH)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value == tag);
                    let res = if known {
                        hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_MODIFIED)
                            .header(hyper::header::ETAG, tag)
                            .body(Body::new(Full::<Bytes>::default()))
                            .unwrap()
                    } else {
                        hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, "text/html")
                            .header(hyper::header::CACHE_CONTROL, "no-cache")
                            .header(hyper::header::ETAG, tag)
                            .body(Body::new(Full::<Bytes>::from(body)))
                            .unwrap()
                    };
                    Ok(res)
                } else if path == "/dev/html/annotate.js" || path == "/dev/html/annotate.css" {
                    // The client itself, which is the same for every document
                    // and for the listing: asked for without one, and read from
                    // the source tree per request so an edit applies on reload.
                    let (body, mime) = if path.ends_with(".js") {
                        (
                            super::html_annotations::client_js(),
                            "application/javascript",
                        )
                    } else {
                        (super::html_annotations::client_css(), "text/css")
                    };
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, mime)
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap();
                    Ok(res)
                } else if path.starts_with("/dev/html/") && html.is_some() {
                    // HTML mode's own endpoints. The document arrives as a
                    // fragment with every piece labelled with the source range
                    // it came from; the annotations arrive as source offsets.
                    // Geometry is the browser's business here, so none is sent.
                    let html = html.unwrap();
                    let (body, mime) = match path {
                        "/dev/html/annotate.js" => (
                            super::html_annotations::client_js(),
                            "application/javascript",
                        ),
                        "/dev/html/annotate.css" => {
                            (super::html_annotations::client_css(), "text/css")
                        }
                        "/dev/html/doc" => {
                            let payload = match html.document() {
                                Ok(doc) => {
                                    let frag = super::html_annotations::fragment(&doc);
                                    serde_json::json!({
                                        "ok": true,
                                        "title": frag.title,
                                        "body": frag.body,
                                    })
                                }
                                Err(err) => serde_json::json!({"ok": false, "error": err}),
                            };
                            (payload.to_string(), "application/json")
                        }
                        "/dev/html/pins" => {
                            let payload = serde_json::json!({
                                "ok": true,
                                "pins": html.pins(),
                            });
                            (payload.to_string(), "application/json")
                        }
                        other => (
                            serde_json::json!({
                                "ok": false,
                                "error": format!("unknown endpoint: {other}"),
                            })
                            .to_string(),
                            "application/json",
                        ),
                    };
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, mime)
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/annotations.js" && annot.is_some() {
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/javascript")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(super::annotations_js())))
                        .unwrap();
                    Ok(res)
                } else if path.starts_with("/dev/annotate") && annot.is_some() {
                    // Annotation endpoints: POST /dev/annotate creates an
                    // annotation at a clicked position; POST
                    // /dev/annotate/delete removes one by id.
                    use http_body_util::BodyExt;
                    #[derive(serde::Deserialize)]
                    struct UuidReq {
                        uuid: String,
                        #[serde(default)]
                        text: String,
                        #[serde(default)]
                        status: String,
                    }
                    let path = path.to_owned();
                    let annot = annot.unwrap();
                    // Resolved before the body is consumed, and imposed on the
                    // request afterwards: whatever the client claimed about
                    // authorship is not consulted.
                    let author = request_author(req.headers());
                    let body = req.into_body().collect().await?.to_bytes();
                    let parse_uuid = || {
                        serde_json::from_slice::<UuidReq>(&body).map_err(|e| e.to_string())
                    };
                    let outcome = match path.as_str() {
                        "/dev/annotate" => serde_json::from_slice::<super::AnnotateRequest>(&body)
                            .map_err(|e| e.to_string())
                            .and_then(|mut req| {
                                req.author = author.clone();
                                annot.annotate(req)
                            }),
                        "/dev/annotate/delete" => parse_uuid()
                            .and_then(|req| annot.remove(&req.uuid).map(|()| String::new())),
                        "/dev/annotate/reply" => parse_uuid().and_then(|req| {
                            annot
                                .reply(&req.uuid, &req.text, author.as_deref())
                                .map(|()| String::new())
                        }),
                        "/dev/annotate/status" => parse_uuid().and_then(|req| {
                            annot
                                .set_status(&req.uuid, &req.status)
                                .map(|()| String::new())
                        }),
                        "/dev/annotate/layout" => {
                            let (status, body) = match annot.layout() {
                                Ok(map) => (
                                    hyper::StatusCode::OK,
                                    serde_json::to_string(&serde_json::json!({
                                        "ok": true,
                                        "blocks": map.blocks,
                                    }))
                                    .unwrap_or_default(),
                                ),
                                Err(err) => (
                                    hyper::StatusCode::BAD_REQUEST,
                                    serde_json::json!({ "ok": false, "error": err })
                                        .to_string(),
                                ),
                            };
                            let res = hyper::Response::builder()
                                .status(status)
                                .header(hyper::header::CONTENT_TYPE, "application/json")
                                .header(hyper::header::CACHE_CONTROL, "no-cache")
                                .body(Body::new(Full::<Bytes>::from(body)))
                                .unwrap();
                            return Ok(res);
                        }
                        "/dev/annotate/words" => {
                            #[derive(serde::Deserialize)]
                            struct WordsReq {
                                s: usize,
                                e: usize,
                            }
                            let outcome = serde_json::from_slice::<WordsReq>(&body)
                                .map_err(|e| e.to_string())
                                .and_then(|req| annot.words(req.s, req.e));
                            let (status, body) = match outcome {
                                Ok(words) => (
                                    hyper::StatusCode::OK,
                                    serde_json::to_string(&serde_json::json!({
                                        "ok": true, "words": words,
                                    }))
                                    .unwrap_or_default(),
                                ),
                                Err(err) => (
                                    hyper::StatusCode::BAD_REQUEST,
                                    serde_json::json!({ "ok": false, "error": err })
                                        .to_string(),
                                ),
                            };
                            let res = hyper::Response::builder()
                                .status(status)
                                .header(hyper::header::CONTENT_TYPE, "application/json")
                                .body(Body::new(Full::<Bytes>::from(body)))
                                .unwrap();
                            return Ok(res);
                        }
                        "/dev/annotate/probe" => {
                            #[derive(serde::Deserialize)]
                            struct ProbeReq {
                                page: usize,
                                x: f64,
                                y: f64,
                                // A second point makes it a drag: the probe
                                // reports the span it would create.
                                #[serde(default)]
                                page2: Option<usize>,
                                #[serde(default)]
                                x2: Option<f64>,
                                #[serde(default)]
                                y2: Option<f64>,
                            }
                            let outcome = serde_json::from_slice::<ProbeReq>(&body)
                                .map_err(|e| e.to_string())
                                .and_then(|req| {
                                    match (req.page2, req.x2, req.y2) {
                                        (Some(p2), Some(x2), Some(y2)) => annot.probe_span(
                                            (req.page, req.x, req.y),
                                            (p2, x2, y2),
                                        ),
                                        _ => annot.probe(req.page, req.x, req.y),
                                    }
                                });
                            let (status, body) = match outcome {
                                Ok(pos) => (
                                    hyper::StatusCode::OK,
                                    serde_json::json!({
                                        "ok": true,
                                        "scope": pos.scope,
                                        "page": pos.page, "x": pos.x, "y": pos.y,
                                        "rects": pos.rects,
                                    })
                                    .to_string(),
                                ),
                                Err(err) => (
                                    hyper::StatusCode::BAD_REQUEST,
                                    serde_json::json!({ "ok": false, "error": err })
                                        .to_string(),
                                ),
                            };
                            let res = hyper::Response::builder()
                                .status(status)
                                .header(hyper::header::CONTENT_TYPE, "application/json")
                                .body(Body::new(Full::<Bytes>::from(body)))
                                .unwrap();
                            return Ok(res);
                        }
                        _ => Err(format!("unknown annotation endpoint: {path}")),
                    };
                    let (status, body) = match outcome {
                        Ok(uuid) => (
                            hyper::StatusCode::OK,
                            serde_json::json!({ "ok": true, "uuid": uuid }).to_string(),
                        ),
                        Err(err) => (
                            hyper::StatusCode::BAD_REQUEST,
                            serde_json::json!({ "ok": false, "error": err }).to_string(),
                        ),
                    };
                    let res = hyper::Response::builder()
                        .status(status)
                        .header(hyper::header::CONTENT_TYPE, "application/json")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap();
                    Ok(res)
                } else {
                    // Anything else is a page asked for without a mode: send it
                    // into this server's own, keeping whatever it named.
                    let target = format!(
                        "{}{}",
                        super::role_prefix(identity.role),
                        raw_path.trim_start_matches('/')
                    );
                    let res = hyper::Response::builder()
                        .status(hyper::StatusCode::FOUND)
                        .header(hyper::header::LOCATION, target)
                        .body(Body::new(Full::<Bytes>::default()))
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

    // Serving nobody: when the last browser goes away the server has no reason
    // to outlive it. A short grace period covers a page reload, which drops
    // every connection for a moment before opening new ones.
    const IDLE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
    if shutdown_on_last_client {
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering::SeqCst;
            loop {
                tokio::time::sleep(IDLE_GRACE).await;
                log::debug!(
                    target: crate::PREVIEW_COMPAT_LOG_TARGET,
                    "idle check: pages={} served={}",
                    live.load(SeqCst),
                    served_anyone.load(SeqCst)
                );
                if !served_anyone.load(SeqCst) || live.load(SeqCst) > 0 {
                    continue;
                }
                // Still nobody a whole grace period later: this was not a reload.
                tokio::time::sleep(IDLE_GRACE).await;
                if live.load(SeqCst) == 0 {
                    log::info!(
                        target: crate::PREVIEW_COMPAT_LOG_TARGET,
                        "last client disconnected, shutting down"
                    );
                    std::process::exit(0);
                }
            }
        });
    }

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

/// The identity a request carries, when it has one.
///
/// `tailscale serve` authenticates the caller against the tailnet and injects
/// the result, so a document served that way knows who is writing without
/// asking anyone to log in. A direct loopback request has no such header, and
/// the caller is whoever is running the server — see `author_or_local`.
///
/// Read from the headers and never from the body: the point of the header is
/// that a proxy the client cannot forge sets it.
fn request_author(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get("Tailscale-User-Login")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|login| !login.is_empty())
        .map(str::to_owned)
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

fn is_valid_origin(
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
