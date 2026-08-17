//! The document server's HTTP face.
//!
//! Everything a served document answers: the annotator's page and its assets,
//! the rendered body, the annotation endpoints, the sidecar view, stored
//! captures, the event log, the listing, and the agent endpoint at `/m/`.
//!
//! A fork of the previewer's server, which is what this grew out of and is
//! deliberately no longer part of: a previewer serves one document to one
//! editor over a websocket, and none of the above is any of its business.

use std::net::SocketAddr;

use hyper::service::service_fn;
use hyper_tungstenite::HyperWebsocket;
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
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
    site: std::sync::Arc<dyn super::DocumentSite>,
    shutdown_on_last_client: bool,
    // Whether the machine face is served at `/m/`.
    mcp: bool,
    identity: crate::tool::webapp::WebAppIdentity,
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
            super::http::LIVE_CLIENTS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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

    fn sse_frame(payload: &crate::tool::preview::OverlayPayload) -> Result<Frame<Bytes>, std::convert::Infallible> {
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
    // When an agent last called. A page says it is there by holding its event
    // stream open, which is what "is anyone there" counts; an agent asks a
    // question and goes away to think about the answer, so it says it is there
    // by having asked recently.
    let started = std::time::Instant::now();
    let last_call = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // The listing as it was last reported, so a page asking again every few
    // seconds does not say the same thing every few seconds.
    let last_listing = std::sync::Arc::new(parking_lot::Mutex::new(None::<String>));
    let site = site.clone();
    let make_service = {
        let last_call = last_call.clone();
        let site = site.clone();
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        let identity = identity.clone();
        let allowed_origins = allowed_origins.clone();
        let next_client = next_client.clone();
        let last_listing = last_listing.clone();
        move |peer: std::net::SocketAddr| {
        let last_call = last_call.clone();
        let frontend_html = frontend_html.clone();
        let websocket_tx = websocket_tx.clone();
        let static_file_addr = static_file_addr.clone();
        let site = site.clone();
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        let identity = identity.clone();
        let allowed_origins = allowed_origins.clone();
        let next_client = next_client.clone();
        let last_listing = last_listing.clone();
        service_fn(move |mut req: hyper::Request<Incoming>| {
            let identity = identity.clone();
            let allowed_origins = allowed_origins.clone();
            let last_call = last_call.clone();
            let frontend_html = frontend_html.clone();
            let websocket_tx = websocket_tx.clone();
            let static_file_addr = static_file_addr.clone();
            let site = site.clone();
            let next_client = next_client.clone();
            let last_listing = last_listing.clone();
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
                    !crate::tool::preview::http::is_valid_origin(h, &static_file_addr, addr.port(), &allowed_origins)
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
                // Read back from what a browser sent: a document called
                // 道德經 arrives as `%E9%81%93%E5%BE%B7%E7%B6%93`, and a file of
                // that name is not found by looking for one called `%E9%81%93…`.
                //
                // Decoding is also how a path climbs out of what is being
                // served — `%2e%2e%2f` is `../`, which the raw path could not
                // contain — so a decoded path that says `..` anywhere is
                // refused here rather than trusted to be caught later.
                let raw_path = unescape(req.uri().path(), false);
                if raw_path
                    .split('/')
                    .any(|part| part == ".." || part.contains('\0'))
                {
                    return Ok(hyper::Response::builder()
                        .status(hyper::StatusCode::BAD_REQUEST)
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .body(Body::new(Full::<Bytes>::from("no\n")))
                        .unwrap());
                }
                let listing = site.is_listing();
                // What a path under a directory names is the directory's own
                // business: a document may be several directories down, and
                // what is not a document may be a file to hand over or a
                // directory to list.
                let mut located = None;
                let (page_role, slug, path) = match crate::tool::webapp::role_of_path(&raw_path) {
                    // The site's own endpoints — the listing's data, chiefly —
                    // are not a document called `dev`.
                    Some((role, rest)) if listing && rest.starts_with("dev/") => {
                        (Some(role), String::new(), format!("/{rest}"))
                    }
                    Some((role, rest)) if listing => {
                        let found = site.locate(rest);
                        let answer = match &found {
                            super::Located::Document { slug, tail } => {
                                (Some(role), slug.clone(), format!("/{tail}"))
                            }
                            _ => (Some(role), String::new(), String::new()),
                        };
                        located = Some(found);
                        answer
                    }
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
                // A request that names no document — an asset, the listing —
                // has none of these; one that names a document has all of them.
                let diag_rx = doc.as_ref().map(|d| d.diag_rx.clone());
                let annot = doc.as_ref().map(|d| d.annot.clone());
                let html = doc.as_ref().map(|d| d.body.clone());

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
                } else if raw_path == crate::tool::webapp::CSS_ROUTE {
                    // The reader's own stylesheet, read from disk on every
                    // request so an edit to it needs no restart. Not announced:
                    // it is furniture, asked for once per page.
                    let said = crate::tool::webapp::injected_css()
                        .and_then(|path| std::fs::read(path).ok());
                    let res = match said {
                        Some(bytes) => hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, "text/css; charset=utf-8")
                            .header(hyper::header::CACHE_CONTROL, "no-cache")
                            .body(Body::new(Full::<Bytes>::from(bytes)))
                            .unwrap(),
                        None => hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from("no stylesheet\n")))
                            .unwrap(),
                    };
                    Ok(res)
                } else if let Some(icon) = crate::tool::webapp::icon_asset(&raw_path, port, &identity) {
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "image/png")
                        .header(hyper::header::CACHE_CONTROL, "max-age=3600")
                        .body(Body::new(Full::<Bytes>::from(icon)))
                        .unwrap();
                    Ok(res)
                } else if let Some(manifest) = crate::tool::webapp::web_manifest(&raw_path, port, &identity) {
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/manifest+json")
                        .body(Body::new(Full::<Bytes>::from(manifest)))
                        .unwrap();
                    Ok(res)
                } else if let (Some(role), true) = (page_role, path.is_empty() || path == "/") {
                    if tinymist_project::announcing() {
                        // A page was asked for. Nothing is connected yet and
                        // may never be — a listing page opens no event stream —
                        // so this carries no id: an id is for a client that can
                        // be counted, and counting one that never arrives is
                        // what keeps a server alive for nobody.
                        tinymist_project::announce(
                            "client_requested",
                            &[
                                ("url", raw_path.clone().into()),
                                ("ip", peer.ip().to_string().into()),
                                (
                                    "name",
                                    request_author(req.headers())
                                        .map(Into::into)
                                        .unwrap_or(serde_json::Value::Null),
                                ),
                            ],
                        );
                    }
                    // The mode rides in the URL's first segment, so each has its
                    // own manifest, icon and dock app — and the rest of the path
                    // names the document, which is how one server comes to serve
                    // a directory.
                    let identity = identity.with_role(role);
                    // A file the directory holds — a PDF, a picture, a note —
                    // is handed over as it is.
                    if let Some(super::Located::File(file)) = &located {
                        return Ok(match file_bytes(file) {
                            Some((bytes, mime)) => hyper::Response::builder()
                                .header(hyper::header::CONTENT_TYPE, mime)
                                .header(hyper::header::CACHE_CONTROL, "no-cache")
                                .body(Body::new(Full::<Bytes>::from(bytes)))
                                .unwrap(),
                            None => hyper::Response::builder()
                                .status(hyper::StatusCode::NOT_FOUND)
                                .header(hyper::header::CONTENT_TYPE, "text/plain")
                                .body(Body::new(Full::<Bytes>::from("cannot read that file\n")))
                                .unwrap(),
                        });
                    }
                    if let Some(super::Located::Missing) = &located {
                        return Ok(hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from(format!(
                                "nothing here: {raw_path}\n"
                            ))))
                            .unwrap());
                    }
                    if listing && slug.is_empty() {
                        // The directory's own front page, and the same page for
                        // every directory under it. It holds no documents — it
                        // asks for them — so it can be an asset like the
                        // annotator's own script and stylesheet.
                        //
                        // Its stylesheet is named at the mount rather than
                        // beside the page: the same page is served at every
                        // depth, and a relative name at `/a/report/` would ask
                        // for a stylesheet inside a directory. A document's own
                        // page keeps the relative name, since there the tail
                        // after the document *is* the endpoint.
                        let prefix = crate::tool::webapp::public_prefix(role);
                        let page = super::listing_html()
                            .replace("href=\"dev/", &format!("href=\"{prefix}dev/"))
                            .replace("src=\"dev/", &format!("src=\"{prefix}dev/"));
                        let body = crate::tool::webapp::mode_head(&page, &identity, port, true);
                        return Ok(hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
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
                    let page = crate::tool::webapp::mode_head(
                        std::str::from_utf8(&frontend_html).unwrap_or_default(),
                        &identity,
                        port,
                        false,
                    );
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .body(Body::new(Full::<Bytes>::from(page)))
                        .unwrap();
                    Ok(res)
                } else if raw_path == "/m" || raw_path.starts_with("/m/") {
                    // The machine face, beside the pages rather than instead of
                    // them: a person reads `/a/`, an agent calls `/m/`.
                    if !mcp {
                        let res = hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from(
                                "this server was not started with --mcp\n",
                            )))
                            .unwrap();
                        return Ok(res);
                    }
                    if req.method() != hyper::Method::POST {
                        // The stream a client may open to be spoken to first is
                        // not offered: everything here is asked for, including
                        // waiting for something to happen.
                        let res = hyper::Response::builder()
                            .status(hyper::StatusCode::METHOD_NOT_ALLOWED)
                            .header(hyper::header::ALLOW, "POST")
                            .body(Body::new(Full::<Bytes>::default()))
                            .unwrap();
                        return Ok(res);
                    }
                    use http_body_util::BodyExt;
                    last_call.store(
                        started.elapsed().as_secs().max(1),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    let body = req.into_body().collect().await?.to_bytes();
                    let request: serde_json::Value =
                        serde_json::from_slice(&body).unwrap_or_default();
                    let answer = match request {
                        // A batch, which the protocol allows.
                        serde_json::Value::Array(calls) => {
                            let mut answers = vec![];
                            for call in calls {
                                if let Some(answer) =
                                    crate::tool::mcp::tools::handle(&site, call).await
                                {
                                    answers.push(answer);
                                }
                            }
                            (!answers.is_empty())
                                .then(|| serde_json::Value::Array(answers))
                        }
                        one => crate::tool::mcp::tools::handle(&site, one).await,
                    };
                    let res = match answer {
                        Some(answer) => hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, "application/json")
                            .body(Body::new(Full::<Bytes>::from(answer.to_string())))
                            .unwrap(),
                        // A notification is answered with silence, which over
                        // HTTP is an empty acceptance.
                        None => hyper::Response::builder()
                            .status(hyper::StatusCode::ACCEPTED)
                            .body(Body::new(Full::<Bytes>::default()))
                            .unwrap(),
                    };
                    Ok(res)
                } else if raw_path == "/dev/stop" {
                    // Asked to stop, which is not the same as being killed: the
                    // rendered site is cleared up and the register is told.
                    // Loopback only, like everything else that writes here.
                    log::info!(
                        target: crate::PREVIEW_COMPAT_LOG_TARGET,
                        "asked to stop by {peer}"
                    );
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/plain")
                        .body(Body::new(Full::<Bytes>::from("stopping\n")))
                        .unwrap();
                    tokio::spawn(async {
                        // After the answer has gone out.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        super::shutdown("asked to stop");
                    });
                    Ok(res)
                } else if path == "/dev/events" {
                    // What the server has said, with numbers on it. A client
                    // asks with the last id it saw and waits for the next: a
                    // loop is then the length of what happened, not of how
                    // often it asked.
                    let query = req.uri().query().unwrap_or("");
                    let param = |name: &str| -> Option<String> {
                        query.split('&').find_map(|pair| {
                            let (key, value) = pair.split_once('=')?;
                            (key == name).then(|| value.to_owned())
                        })
                    };
                    let since = param("since").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                    let wait = param("wait")
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(0)
                        .min(300);
                    let (mut events, mut cursor) = tinymist_project::events_since(since);
                    if events.is_empty() && wait > 0 {
                        let mut signal = tinymist_project::event_signal();
                        let waited = tokio::time::timeout(
                            std::time::Duration::from_secs(wait),
                            signal.changed(),
                        )
                        .await;
                        if waited.is_ok() {
                            let fresh = tinymist_project::events_since(since);
                            events = fresh.0;
                            cursor = fresh.1;
                        }
                    }
                    let payload = serde_json::json!({
                        "ok": true,
                        "events": events,
                        "cursor": cursor,
                    });
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/json")
                        .header(hyper::header::CACHE_CONTROL, "no-store")
                        .body(Body::new(Full::<Bytes>::from(payload.to_string())))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/docs" && listing {
                    // What the listing page draws: the directory as it is now.
                    //
                    // The page asks again every few seconds, and saying so
                    // every time buries everything else the server has to say.
                    // Only a listing that has changed is worth a line.
                    let began = std::time::Instant::now();
                    // Which directory: the one being served, or one under it,
                    // named by the query since the path is the endpoint's.
                    let under = req
                        .uri()
                        .query()
                        .and_then(|query| {
                            query
                                .split('&')
                                .find_map(|pair| pair.strip_prefix("under="))
                                .map(|value| unescape(value, true))
                        })
                        .unwrap_or_default();
                    let dir = match site.locate(&under) {
                        super::Located::Listing(dir) => dir,
                        _ => site.root().to_path_buf(),
                    };
                    let entries = super::entries_in(&dir);
                    let seen = format!(
                        "{}:{}",
                        dir.display(),
                        entries
                            .iter()
                            .map(|entry| format!("{}@{}", entry.slug, entry.annotations))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    if last_listing.lock().replace(seen.clone()).as_deref() != Some(seen.as_str()) {
                        tinymist_project::announce(
                            "listed",
                            &[
                                ("path", dir.display().to_string().into()),
                                ("file_count", entries.len().into()),
                                ("elapsed", began.elapsed().as_secs_f64().into()),
                            ],
                        );
                    }
                    let body = super::listing_json_of(
                        &identity.title(port),
                        &dir,
                        &entries,
                        &super::dirs_in(&dir),
                        &super::assets_in(&dir),
                        &under,
                    );
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
                    LIVE_CLIENTS.fetch_add(1, SeqCst);
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
                            crate::tool::serve::annotations::author_or_local(request_author(headers).as_deref());
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
                        .body(Body::new(Full::<Bytes>::from(crate::tool::preview::overlay_js())))
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
                        .body(Body::new(Full::<Bytes>::from(crate::tool::webapp::build_stamp())))
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
                } else if path == "/sidecar" && annot.is_some() {
                    // The sidecar as a page: what the server thinks it has,
                    // annotation by annotation, captures and all. A debugging
                    // view, so it is rendered fresh every time.
                    let annot = annot.clone().unwrap();
                    let named = doc
                        .as_ref()
                        .map(|doc| doc.title.clone())
                        .unwrap_or_default();
                    let title = if named.is_empty() {
                        site.root()
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "document".into())
                    } else {
                        named
                    };
                    let where_ = if slug.is_empty() {
                        site.root().display().to_string()
                    } else {
                        format!("{}/{slug}.typ", site.root().display())
                    };
                    let body = super::sidecar::page(
                        &format!("{title} — sidecar"),
                        &where_,
                        &annot,
                    );
                    Ok(hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .header(hyper::header::CACHE_CONTROL, "no-store")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap())
                } else if let Some(name) = path.strip_prefix("/dev/capture/") {
                    // One stored capture, as it was stored. The page shows them
                    // as images, and an SVG is already one.
                    let stored = name.rsplit_once('.').and_then(|(hash, fmt)| {
                        let bytes = super::capture::read(hash, fmt)?;
                        let mime = match fmt {
                            "svg" => "image/svg+xml",
                            "png" => "image/png",
                            _ => "application/octet-stream",
                        };
                        Some((bytes, mime))
                    });
                    Ok(match stored {
                        Some((bytes, mime)) => hyper::Response::builder()
                            .header(hyper::header::CONTENT_TYPE, mime)
                            // Named by the hash of what is in it, so it can be
                            // held onto for as long as the browser likes.
                            .header(hyper::header::CACHE_CONTROL, "max-age=86400, immutable")
                            .body(Body::new(Full::<Bytes>::from(bytes)))
                            .unwrap(),
                        None => hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from("no such capture\n")))
                            .unwrap(),
                    })
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
                            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
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
                            crate::tool::render::html::client_js(),
                            "application/javascript",
                        )
                    } else {
                        (crate::tool::render::html::client_css(), "text/css")
                    };
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, mime)
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(body)))
                        .unwrap();
                    Ok(res)
                } else if path == "/dev/html/relocate" && annot.is_some() {
                    // Locations a page is holding against a rendering that has
                    // been replaced, expressed against the one it is showing
                    // now. A page asks after a compile; what it holds is what
                    // it has not managed to send.
                    use http_body_util::BodyExt;
                    #[derive(serde::Deserialize)]
                    struct Ask {
                        render: String,
                        #[serde(default)]
                        locations: Vec<tinymist_annos::HtmlLocation>,
                    }
                    let annot = annot.unwrap();
                    let body = req.into_body().collect().await?.to_bytes();
                    let asked = serde_json::from_slice::<Ask>(&body)
                        .map_err(|err| err.to_string())
                        .and_then(|ask| annot.relocate(&ask.render, &ask.locations));
                    let payload = match asked {
                        Ok(locations) => serde_json::json!({
                            "ok": true,
                            "locations": locations,
                        }),
                        Err(err) => serde_json::json!({"ok": false, "error": err}),
                    };
                    let res = hyper::Response::builder()
                        .header(hyper::header::CONTENT_TYPE, "application/json")
                        .header(hyper::header::CACHE_CONTROL, "no-cache")
                        .body(Body::new(Full::<Bytes>::from(payload.to_string())))
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
                            crate::tool::render::html::client_js(),
                            "application/javascript",
                        ),
                        "/dev/html/annotate.css" => {
                            (crate::tool::render::html::client_css(), "text/css")
                        }
                        "/dev/html/doc" => {
                            // The rendering and its map together, in one
                            // answer: a page that fetched them separately could
                            // pair a body with the map of a different compile.
                            let payload = match html.rendering() {
                                Ok((doc, map)) => {
                                    let frag = crate::tool::render::html::fragment(&doc);
                                    serde_json::json!({
                                        "ok": true,
                                        "render": map.render,
                                        "title": frag.title,
                                        // What the exporter put in the head,
                                        // which the page has to carry since it
                                        // takes only the body.
                                        "style": frag.style,
                                        "body": frag.body,
                                        "map": map,
                                    })
                                }
                                Err(err) => serde_json::json!({
                                    "ok": false,
                                    "waiting": err == crate::tool::render::html::NOT_COMPILED,
                                    "error": err,
                                }),
                            };
                            (payload.to_string(), "application/json")
                        }
                        "/dev/html/pins" => {
                            // Annotations are the server's, not the page's: a
                            // document rendered for an editor has none, and the
                            // client is content with an empty list.
                            let pins = annot.as_ref().map(|annot| annot.pins());
                            let payload = serde_json::json!({
                                "ok": true,
                                "pins": pins.unwrap_or_default(),
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
                } else if path.starts_with("/dev/annotate") && annot.is_some() {
                    // Annotation endpoints: POST /dev/annotate creates an
                    // annotation at a clicked position; POST
                    // /dev/annotate/delete removes one by id.
                    use http_body_util::BodyExt;
                    // A server started with a latency answers these slowly on
                    // purpose: what a page does while it waits — the mark that
                    // stands in for an annotation until the server has it — is
                    // otherwise only visible on a machine slow enough to see.
                    let wait = crate::tool::serve::annotate_latency();
                    if wait > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    }
                    #[derive(serde::Deserialize)]
                    struct UuidReq {
                        uuid: String,
                        #[serde(default)]
                        text: String,
                        /// Whether somebody is on it; absent leaves it alone.
                        #[serde(default)]
                        claimed: Option<bool>,
                        /// Whether it is done; absent leaves it alone.
                        #[serde(default)]
                        resolved: Option<bool>,
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
                        "/dev/annotate" => serde_json::from_slice::<crate::tool::serve::AnnotateRequest>(&body)
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
                        "/dev/annotate/flags" => parse_uuid().and_then(|req| {
                            annot
                                .set_flags(&req.uuid, req.claimed, req.resolved)
                                .map(|()| String::new())
                        }),
                        other => Err(format!("no such endpoint: {other}")),
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
                    // into this server's own, keeping whatever it named. A path
                    // that already names a mode is not sent anywhere — it named
                    // something this server does not have, and redirecting it
                    // into the mode it is already in is how a browser ends up
                    // following `/a/a/a/…` until it gives up.
                    if crate::tool::webapp::role_of_path(&raw_path).is_some() {
                        return Ok(hyper::Response::builder()
                            .status(hyper::StatusCode::NOT_FOUND)
                            .header(hyper::header::CONTENT_TYPE, "text/plain")
                            .body(Body::new(Full::<Bytes>::from(format!(
                                "nothing here: {raw_path}\n"
                            ))))
                            .unwrap());
                    }
                    let target = format!(
                        "{}{}",
                        crate::tool::webapp::public_prefix(identity.role),
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
    // How long an empty server waits before going, which depends on what is
    // going on in it. A closed window usually means the reader is done, and ten
    // seconds is enough to tell that from a reload. But a document an agent has
    // been asked about is one it may come back to, and a document with an
    // annotation somebody has claimed is one being worked on right now — the
    // claim is the difference between a pause and an ending, and it is dropped
    // as soon as the annotation is resolved or released.
    const CLOSED: u64 = 10;
    const AGENT_HERE: u64 = 60;
    const AGENT_WORKING: u64 = 30 * 60;
    if shutdown_on_last_client {
        let live = live.clone();
        let served_anyone = served_anyone.clone();
        let last_call = last_call.clone();
        let site = site.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering::SeqCst;
            let mut empty_since: Option<std::time::Instant> = None;
            loop {
                tokio::time::sleep(IDLE_GRACE).await;
                if !served_anyone.load(SeqCst) {
                    continue;
                }
                if live.load(SeqCst) > 0 {
                    empty_since = None;
                    continue;
                }
                let empty = *empty_since.get_or_insert_with(std::time::Instant::now);
                let called = last_call.load(SeqCst) > 0;
                let grace = if called && super::anyone_working(&site.sidecars()) {
                    AGENT_WORKING
                } else if called {
                    AGENT_HERE
                } else {
                    CLOSED
                };
                if empty.elapsed().as_secs() < grace {
                    continue;
                }
                super::shutdown(&format!("nobody reading for {grace}s"));
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

/// How many readers are connected, across every document this process serves.
///
/// A page holds its event stream open for as long as it is open, so the number
/// of open streams is the number of readers. Counted per process rather than
/// per document, which is what a caller asking "is anyone reading this" wants.
static LIVE_CLIENTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The count, for whoever asks.
pub fn live_clients() -> usize {
    LIVE_CLIENTS.load(std::sync::atomic::Ordering::SeqCst)
}

/// A file from the directory being served, as it is.
///
/// A directory of documents is also where their pictures and their exports
/// live, and a link to one is not a request to compile anything. Read whole:
/// these are a few hundred kilobytes at most, and streaming them would mean
/// holding a file open across an await for no gain.
fn file_bytes(path: &std::path::Path) -> Option<(Vec<u8>, &'static str)> {
    let bytes = std::fs::read(path).ok()?;
    Some((bytes, mime_of(path)))
}

/// What a file says it is, by its extension. Anything unrecognised is offered
/// as bytes, which a browser will download rather than mangle.
fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("csv") => "text/csv; charset=utf-8",
        Some("md" | "txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// A query value with its escapes read back.
///
/// `plus_is_space` is the difference between the two places this is used: in a
/// query a `+` stands for a space, and in a path it is a plus, which is a
/// character a file name may have.
fn unescape(value: &str, plus_is_space: bool) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        at += 3;
                    }
                    None => {
                        out.push(bytes[at]);
                        at += 1;
                    }
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                at += 1;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
