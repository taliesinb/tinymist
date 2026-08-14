//! `talimist mcp`: the one address an agent is told about.
//!
//! A document server's port is derived from its path, which makes it permanent
//! but not discoverable, and an agent may be asked about a document whose
//! server is not running at all. So agents are pointed at one fixed address
//! instead, and this is what answers there: it reads the register of running
//! servers, forwards each call to whichever one it names, and can start one
//! that is not up — with a window, if a person should be watching.
//!
//! It holds no documents and compiles nothing. Everything it can do, one of
//! the servers does; this is where they can be found.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::tool::registry::{self, ServerNote};

/// Where the hub answers, always. Above the range document servers derive
/// their ports from, so that reserving it takes nothing away from them: the
/// point of a derived port is that a document keeps the URL it had yesterday.
pub const HUB_PORT: u16 = 25000;

/// The tools the hub owns, as opposed to those it forwards.
fn hub_tools() -> Value {
    json!([
        {
            "name": "list_servers",
            "title": "List document servers",
            "description": "The document servers running now: a short name for each, what it \
                            is serving, and the documents in it. Everything else takes one of \
                            these names as `server`. Start here.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false,
            },
        },
        {
            "name": "open_document",
            "title": "Open a document or directory",
            "description": "Serves a file or directory that nothing is serving yet, and \
                            returns the server's name. Say show: true to open a window on it, \
                            which is how to show a person what you are working on.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The .typ file, or a directory of them."},
                    "show": {"type": "boolean", "description": "Open a browser window on it as well."},
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        },
        {
            "name": "close_document",
            "title": "Close a document server",
            "description": "Stops a server this session started. A server somebody is reading \
                            in a browser is better left alone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "The server's name, from list_servers."},
                },
                "required": ["server"],
                "additionalProperties": false,
            },
        },
    ])
}

/// How a server describes itself to an agent.
fn server_json(note: &ServerNote) -> Value {
    let documents = if note.directory {
        registry::get(note.port, "/a/dev/docs")
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|listing| listing.get("docs").cloned())
            .map(|docs| {
                docs.as_array()
                    .map(|docs| {
                        docs.iter()
                            .filter_map(|doc| doc.get("slug").cloned())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };
    json!({
        "server": note.server,
        "path": note.path,
        "url": note.url,
        "role": note.role,
        "documents": documents,
        "annotating": note.role == "annotate",
        "agents": note.mcp,
    })
}

/// The server a call is about: named, or the only one there is.
fn server_for(args: &Value) -> Result<ServerNote, String> {
    let running = registry::running_servers();
    let asked = args.get("server").and_then(Value::as_str).unwrap_or("");
    if asked.is_empty() {
        return match running.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err("no document server is running; open_document starts one".into()),
            many => {
                let names: Vec<_> = many.iter().map(|note| note.server.as_str()).collect();
                Err(format!("say which server: {}", names.join(", ")))
            }
        };
    }
    // By name, or by path: an agent is usually told a file, not a name.
    running
        .iter()
        .find(|note| note.server == asked)
        .or_else(|| running.iter().find(|note| note.path == asked))
        .or_else(|| {
            running
                .iter()
                .find(|note| note.server.contains(asked) || note.path.contains(asked))
        })
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = running.iter().map(|note| note.server.as_str()).collect();
            format!("no server called {asked}; there is {}", names.join(", "))
        })
}

/// Starts a server for a path, and waits for it to answer.
fn start_server(path: &Path, show: bool) -> Result<ServerNote, String> {
    let binary = talimist_binary().map_err(|err| err.to_string())?;
    let mut command = std::process::Command::new(binary);
    command
        .arg("serve")
        .arg("--anno")
        .arg("--mcp")
        // Nothing here is anybody's parent: the agent that asked for this will
        // finish its session long before the reader is done with the document.
        .arg("--daemon")
        .arg(path);
    if show {
        command.arg("--open");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("cannot start a server: {err}"))?;

    let began = std::time::Instant::now();
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    while began.elapsed() < std::time::Duration::from_secs(30) {
        if let Some(note) = registry::running_servers()
            .into_iter()
            .find(|note| Path::new(&note.path) == canonical)
        {
            return Ok(note);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("the server for {} did not come up", path.display()))
}

/// Answers one JSON-RPC request, forwarding what it does not own.
pub fn handle(request: Value) -> Option<Value> {
    let id = request.get("id").cloned()?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    let result = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "talimist",
                "title": "Talimist documents",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Typst documents being read and annotated by a person. Each server \
                             serves one file or one directory; call list_servers first, then \
                             pass that name as `server` to everything else. Annotations are \
                             questions and requests left in the margin: claim one, read the \
                             block it points at, rewrite that block, then say what you did and \
                             resolve it. wait_for_annotations blocks until there is something \
                             to do.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            // The hub's own, and every server's — which are the same list
            // whichever server answers, so the first one to answer is asked.
            let mut tools = hub_tools().as_array().cloned().unwrap_or_default();
            let forwarded = registry::running_servers()
                .iter()
                .find(|note| note.mcp)
                .and_then(|note| {
                    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
                    registry::post_json(note.port, "/m/", &body.to_string())
                })
                .and_then(|body| serde_json::from_str::<Value>(&body).ok())
                .and_then(|answer| answer.pointer("/result/tools").cloned())
                .or_else(|| Some(super::tools::tool_list().get("tools")?.clone()))
                .unwrap_or_else(|| json!([]));
            for tool in forwarded.as_array().cloned().unwrap_or_default() {
                // Every forwarded tool gains the server it is about.
                let mut tool = tool;
                if let Some(properties) = tool.pointer_mut("/inputSchema/properties") {
                    if let Some(map) = properties.as_object_mut() {
                        map.insert(
                            "server".into(),
                            json!({
                                "type": "string",
                                "description": "Which document server, from list_servers. May be left out when only one is running.",
                            }),
                        );
                    }
                }
                tools.push(tool);
            }
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            Ok(call(&name, &args))
        }
        other => Err(format!("no such method: {other}")),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": message },
        }),
    })
}

/// A tool result, in the shape a client renders.
fn result(value: Value, failed: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
        "structuredContent": value,
        "isError": failed,
    })
}

/// Runs one tool call: the hub's own, or somebody else's.
fn call(name: &str, args: &Value) -> Value {
    match name {
        "list_servers" => {
            let servers: Vec<Value> = registry::running_servers().iter().map(server_json).collect();
            result(json!({ "servers": servers }), false)
        }
        "open_document" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return result(json!({ "error": "which path? pass path" }), true);
            };
            let path = PathBuf::from(shellexpand(path));
            if !path.exists() {
                return result(json!({ "error": format!("{} does not exist", path.display()) }), true);
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
            // One that is already serving it is the one to use: a second server
            // for the same document would answer on the same port anyway.
            if let Some(note) = registry::running_servers()
                .into_iter()
                .find(|note| Path::new(&note.path) == canonical)
            {
                return result(
                    json!({ "server": note.server, "url": note.url, "started": false }),
                    false,
                );
            }
            let show = args.get("show").and_then(Value::as_bool).unwrap_or(false);
            match start_server(&path, show) {
                Ok(note) => result(
                    json!({ "server": note.server, "url": note.url, "started": true }),
                    false,
                ),
                Err(err) => result(json!({ "error": err }), true),
            }
        }
        "close_document" => match server_for(args) {
            Ok(note) => {
                // Asked to stop, not killed: it has a site to clear up and a
                // note to withdraw.
                let stopped = registry::get(note.port, "/dev/stop").is_some();
                result(json!({ "server": note.server, "stopped": stopped }), !stopped)
            }
            Err(err) => result(json!({ "error": err }), true),
        },
        // Everything else belongs to a document server.
        other => match server_for(args) {
            Ok(note) if !note.mcp => result(
                json!({
                    "error": format!(
                        "{} is serving {} but was not started with --mcp",
                        note.server, note.path
                    )
                }),
                true,
            ),
            Ok(note) => {
                let body = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": { "name": other, "arguments": args },
                });
                match registry::post_json(note.port, "/m/", &body.to_string()) {
                    Some(answer) => serde_json::from_str::<Value>(&answer)
                        .ok()
                        .and_then(|answer| answer.get("result").cloned())
                        .unwrap_or_else(|| {
                            result(json!({ "error": "the server answered with nonsense" }), true)
                        }),
                    None => result(
                        json!({ "error": format!("{} did not answer", note.server) }),
                        true,
                    ),
                }
            }
            Err(err) => result(json!({ "error": err }), true),
        },
    }
}

/// `~` in a path an agent typed, which it will type.
fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest).display().to_string())
            .unwrap_or_else(|| path.to_owned()),
        None => path.to_owned(),
    }
}

/// Answers on stdin and stdout, one JSON message per line, which is how a
/// client that starts its own tools speaks to them.
///
/// Nothing is held here — the register says which servers exist and the
/// servers hold the documents — so a copy of this per session costs nothing
/// and dies with the session that spawned it. That is also what makes it the
/// better way in: a client spawns a stdio server itself, where an address has
/// to have been listening already.
pub fn serve_stdio() -> std::io::Result<()> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                // Nothing to answer to: a message that is not JSON has no id.
                log::warn!("unparseable message: {err}");
                continue;
            }
        };
        let answer = match request {
            Value::Array(calls) => {
                let answers: Vec<Value> = calls.into_iter().filter_map(handle).collect();
                (!answers.is_empty()).then(|| Value::Array(answers))
            }
            one => handle(one),
        };
        // A notification is answered with silence.
        if let Some(answer) = answer {
            writeln!(stdout, "{answer}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ serving
//
// A server of its own, rather than one of the document servers wearing another
// hat: it has to be there when no document is being served at all, which is
// exactly when an agent needs it most.

/// Serves the hub until the process ends.
pub async fn serve(port: u16) -> std::io::Result<()> {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper::service::service_fn;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    log::info!("talimist mcp listening on http://127.0.0.1:{port}/m/");
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| async move {
                let path = req.uri().path().to_owned();
                let is_post = req.method() == hyper::Method::POST;
                let body = req.into_body().collect().await.map(|b| b.to_bytes());
                let answer = |status: hyper::StatusCode, mime: &str, body: String| {
                    hyper::Response::builder()
                        .status(status)
                        .header(hyper::header::CONTENT_TYPE, mime)
                        .body(Full::<Bytes>::from(body))
                        .unwrap()
                };
                let res = match (is_post, path.as_str()) {
                    // What a document server answers with, so that whoever is
                    // probing for one of ours finds this too.
                    (false, "/dev/build") => answer(
                        hyper::StatusCode::OK,
                        "text/plain",
                        crate::tool::webapp::build_stamp(),
                    ),
                    (true, "/m") | (true, "/m/") => {
                        let request: Value = body
                            .ok()
                            .and_then(|body| serde_json::from_slice(&body).ok())
                            .unwrap_or_default();
                        // Answering the calls in a batch one after another is
                        // enough: a client sends one at a time, and the ones
                        // that wait are meant to.
                        let out = match request {
                            Value::Array(calls) => {
                                let answers: Vec<Value> =
                                    calls.into_iter().filter_map(handle).collect();
                                (!answers.is_empty()).then(|| Value::Array(answers))
                            }
                            one => handle(one),
                        };
                        match out {
                            Some(out) => {
                                answer(hyper::StatusCode::OK, "application/json", out.to_string())
                            }
                            None => answer(hyper::StatusCode::ACCEPTED, "text/plain", String::new()),
                        }
                    }
                    (false, "/m") | (false, "/m/") => answer(
                        hyper::StatusCode::METHOD_NOT_ALLOWED,
                        "text/plain",
                        "post JSON-RPC here\n".into(),
                    ),
                    _ => answer(
                        hyper::StatusCode::NOT_FOUND,
                        "text/plain",
                        "talimist mcp: the tools are at /m/\n".into(),
                    ),
                };
                Ok::<_, std::convert::Infallible>(res)
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

/// The binary to start a hub with: this one. Every mode is a subcommand of it,
/// so whatever is running can start the hub.
fn talimist_binary() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

/// Makes sure a hub is running, and says whether this call started it.
///
/// Idempotent on purpose: it is meant to be run at the start of every agent
/// session, and finding one already there is the ordinary case.
pub fn ensure_running(port: u16) -> std::io::Result<bool> {
    if registry::answers(port) {
        return Ok(false);
    }
    let mut child = std::process::Command::new(talimist_binary()?)
        .arg("mcp")
        .arg("--serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let began = std::time::Instant::now();
    while began.elapsed() < std::time::Duration::from_secs(10) {
        if registry::answers(port) {
            return Ok(true);
        }
        // A child that has already exited is not coming up, and waiting out the
        // timeout for it is ten seconds of nothing: whoever is starting a
        // server is usually waiting for a window to open.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(std::io::Error::other(format!(
                "the agent endpoint exited immediately ({status})"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(std::io::Error::other("the agent endpoint did not come up"))
}
