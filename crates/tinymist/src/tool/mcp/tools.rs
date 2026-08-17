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

use crate::tool::serve::annotations::{AnchorPolicy, AnnotationServer};

use crate::tool::serve::{DocServices, DocumentSite};

/// How every tool describes the annotation it works on. An annotation has an
/// id, which is what tools pass around, and a letter, which is what a reader
/// sees on the page and therefore what a person quotes at an agent.
const ID_DESC: &str = "The annotation, by id or by letter: \"82989169e6fbef9b\" or \"g\". \
                       The letter is what the reader sees on the page and what a person will \
                       quote; the id is what these tools return.";

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
///
/// Descriptions are read by a model, so they say what the tool does, what it
/// returns and when to use it, in plain sentences. Each carries an example
/// call, and a second one where a tool is called in two substantially
/// different ways. The examples assume the usual case — one file, served with
/// `talimist serve --anno FILE.typ --mcp` — so none of them passes `document`.
fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_documents",
            title: "List documents",
            description: "Lists the documents this server holds: name, title, path, and the \
                          number of annotations by status. A server of one document lists one. \
                          Example: {}",
            schema: || schema(json!({}), &[]),
        },
        Tool {
            name: "list_annotations",
            title: "List annotations",
            description: "Lists a document's annotations with a one-line excerpt of what each \
                          points at, which is enough to choose one without reading the document. \
                          Each entry has an id, a letter (what the reader sees on the page), a \
                          status, and a location saying what it is about: a word, a paragraph, a \
                          drawing, a position between two things, or the document itself. Use it \
                          to find work. Examples: {\"status\": \"open\"} for what nobody has \
                          taken; {} for everything.",
            schema: || {
                schema(
                    json!({
                        "document": optional_string("Which document, when the server holds several. Its name in the URL."),
                        "status": optional_string("Only these: open (nobody has it), claimed (somebody is on it), resolved (done)."),
                        "author": optional_string("Only annotations by this author."),
                    }),
                    &[],
                )
            },
        },
        Tool {
            name: "wait_for_annotations",
            title: "Wait for annotations",
            description: "Blocks until something happens, then reports it: an annotation added, \
                          replied to, claimed, resolved, reopened or deleted; a file changed; a \
                          document compiled. Returns as soon as there is anything, or an empty \
                          list when the wait runs out. Pass the cursor from the previous call to \
                          get everything since, including what happened while you were working. \
                          Examples: {} on the first call, which starts from now; {\"cursor\": \
                          41, \"timeout\": 120} on every call after that, using the cursor the \
                          previous call returned.",
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
            description: "Returns one annotation in full: what it says, who wrote it, its \
                          status, its discussion, and its location. A location names anchors, \
                          which are labels written into the document; several annotations can \
                          share one anchor, because a Typst element carries at most one label. A \
                          location of type \"document\" means the annotation is about the whole \
                          document, either because it was written that way or because the place \
                          it pointed at has been edited away. Example: {\"uuid\": \"g\"}",
            schema: || schema(json!({ "uuid": string(ID_DESC) }), &["uuid"]),
        },
        Tool {
            name: "claim",
            title: "Claim an annotation",
            description: "Marks an annotation as being worked on, so that the reader and other \
                          agents can see it is taken. Do this before rewriting anything. \
                          Example: {\"uuid\": \"g\"}",
            schema: || schema(json!({ "uuid": string(ID_DESC) }), &["uuid"]),
        },
        Tool {
            name: "release",
            title: "Release an annotation",
            description: "Puts a claimed annotation back to open, for work that was abandoned. \
                          Better than leaving it claimed. Example: {\"uuid\": \"g\"}",
            schema: || schema(json!({ "uuid": string(ID_DESC) }), &["uuid"]),
        },
        Tool {
            name: "get_block",
            title: "Get the block an annotation is in",
            description: "Returns the piece of Typst source an annotation points into: the \
                          smallest thing that can be rewritten whole, such as a paragraph, a \
                          list item, a heading or a figure. The reply has the block's id (needed \
                          to rewrite it), the headings above it, and every anchor inside it with \
                          its offset, which a rewrite must preserve. Works even when the \
                          document does not compile. An annotation about the document, or one \
                          whose anchor is gone, has no block; the reply says so and gives the \
                          text it was written against. Examples: {\"uuid\": \"g\"}; {\"uuid\": \
                          \"g\", \"context\": true} to see the blocks either side as well.",
            schema: || {
                schema(
                    json!({
                        "uuid": string(ID_DESC),
                        "context": {"type": "boolean", "description": "Also return the blocks either side."},
                    }),
                    &["uuid"],
                )
            },
        },
        Tool {
            name: "replace_block",
            title: "Replace a block",
            description: "Rewrites the block an annotation is in, waits for the document to \
                          compile, and reports whether it did. Refused if the block has changed \
                          since you read it (read it again), if the new text drops an anchor \
                          without saying so, or if it invents one. The reply includes the block \
                          as it now stands, with a fresh id. Examples: {\"uuid\": \"g\", \
                          \"blockId\": \"3f9a2c…\", \"text\": \"The rewritten paragraph<anno.A100>\"} \
                          keeps the anchor where it was; add {\"anchors\": \"reattach\"} when the \
                          rewrite has no room for it and the annotation should stay on the \
                          block.",
            schema: || {
                schema(
                    json!({
                        "uuid": string(ID_DESC),
                        "blockId": string("The id from get_block, which says which text you are replacing."),
                        "text": string("The new Typst source for the whole block."),
                        "anchors": optional_string(
                            "What to do with anchors the new text drops: keep (default, refuses), \
                             reattach (puts them back at the start of the block), drop (accepts \
                             it; the annotations that pointed at them become annotations about \
                             the document, and are not deleted).",
                        ),
                    }),
                    &["uuid", "blockId", "text"],
                )
            },
        },
        Tool {
            name: "get_capture",
            title: "See what an annotation points at",
            description: "Returns an image of what a graphical annotation is about — a plot, a \
                          diagram, a framed drawing — together with anything the reader drew on \
                          top of it. The document is source, so this is the only way to see what \
                          the reader saw. Defaults to the most recent capture; earlier ones show \
                          the same drawing before it changed. Examples: {\"uuid\": \"q\"}; \
                          {\"uuid\": \"q\", \"markup\": false} for the drawing without what the \
                          reader drew on it.",
            schema: || {
                schema(
                    json!({
                        "uuid": string("The annotation, by id or letter. Its captures are listed on the annotation."),
                        "index": {"type": "integer", "description": "Which capture: 0 is the most recent, 1 the one before it. Default 0."},
                        "hash": optional_string("A capture's hash, when you want that exact one."),
                        "markup": {"type": "boolean", "description": "Include what the reader drew on top. Default true; pass false to see the drawing bare."},
                        "scale": {"type": "number", "description": "Size, as a multiple of the drawing's own. Default is half size, or less when that would still be over 1000px on the long edge."},
                        "document": optional_string("Which document, when the server holds several."),
                    }),
                    &["uuid"],
                )
            },
        },
        Tool {
            name: "annotate",
            title: "Annotate the document",
            description: "Adds an annotation about the document as a whole — something noticed \
                          while reading that is not about one place. It appears in the corner of \
                          the reader's page. Annotations about a particular word or block are \
                          made by the reader, who points at one; this tool cannot point. \
                          Example: {\"text\": \"Section 3 and section 5 give different totals.\", \
                          \"kind\": \"question\"}",
            schema: || {
                schema(
                    json!({
                        "text": string("What to say."),
                        "kind": optional_string("What sort of remark: comment (default), question, request."),
                        "author": optional_string("Who is saying it. Say who you are; the default is whoever is running the server."),
                        "document": optional_string("Which document, when the server holds several."),
                    }),
                    &["text"],
                )
            },
        },
        Tool {
            name: "audit",
            title: "Audit the annotations",
            description: "Reports what the document and its sidecar say about each other: how \
                          many annotations and anchors there are, anchors nothing points at any \
                          more, and annotations whose anchor is gone. The last of these are the \
                          ones the reader cannot see in place; they are about the document until \
                          an anchor comes back. Example: {}",
            schema: || {
                schema(
                    json!({ "document": optional_string("Which document, when the server holds several.") }),
                    &[],
                )
            },
        },
        Tool {
            name: "delete",
            title: "Delete an annotation",
            description: "Deletes an annotation. Prefer resolve, which keeps what was said and \
                          what was done about it; use this for one that should not have been \
                          made. Example: {\"uuid\": \"g\"}",
            schema: || schema(json!({ "uuid": string(ID_DESC) }), &["uuid"]),
        },
        Tool {
            name: "reply",
            title: "Reply to an annotation",
            description: "Adds a message to an annotation's discussion, where the reader sees \
                          it. Example: {\"uuid\": \"g\", \"text\": \"Rewritten; the totals now \
                          agree.\", \"author\": \"claude\"}",
            schema: || {
                schema(
                    json!({
                        "uuid": string(ID_DESC),
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
            description: "Says what was done and marks the annotation resolved, which ends the \
                          thread and releases the claim. Example: {\"uuid\": \"g\", \"text\": \
                          \"Fixed in the summary table.\", \"author\": \"claude\"}",
            schema: || {
                schema(
                    json!({
                        "uuid": string(ID_DESC),
                        "text": optional_string("What was done. Added to the discussion first."),
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
    let annot = doc.annot.clone();
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
fn record_json(rec: &crate::tool::serve::annotations::AnnotationRecord, excerpt: Option<String>) -> Value {
    let mut value = serde_json::to_value(rec).unwrap_or_default();
    if let (Some(map), Some(excerpt)) = (value.as_object_mut(), excerpt) {
        map.insert("excerpt".into(), excerpt.into());
    }
    value
}

/// The annotation a caller named, by id or by letter.
///
/// A person reads letters off the page and says "look at g"; a tool passes ids
/// around. Both arrive as the same argument, and an id is tried first since it
/// cannot be mistaken for anything else.
fn identify(annot: &Arc<dyn AnnotationServer>, given: &str) -> Result<String, String> {
    let records = annot.records()?;
    if records.iter().any(|rec| rec.uuid == given) {
        return Ok(given.to_owned());
    }
    let wanted = given.trim().to_ascii_lowercase();
    let found: Vec<&crate::tool::serve::AnnotationRecord> = records
        .iter()
        .filter(|rec| rec.letter.to_ascii_lowercase() == wanted)
        .collect();
    match found.as_slice() {
        [one] => Ok(one.uuid.clone()),
        [] => Err(format!("no annotation {given}")),
        several => Err(format!(
            "{given} names {} annotations; pass one of {}",
            several.len(),
            several
                .iter()
                .map(|rec| rec.uuid.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A line of what an annotation points at, for deciding whether to open it.
fn excerpt_of(annot: &Arc<dyn AnnotationServer>, uuid: &str) -> Option<String> {
    // What it points at — or, for one about the document and one whose place
    // has been edited away, what it was about when it was written.
    let text = match annot.block(uuid, false) {
        Ok(block) => block.text,
        Err(_) => annot
            .records()
            .ok()?
            .into_iter()
            .find(|rec| rec.uuid == uuid)?
            .snapshot?,
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
                .filter(|rec| {
                    status.as_ref().is_none_or(|want| match want.as_str() {
                        "resolved" => rec.resolved,
                        "claimed" => rec.claimed && !rec.resolved,
                        "open" => !rec.claimed && !rec.resolved,
                        _ => true,
                    })
                })
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
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            let record = annot
                .records()?
                .into_iter()
                .find(|rec| rec.uuid == uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            Ok(record_json(&record, excerpt_of(&annot, &uuid)))
        }
        "claim" | "release" => {
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            let claimed = name == "claim";
            annot.set_flags(&uuid, Some(claimed), None)?;
            Ok(json!({ "uuid": uuid, "claimed": claimed }))
        }
        "delete" => {
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            annot.remove(&uuid)?;
            Ok(json!({ "ok": true, "uuid": uuid }))
        }
        "annotate" => {
            let said = text("text").ok_or("what should it say? pass text")?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = annot.annotate(crate::tool::serve::AnnotateRequest {
                location: tinymist_annos::HtmlLocation::Document,
                render: String::new(),
                text: said,
                kind: text("kind"),
                color: None,
                snapshot: None,
                author: text("author"),
            })?;
            let letter = annot
                .records()?
                .into_iter()
                .find(|rec| rec.uuid == uuid)
                .map(|rec| rec.letter)
                .unwrap_or_default();
            Ok(json!({ "ok": true, "uuid": uuid, "letter": letter }))
        }
        "audit" => {
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            annot.audit()
        }
        "get_block" => {
            let context = args
                .get("context")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            let record = annot
                .records()?
                .into_iter()
                .find(|rec| rec.uuid == uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            // Two annotations have no block, and neither is a failure: one
            // about the document was never about a place, and one whose anchor
            // has been edited away is about the document now. An agent asked
            // what this is about, and there is an answer.
            match annot.block(&uuid, context) {
                Ok(block) => serde_json::to_value(block).map_err(|err| err.to_string()),
                Err(_) if record.location.kind() == "document" => Ok(json!({
                    "block": null,
                    "about": "the document",
                    "note": "About the document as a whole; there is no block to read or rewrite.",
                    "snapshot": record.snapshot,
                })),
                Err(err) => Ok(json!({
                    "block": null,
                    "about": "a place that is gone",
                    "note": format!(
                        "The place this pointed at is gone: {err}. It is about the document \
                         until an anchor comes back."
                    ),
                    "snapshot": record.snapshot,
                    "location": record.location,
                })),
            }
        }
        "replace_block" => {
            let uuid = uuid()?;
            let block_id = text("blockId").ok_or("which block? pass blockId")?;
            let new_text = text("text").ok_or("what should it say? pass text")?;
            let policy = AnchorPolicy::parse(text("anchors").unwrap_or_default().as_str())?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid)?;
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
        "get_capture" => {
            use crate::tool::serve::capture;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            let record = annot
                .records()?
                .into_iter()
                .find(|rec| rec.uuid == uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            if record.captures.is_empty() {
                return Err(format!(
                    "{uuid} has no captures: it does not point at a drawing, or the document has \
                     not compiled since it was made"
                ));
            }
            // Newest first, which is what "the picture" means unless an older
            // one is asked for by name.
            let newest = record.captures.len() - 1;
            let wanted = match text("hash") {
                Some(hash) => record
                    .captures
                    .iter()
                    .position(|capture| capture.hash == hash)
                    .ok_or_else(|| format!("{uuid} has no capture {hash}"))?,
                None => {
                    let back = args.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    newest
                        .checked_sub(back)
                        .ok_or_else(|| format!("{uuid} has only {} captures", record.captures.len()))?
                }
            };
            let chosen = &record.captures[wanted];
            let source = capture::read(&chosen.hash, &chosen.fmt).ok_or_else(|| {
                format!(
                    "the capture {} is no longer stored; it is remade the next time the drawing \
                     changes",
                    chosen.hash
                )
            })?;
            if chosen.fmt != "svg" {
                return Err(format!(
                    "captures stored as {} cannot be rendered yet",
                    chosen.fmt
                ));
            }
            let svg = String::from_utf8(source).map_err(|_| "the capture is not text".to_owned())?;
            let with_markup = args.get("markup").and_then(Value::as_bool).unwrap_or(true);
            let markup = chosen.markup.as_deref().filter(|_| with_markup);
            let drawn = match markup {
                Some(markup) => capture::with_markup(&svg, markup),
                None => svg,
            };
            let scale = args.get("scale").and_then(Value::as_f64).map(|s| s as f32);
            let png = capture::png(&drawn, scale)?;
            use base64::Engine as _;
            let data = base64::engine::general_purpose::STANDARD.encode(&png);
            Ok(json!({
                "image": { "data": data, "mimeType": "image/png" },
                "uuid": uuid,
                "hash": chosen.hash,
                "time": chosen.time,
                "index": newest - wanted,
                "captures": record.captures.len(),
                "markup": markup.is_some(),
                "width": chosen.width,
                "height": chosen.height,
            }))
        }
        "reply" => {
            let said = text("text").ok_or("what should it say? pass text")?;
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            annot.reply(&uuid, &said, text("author").as_deref())?;
            Ok(json!({ "uuid": uuid, "replied": true }))
        }
        "resolve" => {
            let (_, doc) = document_of(site, args).await?;
            let annot = annot_of(&doc).await?;
            let uuid = identify(&annot, &uuid()?)?;
            if let Some(said) = text("text") {
                annot.reply(&uuid, &said, text("author").as_deref())?;
            }
            // Resolved and let go in one move: an annotation nobody needs to
            // look at again is not one anybody is still holding.
            annot.set_flags(&uuid, Some(false), Some(true))?;
            Ok(json!({ "uuid": uuid, "claimed": false, "resolved": true }))
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
    // A tool that answers with a picture says so by putting it under `image`,
    // which is lifted out here into a content block of its own: an image block
    // is what a client renders and what a model can actually look at, and the
    // base64 has no business being in the text beside it.
    let mut value = value;
    let image = value
        .as_object_mut()
        .and_then(|map| map.remove("image"))
        .filter(|image| image.get("data").is_some());
    let text = serde_json::to_string_pretty(&value).unwrap_or_default();
    let mut content = vec![];
    if let Some(image) = image {
        content.push(json!({
            "type": "image",
            "data": image.get("data").and_then(Value::as_str).unwrap_or_default(),
            "mimeType": image
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image/png"),
        }));
    }
    content.push(json!({ "type": "text", "text": text }));
    json!({
        "content": content,
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
