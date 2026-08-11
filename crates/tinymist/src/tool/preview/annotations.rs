//! Preview annotations: comments anchored to document text via labels.
//!
//! An annotation is a cursor label like `<-7C42->` inserted into the
//! document source at the clicked word, plus a structured record in a
//! sidecar file `<main>.annos.typ` next to the main file (schema documented
//! in `annos_prelude.typ`, which is stamped at the top of every new
//! sidecar). The rendered position of each annotation is resolved by
//! querying the compiled document for the label, so anchors survive
//! arbitrary edits around them. Records carry an author, ISO 8601 time, a
//! status, and a discussion thread that agents and the preview UI append to.

use std::path::PathBuf;

use lsp_types::Url;
use reflexo::debug_loc::LspPosition;
use reflexo_typst::TypstDocument;
use serde::{Deserialize, Serialize};
use tinymist_project::LspCompiledArtifact;
use tinymist_query::{jump_from_click, to_lsp_position, PositionEncoding};
use typst::foundations::Label;
use typst::introspection::PagedPosition;
use typst::layout::{Abs, Point};
use typst::syntax::SyntaxKind;
use typst::utils::PicoStr;
use typst::World;

/// The schema documentation stamped at the top of new sidecar files.
pub const ANNOS_PRELUDE: &str = include_str!("annos_prelude.typ");

/// A reply in an annotation's discussion thread.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationReply {
    /// The reply author.
    pub author: String,
    /// The reply time, ISO 8601 UTC.
    pub time: String,
    /// The reply text.
    pub content: String,
}

/// A stored annotation record from the sidecar file.
#[derive(Debug, Clone)]
pub struct AnnotationRecord {
    /// The annotation kind: "comment" | "question" | "request".
    pub rtype: String,
    /// The short label, e.g. "7C42"; the document anchor is `<-7C42->`.
    pub label: String,
    /// The author of the annotation.
    pub author: String,
    /// The message.
    pub content: String,
    /// Creation time, ISO 8601 UTC.
    pub time: String,
    /// The status: "created" | "ongoing" | "resolved".
    pub status: String,
    /// The discussion thread, in order.
    pub discussion: Vec<AnnotationReply>,
}

/// An annotation resolved onto the rendered document, sent to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPin {
    /// The annotation kind.
    #[serde(rename = "type")]
    pub rtype: String,
    /// The short label.
    pub label: String,
    /// The author.
    pub author: String,
    /// The message.
    pub content: String,
    /// Creation time, ISO 8601 UTC.
    pub time: String,
    /// The status.
    pub status: String,
    /// The discussion thread.
    pub discussion: Vec<AnnotationReply>,
    /// The 1-based page number.
    pub page: usize,
    /// The x coordinate of the anchor, in pt.
    pub x: f64,
    /// The y coordinate of the anchor, in pt.
    pub y: f64,
    /// The page width, in pt.
    pub page_width: f64,
    /// The page height, in pt.
    pub page_height: f64,
}

/// A request to create an annotation at a clicked position.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotateRequest {
    /// A client-suggested label, e.g. `7C42` (random 16-bit hex stamped by
    /// the browser). Used verbatim when valid and free; otherwise the server
    /// generates one.
    #[serde(default)]
    pub label: Option<String>,
    /// The 1-based page number.
    pub page: usize,
    /// The x coordinate of the click, in pt.
    pub x: f64,
    /// The y coordinate of the click, in pt.
    pub y: f64,
    /// The comment text.
    pub text: String,
}

/// The edits needed to create or delete an annotation. When the target file
/// has no unsaved editor changes (its disk content matches the compiled
/// source), `disk_content` carries the new file content to write directly on
/// disk — so external watchers see the label immediately; otherwise the edit
/// must go through the editor as a workspace edit. The sidecar is always
/// written on disk.
#[derive(Debug, Clone)]
pub struct AnnotationEdit {
    /// The annotation label.
    pub label: String,
    /// The uri of the source file to edit.
    pub uri: Url,
    /// The path of the source file to edit.
    pub path: PathBuf,
    /// The new full content of the source file, when it can be written on
    /// disk directly.
    pub disk_content: Option<String>,
    /// Whether the edit must (also) go through the editor buffer. False only
    /// when the file was clean and the disk write alone suffices (the editor
    /// reloads clean buffers silently).
    pub buffer_edit: bool,
    /// The range to replace in the source file (empty range = insertion).
    pub range: lsp_types::Range,
    /// The replacement text.
    pub new_text: String,
    /// The sidecar file path.
    pub sidecar: PathBuf,
    /// The new full content of the sidecar file.
    pub sidecar_content: String,
}

/// The server-side annotation API exposed to the preview http server.
pub trait AnnotationServer: Send + Sync {
    /// Creates an annotation at a clicked position. Returns the new label.
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String>;
    /// Deletes an annotation by label.
    fn remove(&self, label: &str) -> Result<(), String>;
    /// Appends a reply to an annotation's discussion.
    fn reply(&self, label: &str, text: &str) -> Result<(), String>;
    /// Sets an annotation's status.
    fn set_status(&self, label: &str, status: &str) -> Result<(), String>;
}

/// The document anchor text for a label, e.g. `<-7C42->`.
fn anchor_text(label: &str) -> String {
    format!("<-{label}->")
}

/// The label name queried in the compiled document, e.g. `-7C42-`.
fn anchor_name(label: &str) -> String {
    format!("-{label}-")
}

/// The sidecar path for the current main file, e.g. `typing.annos.typ`
/// next to `typing.typ`.
pub fn sidecar_path(art: &LspCompiledArtifact) -> Option<PathBuf> {
    let world = art.world();
    let main = world.main();
    let path = world.path_for_id(main).ok()?.to_err().ok()?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    Some(path.with_file_name(format!("{stem}.annos.typ")))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Reads a typst string literal starting at `s` (which must begin with a
/// quote), returning the raw escaped content and the rest.
fn read_string(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start().strip_prefix('"')?;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some((&s[..i], &s[i + 1..])),
            _ => i += 1,
        }
    }
    None
}

fn read_field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let at = block.find(&format!("{key}: "))?;
    Some(&block[at + key.len() + 2..])
}

fn read_string_field(block: &str, key: &str) -> Option<String> {
    read_field(block, key)
        .and_then(read_string)
        .map(|(raw, _)| unescape(raw))
}

/// Parses the discussion segment of an entry block: everything after
/// `discussion:`, as a sequence of (author, time, content) groups.
fn parse_discussion(block: &str) -> Vec<AnnotationReply> {
    let Some(mut rest) = read_field(block, "discussion") else {
        return vec![];
    };
    let mut replies = vec![];
    while let Some(at) = rest.find("author: ") {
        let seg = &rest[at..];
        let author = read_string_field(seg, "author");
        let time = read_string_field(seg, "time");
        let content = read_string_field(seg, "content");
        if let (Some(author), Some(time), Some(content)) = (author, time, content) {
            replies.push(AnnotationReply {
                author,
                time,
                content,
            });
        }
        rest = &seg["author: ".len()..];
    }
    replies
}

/// One parsed sidecar entry: its byte span in the file and its record.
struct ParsedEntry {
    span: std::ops::Range<usize>,
    record: AnnotationRecord,
}

/// Parses the sidecar file into entries with their byte spans. Only entries
/// in the format written by [`format_record`] are recognized.
fn parse_entries(content: &str) -> Vec<ParsedEntry> {
    let mut entries = vec![];
    let mut at = 0;
    while let Some(rel) = content[at..].find("#metadata((") {
        let start = at + rel;
        let block_start = start + "#metadata((".len();
        // The entry ends at the label after the closing `))`.
        let Some(close_rel) = content[block_start..].find("))") else {
            break;
        };
        let close = block_start + close_rel;
        let tail = &content[close..];
        let end_rel = tail
            .find(">\n")
            .map(|e| e + 2)
            .or_else(|| tail.find('>').map(|e| e + 1))
            .unwrap_or(0);
        let end = close + end_rel.max(2);
        let block = &content[block_start..close];
        at = end;

        // The discussion field contains nested dicts, so cut the top-level
        // fields off before it to avoid reading reply fields.
        let head = block
            .find("discussion:")
            .map(|d| &block[..d])
            .unwrap_or(block);
        let label = read_string_field(head, "label");
        let content_field = read_string_field(head, "content");
        let Some(label) = label else { continue };
        let Some(content_field) = content_field else {
            continue;
        };
        entries.push(ParsedEntry {
            span: start..end,
            record: AnnotationRecord {
                rtype: read_string_field(head, "type").unwrap_or_else(|| "comment".into()),
                label,
                author: read_string_field(head, "author").unwrap_or_else(|| "unknown".into()),
                content: content_field,
                time: read_string_field(head, "time").unwrap_or_default(),
                status: read_string_field(head, "status").unwrap_or_else(|| "created".into()),
                discussion: parse_discussion(block),
            },
        });
    }
    entries
}

/// Parses the sidecar file content into annotation records.
pub fn parse_records(content: &str) -> Vec<AnnotationRecord> {
    parse_entries(content)
        .into_iter()
        .map(|entry| entry.record)
        .collect()
}

/// Formats one annotation record as a sidecar entry.
pub fn format_record(rec: &AnnotationRecord) -> String {
    let mut out = String::new();
    out.push_str("#metadata((\n");
    out.push_str(&format!("  type: \"{}\",\n", escape(&rec.rtype)));
    out.push_str(&format!("  label: \"{}\",\n", escape(&rec.label)));
    out.push_str(&format!("  author: \"{}\",\n", escape(&rec.author)));
    out.push_str(&format!("  content: \"{}\",\n", escape(&rec.content)));
    out.push_str(&format!("  time: \"{}\",\n", escape(&rec.time)));
    out.push_str(&format!("  status: \"{}\",\n", escape(&rec.status)));
    if rec.discussion.is_empty() {
        out.push_str("  discussion: (),\n");
    } else {
        out.push_str("  discussion: (\n");
        for reply in &rec.discussion {
            out.push_str("    (\n");
            out.push_str(&format!("      author: \"{}\",\n", escape(&reply.author)));
            out.push_str(&format!("      time: \"{}\",\n", escape(&reply.time)));
            out.push_str(&format!("      content: \"{}\",\n", escape(&reply.content)));
            out.push_str("    ),\n");
        }
        out.push_str("  ),\n");
    }
    out.push_str(&format!(")) <note-{}>\n", rec.label));
    out
}

fn read_sidecar(path: &std::path::Path) -> (Vec<AnnotationRecord>, String) {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let records = parse_records(&content);
            (records, content)
        }
        Err(_) => (vec![], ANNOS_PRELUDE.to_owned()),
    }
}

/// Resolves all annotations of the current main file onto the last
/// successful render.
pub fn annotation_pins(art: &LspCompiledArtifact) -> Vec<AnnotationPin> {
    let Some(sidecar) = sidecar_path(art) else {
        return vec![];
    };
    let (records, _) = read_sidecar(&sidecar);
    if records.is_empty() {
        return vec![];
    }
    let Some(doc) = art.success_doc() else {
        return vec![];
    };
    let TypstDocument::Paged(paged) = &doc else {
        return vec![];
    };
    let introspector = doc.introspector();
    records
        .iter()
        .filter_map(|rec| {
            let label = Label::new(PicoStr::intern(&anchor_name(&rec.label)))?;
            let elem = introspector.query_label(label).ok()?;
            let loc = elem.location()?;
            let pos: PagedPosition = introspector.position(loc)?.as_paged_or_default();
            let page_no: usize = pos.page.into();
            let size = paged.pages().get(page_no - 1)?.frame.size();
            Some(AnnotationPin {
                rtype: rec.rtype.clone(),
                label: rec.label.clone(),
                author: rec.author.clone(),
                content: rec.content.clone(),
                time: rec.time.clone(),
                status: rec.status.clone(),
                discussion: rec.discussion.clone(),
                page: page_no,
                x: pos.point.x.to_pt(),
                y: pos.point.y.to_pt(),
                page_width: size.x.to_pt(),
                page_height: size.y.to_pt(),
            })
        })
        .collect()
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 32
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Picks the label for a new annotation: the client-suggested one when valid
/// and free, else a fresh random 16-bit uppercase hex label like `7C42`.
fn fresh_label(records: &[AnnotationRecord], requested: Option<&str>) -> String {
    let taken = |label: &str| records.iter().any(|rec| rec.label == label);
    if let Some(label) = requested {
        if valid_label(label) && !taken(label) {
            return label.to_owned();
        }
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    loop {
        // splitmix-ish scramble; entropy needs are tiny and collisions are
        // checked against the existing records anyway.
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let label = format!("{:04X}", (seed >> 33) as u16);
        if !taken(&label) {
            return label;
        }
    }
}

/// The current time as an ISO 8601 UTC string, e.g. "2026-08-11T01:12:40Z".
pub fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The author name recorded for annotations created via the preview.
pub fn local_author() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

/// Computes the new on-disk content carrying the edit, and whether the edit
/// must additionally go through the editor buffer.
///
/// A clean file (disk identical to the compiled source) is spliced exactly
/// and needs no buffer edit — the editor reloads clean buffers silently. A
/// dirty file still gets a best-effort patch, anchored on the text around
/// the edit site, so external watchers of the disk see the label
/// immediately; the buffer edit remains the authoritative copy and its next
/// save overwrites the patch.
fn disk_edit(
    path: &std::path::Path,
    source_text: &str,
    range: std::ops::Range<usize>,
    insert: &str,
) -> (Option<String>, bool) {
    let Ok(disk) = std::fs::read_to_string(path) else {
        return (None, true);
    };
    if disk == source_text {
        let mut content = disk;
        content.replace_range(range, insert);
        return (Some(content), false);
    }
    (best_effort_patch(&disk, source_text, range, insert), true)
}

/// Applies the edit to a diverged disk content by re-locating the edit site.
/// Deletions locate the removed text itself (the label, unique by
/// construction); insertions anchor on up to 48 bytes of context around the
/// insertion point, retrying with shorter context when the exact window was
/// disturbed by unsaved edits. Gives up (returns `None`) rather than
/// patching an ambiguous location.
fn best_effort_patch(
    disk: &str,
    source_text: &str,
    range: std::ops::Range<usize>,
    insert: &str,
) -> Option<String> {
    if insert.is_empty() {
        let needle = source_text.get(range)?;
        let mut hits = disk.match_indices(needle);
        let (at, _) = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        let mut content = disk.to_owned();
        content.replace_range(at..at + needle.len(), "");
        return Some(content);
    }
    // Anchor on context before the insertion point, then on context after
    // it (for when the unsaved edits sit just before the click site).
    // Shorter windows are retried when a longer one was disturbed by the
    // unsaved edits; an ambiguous match is never patched.
    for ctx_len in [48usize, 24, 12] {
        let mut start = range.start.saturating_sub(ctx_len);
        while !source_text.is_char_boundary(start) {
            start += 1;
        }
        let Some(ctx) = source_text.get(start..range.start) else {
            continue;
        };
        if ctx.len() < 4 {
            continue;
        }
        let mut hits = disk.match_indices(ctx);
        let Some((at, _)) = hits.next() else {
            continue;
        };
        if hits.next().is_some() {
            break;
        }
        let mut content = disk.to_owned();
        content.insert_str(at + ctx.len(), insert);
        return Some(content);
    }
    for ctx_len in [48usize, 24, 12] {
        let mut end = (range.end + ctx_len).min(source_text.len());
        while !source_text.is_char_boundary(end) {
            end -= 1;
        }
        let Some(ctx) = source_text.get(range.end..end) else {
            continue;
        };
        if ctx.len() < 4 {
            continue;
        }
        let mut hits = disk.match_indices(ctx);
        let Some((at, _)) = hits.next() else {
            continue;
        };
        if hits.next().is_some() {
            break;
        }
        let mut content = disk.to_owned();
        content.insert_str(at, insert);
        return Some(content);
    }
    None
}

/// Prepares the edits creating an annotation at a clicked position: an
/// anchor label insertion at the end of the clicked word, and a new sidecar
/// entry.
pub fn prepare_annotate(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    let world = art.world();
    let Some(TypstDocument::Paged(doc)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    let page = doc
        .pages()
        .get(req.page.checked_sub(1).ok_or("bad page")?)
        .ok_or("no such page")?;
    let click = Point::new(Abs::pt(req.x), Abs::pt(req.y));
    let (start, _) =
        jump_from_click(world, &page.frame, click).ok_or("no text under the click")?;
    let id = start.span.id().ok_or("clicked text has no source")?;
    let source = world.source(id).map_err(|e| e.to_string())?;
    let node = source.find(start.span).ok_or("span not found in source")?;
    if node.kind() != SyntaxKind::Text {
        return Err("annotations can only anchor on markup text".into());
    }
    let node_range = node.range();
    // Advance to the end of the clicked word: a label sticks to the element
    // before it, and mid-word insertion would split the word.
    let mut at = (node_range.start + start.offset).min(node_range.end);
    let text = source.text();
    while at < node_range.end {
        match text[at..].chars().next() {
            Some(c) if !c.is_whitespace() => at += c.len_utf8(),
            _ => break,
        }
    }

    let path = world.path_for_id(id).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let pos = to_lsp_position(at, encoding, &source);

    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let (records, content) = read_sidecar(&sidecar);
    let rec = AnnotationRecord {
        rtype: "comment".into(),
        label: fresh_label(&records, req.label.as_deref()),
        author: local_author(),
        content: req.text.clone(),
        time: iso_now(),
        status: "created".into(),
        discussion: vec![],
    };
    let sidecar_content = format!("{content}{}", format_record(&rec));

    let new_text = anchor_text(&rec.label);
    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at, &new_text);
    Ok(AnnotationEdit {
        uri,
        disk_content,
        buffer_edit,
        path: path.to_path_buf(),
        range: lsp_types::Range::new(as_lsp(pos), as_lsp(pos)),
        new_text,
        label: rec.label,
        sidecar,
        sidecar_content,
    })
}

/// Prepares the edits deleting an annotation: removal of the anchor label
/// from whichever dependency file contains it, and of the sidecar entry.
pub fn prepare_delete(
    art: &LspCompiledArtifact,
    label: &str,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    if !valid_label(label) {
        return Err("bad annotation label".into());
    }
    let world = art.world();
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let content = std::fs::read_to_string(&sidecar).unwrap_or_default();
    let entries = parse_entries(&content);
    let entry = entries
        .iter()
        .find(|entry| entry.record.label == label)
        .ok_or_else(|| format!("unknown annotation: {label}"))?;
    let mut sidecar_content = content.clone();
    sidecar_content.replace_range(entry.span.clone(), "");

    // Find the anchor label in the compiled project's files.
    let needle = anchor_text(label);
    let hit = art.depended_files().iter().find_map(|&file| {
        let source = world.source(file).ok()?;
        let at = source.text().find(&needle)?;
        Some((file, at))
    });
    let (file, at) = hit.ok_or_else(|| format!("label {needle} not found in any source file"))?;
    let source = world.source(file).map_err(|e| e.to_string())?;
    let path = world.path_for_id(file).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let start = to_lsp_position(at, encoding, &source);
    let end = to_lsp_position(at + needle.len(), encoding, &source);

    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at + needle.len(), "");
    Ok(AnnotationEdit {
        label: label.to_owned(),
        uri,
        disk_content,
        buffer_edit,
        path: path.to_path_buf(),
        range: lsp_types::Range::new(as_lsp(start), as_lsp(end)),
        new_text: String::new(),
        sidecar,
        sidecar_content,
    })
}

/// Rewrites one entry of the sidecar in place via a modification of its
/// parsed record, returning (sidecar path, new content).
pub fn modify_record(
    art: &LspCompiledArtifact,
    label: &str,
    modify: impl FnOnce(&mut AnnotationRecord),
) -> Result<(PathBuf, String), String> {
    if !valid_label(label) {
        return Err("bad annotation label".into());
    }
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let content = std::fs::read_to_string(&sidecar)
        .map_err(|e| format!("failed to read {}: {e}", sidecar.display()))?;
    let entries = parse_entries(&content);
    let entry = entries
        .iter()
        .find(|entry| entry.record.label == label)
        .ok_or_else(|| format!("unknown annotation: {label}"))?;
    let mut record = entry.record.clone();
    modify(&mut record);
    let mut new_content = content.clone();
    new_content.replace_range(entry.span.clone(), &format_record(&record));
    Ok((sidecar, new_content))
}

/// Appends a discussion reply to an annotation.
pub fn prepare_reply(
    art: &LspCompiledArtifact,
    label: &str,
    text: &str,
) -> Result<(PathBuf, String), String> {
    modify_record(art, label, |record| {
        record.discussion.push(AnnotationReply {
            author: local_author(),
            time: iso_now(),
            content: text.to_owned(),
        });
    })
}

/// Sets the status of an annotation.
pub fn prepare_status(
    art: &LspCompiledArtifact,
    label: &str,
    status: &str,
) -> Result<(PathBuf, String), String> {
    if !matches!(status, "created" | "ongoing" | "resolved") {
        return Err(format!("bad status: {status}"));
    }
    modify_record(art, label, |record| {
        record.status = status.to_owned();
    })
}

fn as_lsp(pos: LspPosition) -> lsp_types::Position {
    lsp_types::Position::new(pos.line, pos.character)
}
