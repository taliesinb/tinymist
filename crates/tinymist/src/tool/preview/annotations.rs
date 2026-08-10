//! Preview annotations: comments anchored to document text via labels.
//!
//! An annotation is a short label like `<A-E4DE>` inserted into the document
//! source at the clicked word (through a workspace edit, so it goes through
//! the editor buffer and is undoable), plus a record holding the comment
//! text in a sidecar file `<main>.annos.typ` next to the main file.
//! The rendered position of each annotation is resolved by querying the
//! compiled document for the label, so anchors survive arbitrary edits
//! around them.

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

/// A stored annotation record from the sidecar file.
#[derive(Debug, Clone)]
pub struct AnnotationRecord {
    /// The annotation id, which is also the label name in the document.
    pub id: String,
    /// The comment text.
    pub text: String,
    /// Creation time, in seconds since the unix epoch.
    pub created: u64,
    /// Whether the annotation has been addressed. Agents flip this to true
    /// in the sidecar; completed annotations render grayed out.
    pub completed: bool,
}

/// An annotation resolved onto the rendered document, sent to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationPin {
    /// The annotation id.
    pub id: String,
    /// The comment text.
    pub text: String,
    /// Creation time, in seconds since the unix epoch.
    pub created: u64,
    /// Whether the annotation has been addressed.
    pub completed: bool,
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
    /// A client-suggested id, e.g. `A-E4DE` (random 16-bit suffix stamped by
    /// the browser). Used verbatim when valid and free; otherwise the server
    /// generates one.
    #[serde(default)]
    pub id: Option<String>,
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
    /// The annotation id.
    pub id: String,
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
/// construction); insertions anchor on up to 48 bytes of context ending at
/// the insertion point, retrying with shorter context when the exact window
/// was disturbed by unsaved edits. Gives up (returns `None`) rather than
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

/// The server-side annotation API exposed to the preview http server.
pub trait AnnotationServer: Send + Sync {
    /// Creates an annotation at a clicked position. Returns the new id.
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String>;
    /// Deletes an annotation by id.
    fn remove(&self, id: &str) -> Result<(), String>;
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
    let s = s.strip_prefix('"')?;
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

/// Parses the sidecar file content into annotation records. Only entries in
/// the format written by [`format_record`] are recognized.
pub fn parse_records(content: &str) -> Vec<AnnotationRecord> {
    let mut records = vec![];
    for block in content.split("#metadata((").skip(1) {
        let Some(end) = block.find("))") else {
            continue;
        };
        let block = &block[..end];
        let id = read_field(block, "id").and_then(read_string);
        let text = read_field(block, "text").and_then(read_string);
        let created = read_field(block, "created")
            .and_then(|rest| rest.split(',').next())
            .and_then(|num| num.trim().parse::<u64>().ok());
        let completed = read_field(block, "completed")
            .is_some_and(|rest| rest.trim_start().starts_with("true"));
        if let (Some((id, _)), Some((text, _)), Some(created)) = (id, text, created) {
            records.push(AnnotationRecord {
                id: unescape(id),
                text: unescape(text),
                created,
                completed,
            });
        }
    }
    records
}

/// Formats one annotation record as a sidecar entry.
pub fn format_record(rec: &AnnotationRecord) -> String {
    format!(
        "#metadata((\n  id: \"{}\",\n  text: \"{}\",\n  created: {},\n  completed: {},\n)) <{}-note>\n",
        escape(&rec.id),
        escape(&rec.text),
        rec.created,
        rec.completed,
        rec.id,
    )
}

const SIDECAR_HEADER: &str = "\
// Annotations created from the tinymist preview. Each entry corresponds to
// a matching <A-XXXX> label anchored in the document source; removing an
// entry or its label orphans the other half harmlessly.\n\n";

fn read_sidecar(path: &std::path::Path) -> (Vec<AnnotationRecord>, String) {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let records = parse_records(&content);
            (records, content)
        }
        Err(_) => (vec![], SIDECAR_HEADER.to_owned()),
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
            let label = Label::new(PicoStr::intern(&rec.id))?;
            let elem = introspector.query_label(label).ok()?;
            let loc = elem.location()?;
            let pos: PagedPosition = introspector.position(loc)?.as_paged_or_default();
            let page_no: usize = pos.page.into();
            let size = paged.pages().get(page_no - 1)?.frame.size();
            Some(AnnotationPin {
                id: rec.id.clone(),
                text: rec.text.clone(),
                created: rec.created,
                completed: rec.completed,
                page: page_no,
                x: pos.point.x.to_pt(),
                y: pos.point.y.to_pt(),
                page_width: size.x.to_pt(),
                page_height: size.y.to_pt(),
            })
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Picks the id for a new annotation: the client-suggested one when valid
/// and free, else a fresh `A-XXXX` with a random 16-bit hex suffix.
fn fresh_id(records: &[AnnotationRecord], requested: Option<&str>) -> String {
    let taken = |id: &str| records.iter().any(|rec| rec.id == id);
    if let Some(id) = requested {
        if valid_id(id) && !taken(id) {
            return id.to_owned();
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
        let id = format!("A-{:04X}", (seed >> 33) as u16);
        if !taken(&id) {
            return id;
        }
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Prepares the edits creating an annotation at a clicked position: a label
/// insertion at the end of the clicked word, and a new sidecar entry.
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
        id: fresh_id(&records, req.id.as_deref()),
        text: req.text.clone(),
        created: now_epoch(),
        completed: false,
    };
    let sidecar_content = format!("{content}{}", format_record(&rec));

    let new_text = format!("<{}>", rec.id);
    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at, &new_text);
    Ok(AnnotationEdit {
        uri,
        disk_content,
        buffer_edit,
        path: path.to_path_buf(),
        range: lsp_types::Range::new(as_lsp(pos), as_lsp(pos)),
        new_text,
        id: rec.id,
        sidecar,
        sidecar_content,
    })
}

/// Prepares the edits deleting an annotation: removal of the label from
/// whichever dependency file contains it, and of the sidecar entry.
pub fn prepare_delete(
    art: &LspCompiledArtifact,
    id: &str,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("bad annotation id".into());
    }
    let world = art.world();
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let (records, content) = read_sidecar(&sidecar);
    if !records.iter().any(|rec| rec.id == id) {
        return Err(format!("unknown annotation: {id}"));
    }
    let sidecar_content = strip_record(&content, id);

    // Find the label in the compiled project's files.
    let needle = format!("<{id}>");
    let hit = art.depended_files().iter().find_map(|&file| {
        let source = world.source(file).ok()?;
        let at = source.text().find(&needle)?;
        Some((file, at))
    });
    let (file, at) = hit.ok_or_else(|| format!("label <{id}> not found in any source file"))?;
    let source = world.source(file).map_err(|e| e.to_string())?;
    let path = world.path_for_id(file).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let start = to_lsp_position(at, encoding, &source);
    let end = to_lsp_position(at + needle.len(), encoding, &source);

    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at + needle.len(), "");
    Ok(AnnotationEdit {
        id: id.to_owned(),
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

/// Removes the entry with the given id from the sidecar content.
fn strip_record(content: &str, id: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(at) = rest.find("#metadata((") {
        let (before, block) = rest.split_at(at);
        out.push_str(before);
        let end = block
            .find(">\n")
            .map(|e| e + 2)
            .or_else(|| block.find('>').map(|e| e + 1))
            .unwrap_or(block.len());
        let (entry, after) = block.split_at(end);
        let is_target = parse_records(entry).iter().any(|rec| rec.id == id);
        if !is_target {
            out.push_str(entry);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn as_lsp(pos: LspPosition) -> lsp_types::Position {
    lsp_types::Position::new(pos.line, pos.character)
}
