//! The machine face of a document server: MCP over HTTP, at `/m/`.
//!
//! An agent works the same documents a reader does, and needs different things
//! from them: to be told when an annotation arrives, to claim it, to read the
//! piece of source it points at, to rewrite that piece and be told whether the
//! document still compiles, and to answer in the thread. That is this.
//!
//! It is the same server and the same port — nothing to introduce to anything —
//! and the pages are untouched: `/a/` is the annotator, `/m/` is the agent.
//!
//! The protocol is JSON-RPC over POST, which is all of MCP's HTTP transport a
//! tool server needs: `initialize`, `tools/list`, `tools/call`. Notifications
//! from server to client are deliberately absent — an agent hears about new
//! annotations by calling `wait_for_annotations`, which waits, rather than by
//! subscribing to a stream it has to keep alive while it thinks.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::tool::preview::{AnchorPolicy, AnnotationServer};

use super::{DocServices, DocumentSite};

/// What this server calls itself to an agent.
const SERVER_NAME: &str = "talimist";

/// The protocol version this speaks. The client asks for one; this is what is
/// answered, and it is the version these shapes were written against.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// One tool, as `tools/list` describes it.
struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
}

fn string(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn optional_string(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Every tool an agent can call, in the order it will want them.
fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_documents",
            title: "List documents",
            description: "The documents this server holds: name, title, path, and how many \
                          annotations each has, by status. A server of one document lists one.",
            schema: || schema(json!({}), &[]),
        },
        Tool {
            name: "list_annotations",
            title: "List annotations",
            description: "The annotations on a document, with a one-line excerpt of what each \
                          points at — enough to decide which to work on without reading the \
                          document. Filter by status to find work: \"created\" is unclaimed.",
            schema: || {
                schema(
                    json!({
                        "document": optional_string("Which document, when the server holds several. Its name in the URL."),
                        "status": optional_string("Only these: created | ongoing | resolved."),
                        "author": optional_string("Only annotations by this author."),
                    }),
                    &[],
                )
            },
        },
        Tool {
            name: "wait_for_annotations",
            title: "Wait for annotations",
            description: "Waits for something to happen and reports it: annotations added, \
                          replied to, resolved or deleted, files changed, documents compiled. \
                          Pass the cursor from the last call to hear everything since — \
                          including what arrived while you were thinking. Returns as soon as \
                          there is anything, or empty when the wait runs out.",
            schema: || {
                schema(
                    json!({
                        "cursor": {"type": "integer", "description": "The cursor from the previous call. Omit on the first call to start from now."},
                        "timeout": {"type": "integer", "description": "Seconds to wait. Default 60, maximum 300."},
                    }),
                    &[],
                )
            },
        },
        Tool {
            name: "get_annotation",
            title: "Get an annotation",
            description: "One annotation in full: what was said, by whom, its status, its \
                          discussion, and what it is anchored to.",
            schema: || schema(json!({ "uuid": string("The annotation's id, e.g. 7C42.") }), &["uuid"]),
        },
        Tool {
            name: "claim",
            title: "Claim an annotation",
            description: "Marks an annotation as being worked on, so that a human reading the \
                          document — and any other agent — can see that somebody has it. Do \
                          this before rewriting anything.",
            schema: || schema(json!({ "uuid": string("The annotation's id.") }), &["uuid"]),
        },
        Tool {
            name: "release",
            title: "Release an annotation",
            description: "Puts a claimed annotation back to \"created\", for when the work is \
                          abandoned. Better than leaving it claimed forever.",
            schema: || schema(json!({ "uuid": string("The annotation's id.") }), &["uuid"]),
        },
        Tool {
            name: "get_block",
            title: "Get the block an annotation is in",
            description: "The piece of Typst source the annotation points into: the smallest \
                          thing that can be rewritten whole — a paragraph, a list item, a \
                          heading, a figure. Comes with its id (for rewriting it), the \
                          headings above it, and every anchor inside it with its offset, which \
                          a rewrite must carry across. Works even when the document does not \
                          compile.",
            schema: || {
                schema(
                    json!({
                        "uuid": string("The annotation's id."),
                        "context": {"type": "boolean", "description": "Also return the blocks either side."},
                    }),
                    &["uuid"],
                )
            },
        },
        Tool {
            name: "replace_block",
            title: "Replace a block",
            description: "Rewrites the block an annotation is in, then waits for the document \
                          to compile and reports whether it did. Refused if the block has \
                          changed since you read it (read it again), if the new text drops an \
                          anchor (keep them, or say so), or if it invents one. The reply says \
                          what the block is now.",
            schema: || {
                schema(
                    json!({
                        "uuid": string("The annotation whose block this is."),
                        "blockId": string("The id from get_block, which says which text you are replacing."),
                        "text": string("The new Typst source for the whole block."),
                        "anchors": optional_string(
                            "What to do with anchors the new text drops: keep (default, refuses), \
                             reattach (puts them at the start), drop (deletes those annotations).",
                        ),
                    }),
                    &["uuid", "blockId", "text"],
                )
            },
        },
        Tool {
            name: "reply",
            title: "Reply to an annotation",
            description: "Adds a message to an annotation's discussion, which is where the \
                          author reads it.",
            schema: || {
                schema(
                    json!({
                        "uuid": string("The annotation's id."),
                        "text": string("What to say."),
                        "author": optional_string("Who is saying it. Say who you are; the default is whoever is running the server."),
                    }),
                    &["uuid", "text"],
                )
            },
        },
        Tool {
            name: "resolve",
            title: "Resolve an annotation",
            description: "Says what was done and marks the annotation resolved, which is how a \
                          thread ends.",
            schema: || {
                schema(
                    json!({
                        "uuid": string("The annotation's id."),
                        "text": optional_string("What was done. Said in the discussion first."),
                        "author": optional_string("Who did it. Say who you are; the default is whoever is running the server."),
                    }),
                    &["uuid"],
                )
            },
        },
    ]
}

/// The tool list as `tools/list` answers it.
pub fn tool_list() -> Value {
    let tools: Vec<Value> = tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// The services of the document a call is about, and its name.
async fn document_of(
    site: &Arc<dyn DocumentSite>,
    args: &Value,
) -> Result<(String, Arc<DocServices>), String> {
    let asked = args.get("document").and_then(Value::as_str).unwrap_or("");
    if !site.is_listing() {
        return site
            .services("")
            .await
            .map(|doc| (String::new(), doc))
            .ok_or_else(|| "the document is not available".to_owned());
    }
    let entries = site.listing();
    let slug = if asked.is_empty() {
        match entries.as_slice() {
            [only] => only.slug.clone(),
            [] => return Err("this server holds no documents".into()),
            _ => {
                let names: Vec<_> = entries.iter().map(|e| e.slug.as_str()).collect();
                return Err(format!(
                    "say which document: {}",
                    names.join(", ")
                ));
            }
        }
    } else {
        asked.to_owned()
    };
    site.services(&slug)
        .await
        .map(|doc| (slug.clone(), doc))
        .ok_or_else(|| format!("no document named {slug}"))
}

/// The annotation service of a document, which is what most tools need, once
/// the document has compiled at least once.
///
/// A document is compiled when somebody first asks for it, and an agent asking
/// is somebody: it waits here rather than being told to come back, which is
/// what a first call would otherwise mean.
async fn annot_of(doc: &Arc<DocServices>) -> Result<Arc<dyn AnnotationServer>, String> {
    let annot = doc
        .annot
        .clone()
        .ok_or_else(|| "this document is served without annotations".to_owned())?;
    let began = std::time::Instant::now();
    while annot.compile_revision() == 0 {
        if began.elapsed() > std::time::Duration::from_secs(30) {
            return Err("the document has not compiled".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(annot)
}

/// Waits for the document to compile again, and says how it went.
///
/// An agent that has just rewritten a block wants to know in the same breath
/// whether the document still stands; finding out three edits later is finding
/// out too late.
async fn compile_report(annot: &Arc<dyn AnnotationServer>, was: u64) -> Value {
    let deadline = std::time::Duration::from_secs(10);
    let began = std::time::Instant::now();
    while began.elapsed() < deadline {
        if annot.compile_revision() != was {
            let (ok, messages) = annot.diagnostics();
            return json!({ "compiled": true, "ok": ok, "diagnostics": messages });
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    json!({ "compiled": false, "ok": true, "diagnostics": [] })
}

/// One annotation, as the tools describe it.
fn record_json(rec: &crate::tool::preview::AnnotationRecord, excerpt: Option<String>) -> Value {
    let mut value = serde_json::to_value(rec).unwrap_or_default();
    if let (Some(map), Some(excerpt)) = (value.as_object_mut(), excerpt) {
        map.insert("excerpt".into(), excerpt.into());
    }
    value
}

/// A line of what an annotation points at, for deciding whether to open it.
fn excerpt_of(annot: &Arc<dyn AnnotationServer>, uuid: &str) -> Option<String> {
    let block = annot.block(uuid, false).ok()?;
    let text = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if text.chars().count() > 160 {
        format!("{}…", text.chars().take(160).collect::<String>())
    } else {
        text
    })
}

/// Runs one tool call.
async fn call_tool(site: &Arc<dyn DocumentSite>, name: &str, args: &Value) -> Result<Value, String> {
    let text = |value: &str| -> Option<String> {
        args.get(value)
            .and_then(Value::as_str)
            .map(|it| it.to_owned())
    };
    let uuid = || text("uuid").ok_or_else(|| "which annotation? pass uuid".to_owned());

    match name {
        "list_documents" => {
            let docs: Vec<Value> = if site.is_listing() {
                site.listing()
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "document": entry.slug,
                            "title": entry.title,
                            "file": entry.file,
                            "annotations": entry.annotations,
                        })
                    })
                    .collect()
            } else {
                let (_, doc) = document_of(site, args).await?;
                vec![json!({ "document": "", "title": doc.title })]
            };
            Ok(json!({ "documents": docs }))
        }
        "list_annotations" => {
            let (document, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let status = text("status");
            let author = text("author");
            let records = annot.records()?;
            let annotations: Vec<Value> = records
                .iter()
                .filter(|rec| status.as_ref().is_none_or(|want| &rec.status == want))
                .filter(|rec| author.as_ref().is_none_or(|want| &rec.author == want))
                .map(|rec| record_json(rec, excerpt_of(&annot, &rec.uuid)))
                .collect();
            Ok(json!({ "document": document, "annotations": annotations }))
        }
        "wait_for_annotations" => {
            let cursor = args.get("cursor").and_then(Value::as_u64);
            let timeout = args
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(60)
                .min(300);
            // No cursor means "from now": an agent starting up wants what
            // happens next, not the history of the session it missed.
            let (mut events, mut next) = match cursor {
                Some(cursor) => tinymist_project::events_since(cursor),
                None => (vec![], tinymist_project::events_since(u64::MAX).1),
            };
            if events.is_empty() {
                let mut signal = tinymist_project::event_signal();
                let since = cursor.unwrap_or(next);
                let waited = tokio::time::timeout(
                    std::time::Duration::from_secs(timeout),
                    signal.changed(),
                )
                .await;
                if waited.is_ok() {
                    // One thing happening is usually three: a file changes, a
                    // document compiles, an annotation appears. Settling for a
                    // moment turns that into one answer instead of three calls.
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let fresh = tinymist_project::events_since(since);
                    events = fresh.0;
                    next = fresh.1;
                }
            }
            Ok(json!({ "events": events, "cursor": next }))
        }
        "get_annotation" => {
            let uuid = uuid()?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let record = annot
                .records()?
                .into_iter()
                .find(|rec| rec.uuid == uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            Ok(record_json(&record, excerpt_of(&annot, &uuid)))
        }
        "claim" | "release" => {
            let uuid = uuid()?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let status = if name == "claim" { "ongoing" } else { "created" };
            annot.set_status(&uuid, status)?;
            Ok(json!({ "uuid": uuid, "status": status }))
        }
        "get_block" => {
            let uuid = uuid()?;
            let context = args
                .get("context")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let block = annot.block(&uuid, context)?;
            serde_json::to_value(block).map_err(|err| err.to_string())
        }
        "replace_block" => {
            let uuid = uuid()?;
            let block_id = text("blockId").ok_or("which block? pass blockId")?;
            let new_text = text("text").ok_or("what should it say? pass text")?;
            let policy = AnchorPolicy::parse(text("anchors").unwrap_or_default().as_str())?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let was = annot.compile_revision();
            let dropped = annot.replace_block(&uuid, &block_id, &new_text, policy)?;
            let report = compile_report(&annot, was).await;
            // What the block is now, so the next rewrite has an id that is not
            // already stale.
            let block = annot.block(&uuid, false).ok();
            Ok(json!({
                "ok": true,
                "dropped": dropped,
                "compile": report,
                "block": block,
            }))
        }
        "reply" => {
            let uuid = uuid()?;
            let said = text("text").ok_or("what should it say? pass text")?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            annot.reply(&uuid, &said, text("author").as_deref())?;
            Ok(json!({ "uuid": uuid, "replied": true }))
        }
        "resolve" => {
            let uuid = uuid()?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            if let Some(said) = text("text") {
                annot.reply(&uuid, &said, text("author").as_deref())?;
            }
            annot.set_status(&uuid, "resolved")?;
            Ok(json!({ "uuid": uuid, "status": "resolved" }))
        }
        other => Err(format!("no such tool: {other}")),
    }
}

/// A JSON-RPC error, as MCP expects one.
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// A tool result: text content, because that is what every client renders, and
/// the structured value beside it for those that read it.
fn tool_result(value: Value, failed: bool) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": failed,
    })
}

/// Answers one JSON-RPC request. `None` for a notification, which is answered
/// with silence.
pub async fn handle(site: &Arc<dyn DocumentSite>, request: Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    // A notification has no id and expects no answer.
    let Some(id) = id else {
        return None;
    };

    let result = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": SERVER_NAME,
                "title": "Talimist documents",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Annotations are questions and requests left on a document by \
                             whoever is reading it. Work one at a time: claim it, read the \
                             block it points at, rewrite that block, then say what you did and \
                             resolve it. wait_for_annotations blocks until there is something \
                             to do.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(site, &name, &args).await {
                // A tool that failed is not a protocol error: the model is
                // meant to read what went wrong and try something else.
                Ok(value) => Ok(tool_result(value, false)),
                Err(err) => Ok(tool_result(json!({ "error": err }), true)),
            }
        }
        other => Err(format!("no such method: {other}")),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(message) => rpc_error(id, -32601, &message),
    })
}
