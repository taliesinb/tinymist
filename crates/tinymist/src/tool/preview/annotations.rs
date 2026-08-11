//! Preview annotations: comments anchored to document text via labels.
//!
//! An annotation is a cursor label like `<-7C42->` inserted into the
//! document source at the clicked word, plus a structured record in a
//! sidecar file `<main>.annos.typ` next to the main file (schema documented
//! in `annos_prelude.typ`, which is stamped at the top of every new
//! sidecar). The rendered position of each annotation is resolved by
//! querying the compiled document for the anchor label, so anchors survive
//! arbitrary edits around them. Records carry an author, ISO 8601 time, a
//! status, and a discussion thread that agents and the preview UI append to.
//!
//! The sidecar is *read* by actually evaluating it with the Typst compiler
//! (in a minimal isolated world) and querying its metadata elements — the
//! same data `typst query <sidecar> metadata` returns — so entries may be
//! written with any valid Typst, not just the literal template shapes.

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

/// Loads a dev asset from the source tree when available — so it can be
/// edited without rebuilding — falling back to the copy embedded at build
/// time. The source tree path is baked in at build time, which is exactly
/// right for a locally-built binary.
pub(crate) fn dev_asset(rel: &str, embedded: &'static str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tool/preview")
        .join(rel);
    std::fs::read_to_string(path).unwrap_or_else(|_| embedded.to_owned())
}

/// The schema documentation stamped at the top of new sidecar files.
pub fn annos_prelude() -> String {
    dev_asset("annos_prelude.typ", include_str!("annos_prelude.typ"))
}

/// The sidecar entry template, with `${...}` placeholders.
fn entry_template() -> String {
    dev_asset("annos_entry.tmpl", include_str!("annos_entry.tmpl"))
}

/// The discussion reply template, with `${...}` placeholders.
fn reply_template() -> String {
    dev_asset("annos_reply.tmpl", include_str!("annos_reply.tmpl"))
}

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
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationRecord {
    /// The annotation kind: "comment" | "question" | "request".
    #[serde(rename = "type")]
    pub rtype: String,
    /// The unique id, e.g. "7C42"; the document anchor is `<-7C42->`.
    pub uuid: String,
    /// The display letter shown on the pin: "a".."z", then "aa", ...
    pub letter: String,
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
    /// The unique id.
    pub uuid: String,
    /// The display letter shown on the pin.
    pub letter: String,
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
    /// A client-suggested uuid, e.g. `7C42` (random 16-bit uppercase hex,
    /// the browser). Used verbatim when valid and free; otherwise the server
    /// generates one.
    #[serde(default)]
    pub uuid: Option<String>,
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
/// disk — so external watchers see the anchor immediately; otherwise the edit
/// must go through the editor as a workspace edit. The sidecar is always
/// written on disk.
#[derive(Debug, Clone)]
pub struct AnnotationEdit {
    /// The annotation uuid.
    pub uuid: String,
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
    /// Creates an annotation at a clicked position. Returns the new uuid.
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String>;
    /// Deletes an annotation by uuid.
    fn remove(&self, uuid: &str) -> Result<(), String>;
    /// Appends a reply to an annotation's discussion.
    fn reply(&self, uuid: &str, text: &str) -> Result<(), String>;
    /// Sets an annotation's status.
    fn set_status(&self, uuid: &str, status: &str) -> Result<(), String>;
    /// Resolves a click to the exact would-be anchor position.
    fn probe(&self, page: usize, x: f64, y: f64) -> Result<ProbeResult, String>;
}

/// The document anchor text for a uuid, e.g. `<-7C42->`.
fn anchor_text(uuid: &str) -> String {
    format!("<-{uuid}->")
}

/// The label name queried in the compiled document, e.g. `-7C42-`.
fn anchor_name(uuid: &str) -> String {
    format!("-{uuid}-")
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

static SIDECAR_LIBRARY: std::sync::LazyLock<typst::utils::LazyHash<typst::Library>> =
    std::sync::LazyLock::new(|| {
        use typst::LibraryExt;
        typst::utils::LazyHash::new(typst::Library::default())
    });
static SIDECAR_BOOK: std::sync::LazyLock<typst::utils::LazyHash<typst::text::FontBook>> =
    std::sync::LazyLock::new(|| typst::utils::LazyHash::new(typst::text::FontBook::new()));

/// A minimal world for evaluating a sidecar file in isolation: the default
/// library, no fonts (a metadata-only document shapes no text), and no file
/// access (sidecars must be self-contained).
struct SidecarWorld {
    main: typst::syntax::Source,
}

impl typst::World for SidecarWorld {
    fn library(&self) -> &typst::utils::LazyHash<typst::Library> {
        &SIDECAR_LIBRARY
    }
    fn book(&self) -> &typst::utils::LazyHash<typst::text::FontBook> {
        &SIDECAR_BOOK
    }
    fn main(&self) -> typst::syntax::FileId {
        self.main.id()
    }
    fn source(&self, id: typst::syntax::FileId) -> typst::diag::FileResult<typst::syntax::Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(typst::diag::FileError::AccessDenied)
        }
    }
    fn file(&self, _id: typst::syntax::FileId) -> typst::diag::FileResult<typst::foundations::Bytes> {
        Err(typst::diag::FileError::AccessDenied)
    }
    fn font(&self, _index: usize) -> Option<typst::text::Font> {
        None
    }
    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<typst::foundations::Datetime> {
        None
    }
}

fn string_of(dict: &typst::foundations::Dict, key: &str) -> Option<String> {
    dict.get(key)
        .ok()
        .and_then(|value| value.clone().cast::<typst::foundations::Str>().ok())
        .map(|s| s.to_string())
}

fn record_from_value(value: &typst::foundations::Value) -> Option<AnnotationRecord> {
    let dict = value.clone().cast::<typst::foundations::Dict>().ok()?;
    let discussion = dict
        .get("discussion")
        .ok()
        .and_then(|value| value.clone().cast::<typst::foundations::Array>().ok())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| {
                    let reply = value.clone().cast::<typst::foundations::Dict>().ok()?;
                    Some(AnnotationReply {
                        author: string_of(&reply, "author")?,
                        time: string_of(&reply, "time").unwrap_or_default(),
                        content: string_of(&reply, "content")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(AnnotationRecord {
        rtype: string_of(&dict, "type").unwrap_or_else(|| "comment".into()),
        uuid: string_of(&dict, "uuid")?,
        letter: string_of(&dict, "letter").unwrap_or_default(),
        author: string_of(&dict, "author").unwrap_or_else(|| "unknown".into()),
        content: string_of(&dict, "content")?,
        time: string_of(&dict, "time").unwrap_or_default(),
        status: string_of(&dict, "status").unwrap_or_else(|| "created".into()),
        discussion,
    })
}

/// Parses the sidecar content by evaluating it with the Typst compiler and
/// querying all metadata elements — the same data `typst query <sidecar>
/// metadata` returns.
pub fn parse_records(content: &str) -> Vec<AnnotationRecord> {
    let main = typst::syntax::Source::detached(content);
    let world = SidecarWorld { main };
    let compiled = typst::compile::<reflexo_typst::TypstPagedDocument>(&world);
    let doc = match compiled.output {
        Ok(doc) => doc,
        Err(errors) => {
            log::warn!("sidecar failed to evaluate: {errors:?}");
            return vec![];
        }
    };
    use typst::foundations::NativeElement;
    use typst::introspection::Introspector as _;
    let selector = typst::introspection::MetadataElem::ELEM.select();
    doc.introspector()
        .query(&selector)
        .iter()
        .filter_map(|elem: &typst::foundations::Content| {
            let meta = elem.to_packed::<typst::introspection::MetadataElem>()?;
            record_from_value(&meta.value)
        })
        .collect()
}

/// Locates the byte span of an entry in the sidecar source by its trailing
/// `<note-LABEL>` uuid, for surgical replacement. Format changes only need
/// to keep that uuid after the entry's closing `))`.
fn entry_span(content: &str, uuid: &str) -> Option<std::ops::Range<usize>> {
    let note = format!("<note-{uuid}>");
    let note_at = content.find(&note)?;
    let start = content[..note_at].rfind("#metadata((")?;
    let mut end = note_at + note.len();
    if content[end..].starts_with('\n') {
        end += 1;
    }
    Some(start..end)
}

/// Formats one annotation record as a sidecar entry, using the editable
/// `annos_entry.tmpl` / `annos_reply.tmpl` templates. Templates must keep
/// the `key: "value"` field shapes the parser recognizes.
pub fn format_record(rec: &AnnotationRecord) -> String {
    let discussion = if rec.discussion.is_empty() {
        "()".to_owned()
    } else {
        let reply_tmpl = reply_template();
        let replies: String = rec
            .discussion
            .iter()
            .map(|reply| {
                reply_tmpl
                    .replace("${author}", &escape(&reply.author))
                    .replace("${time}", &escape(&reply.time))
                    .replace("${content}", &escape(&reply.content))
            })
            .collect();
        format!("(\n{replies}  )")
    };
    entry_template()
        .replace("${type}", &escape(&rec.rtype))
        .replace("${uuid}", &escape(&rec.uuid))
        .replace("${letter}", &escape(&rec.letter))
        .replace("${author}", &escape(&rec.author))
        .replace("${content}", &escape(&rec.content))
        .replace("${time}", &escape(&rec.time))
        .replace("${status}", &escape(&rec.status))
        .replace("${discussion}", &discussion)
}

fn read_sidecar(path: &std::path::Path) -> (Vec<AnnotationRecord>, String) {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let records = parse_records(&content);
            (records, content)
        }
        Err(_) => (vec![], annos_prelude()),
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
    let world = art.world();
    records
        .iter()
        .filter_map(|rec| {
            // Prefer the exact anchor position: resolve the `<-XXXX->` byte
            // offset in the source like a cursor, which lands on the precise
            // inter-character point. Fall back to the labeled element's
            // position (the start of its text run).
            let needle = anchor_text(&rec.uuid);
            let exact = art.depended_files().iter().find_map(|&file| {
                let source = world.source(file).ok()?;
                let at = source.text().find(&needle)?;
                exact_anchor_position(paged, &source, at)
            });
            let pos: PagedPosition = match exact {
                Some(pos) => pos,
                None => {
                    let uuid = Label::new(PicoStr::intern(&anchor_name(&rec.uuid)))?;
                    let elem = introspector.query_label(uuid).ok()?;
                    let loc = elem.location()?;
                    introspector.position(loc)?.as_paged_or_default()
                }
            };
            let page_no: usize = pos.page.into();
            let size = paged.pages().get(page_no - 1)?.frame.size();
            Some(AnnotationPin {
                rtype: rec.rtype.clone(),
                uuid: rec.uuid.clone(),
                letter: rec.letter.clone(),
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

fn valid_label(uuid: &str) -> bool {
    !uuid.is_empty()
        && uuid.len() <= 32
        && uuid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Picks the uuid for a new annotation: the client-suggested one when valid
/// and free, else a fresh random 16-bit uppercase hex uuid like `7C42`.
fn fresh_label(records: &[AnnotationRecord], requested: Option<&str>) -> String {
    let taken = |uuid: &str| records.iter().any(|rec| rec.uuid == uuid);
    if let Some(uuid) = requested {
        if valid_label(uuid) && !taken(uuid) {
            return uuid.to_owned();
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
        let uuid = format!("{:04X}", (seed >> 33) as u16);
        if !taken(&uuid) {
            return uuid;
        }
    }
}

/// Parses a display letter as a 1-based index in the sequence
/// a..z, aa, ab, ... (bijective base 26).
fn letter_index(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    s.chars().try_fold(0u64, |acc, c| {
        c.is_ascii_lowercase()
            .then(|| acc * 26 + (c as u64 - 'a' as u64 + 1))
    })
}

/// Formats a 1-based index as a display letter: 1 = "a", 26 = "z",
/// 27 = "aa", ...
fn index_letter(mut n: u64) -> String {
    let mut out = vec![];
    while n > 0 {
        n -= 1;
        out.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The next free display letter: one past the maximum in use.
fn next_letter(records: &[AnnotationRecord]) -> String {
    let max = records
        .iter()
        .filter_map(|rec| letter_index(&rec.letter))
        .max()
        .unwrap_or(0);
    index_letter(max + 1)
}

/// Resolves a source byte position to its exact rendered point, at glyph
/// granularity — unlike `jump_from_cursor`, which only returns the position
/// of the containing text run. Used to place the annotation caret at the
/// precise inter-character anchor position.
fn exact_anchor_position(
    paged: &reflexo_typst::TypstPagedDocument,
    source: &typst::syntax::Source,
    cursor: usize,
) -> Option<PagedPosition> {
    use typst_shim::syntax::LinkedNodeExt;
    let node = typst::syntax::LinkedNode::new(source.root()).leaf_at_compat(cursor)?;
    if node.kind() != SyntaxKind::Text {
        return None;
    }
    let span = node.span();
    let target = cursor.checked_sub(node.offset())?;
    for (idx, page) in paged.pages().iter().enumerate() {
        let mut after = None;
        let exact = anchor_in_frame(&page.frame, span, target, Point::zero(), &mut after);
        if let Some(point) = exact.or(after) {
            return Some(PagedPosition {
                page: std::num::NonZeroUsize::new(idx + 1)?,
                point,
            });
        }
    }
    None
}

/// Finds the point of the glyph boundary at `target` (a byte offset within
/// the node with the given span). An exact hit is the left edge of the first
/// glyph at or past the target; `after` collects the right edge of the last
/// glyph ending at or before it (for anchors at the end of a word).
fn anchor_in_frame(
    frame: &typst::layout::Frame,
    span: typst::syntax::Span,
    target: usize,
    origin: Point,
    after: &mut Option<Point>,
) -> Option<Point> {
    use typst::layout::FrameItem;
    for &(pos, ref item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                if let Some(found) =
                    anchor_in_frame(&group.frame, span, target, origin + pos, after)
                {
                    return Some(found);
                }
            }
            FrameItem::Text(text) => {
                let mut x = origin.x + pos.x;
                let y = origin.y + pos.y;
                for glyph in &text.glyphs {
                    let advance = glyph.x_advance.at(text.size);
                    let (gspan, goffset) = glyph.span;
                    if gspan == span {
                        let goffset = goffset as usize;
                        if target <= goffset {
                            return Some(Point::new(x, y));
                        }
                        if goffset + glyph.range().len() <= target {
                            *after = Some(Point::new(x + advance, y));
                        }
                    }
                    x += advance;
                }
            }
            _ => {}
        }
    }
    None
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
/// the edit site, so external watchers of the disk see the anchor
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
/// Deletions locate the removed text itself (the anchor label, unique by
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

/// The resolved would-be anchor of a click, as returned by the probe
/// endpoint: the exact caret position where the anchor label would land.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    /// The 1-based page number.
    pub page: usize,
    /// The x coordinate of the would-be anchor, in pt.
    pub x: f64,
    /// The y coordinate of the would-be anchor, in pt.
    pub y: f64,
}

/// Resolves a click to the source position where an anchor label would be
/// inserted: the end of the clicked word. Errors when the click does not
/// hit markup text (margins, whitespace past the end of the document, math,
/// generated content).
fn resolve_click(
    art: &LspCompiledArtifact,
    page_no: usize,
    x: f64,
    y: f64,
) -> Result<(typst::syntax::FileId, typst::syntax::Source, usize), String> {
    let world = art.world();
    let Some(TypstDocument::Paged(doc)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    let page = doc
        .pages()
        .get(page_no.checked_sub(1).ok_or("bad page")?)
        .ok_or("no such page")?;
    let click = Point::new(Abs::pt(x), Abs::pt(y));
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
    Ok((id, source, at))
}

/// Resolves a click to the exact would-be anchor position, without editing
/// anything.
pub fn probe_annotate(
    art: &LspCompiledArtifact,
    page_no: usize,
    x: f64,
    y: f64,
) -> Result<ProbeResult, String> {
    let (_, source, at) = resolve_click(art, page_no, x, y)?;
    let Some(TypstDocument::Paged(paged)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    let pos = exact_anchor_position(&paged, &source, at)
        .ok_or("cannot resolve the anchor position")?;
    Ok(ProbeResult {
        page: pos.page.into(),
        x: pos.point.x.to_pt(),
        y: pos.point.y.to_pt(),
    })
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
    let (id, source, at) = resolve_click(art, req.page, req.x, req.y)?;

    let path = world.path_for_id(id).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let pos = to_lsp_position(at, encoding, &source);

    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let (records, content) = read_sidecar(&sidecar);
    let rec = AnnotationRecord {
        rtype: "comment".into(),
        uuid: fresh_label(&records, req.uuid.as_deref()),
        letter: next_letter(&records),
        author: local_author(),
        content: req.text.clone(),
        time: iso_now(),
        status: "created".into(),
        discussion: vec![],
    };
    let sidecar_content = format!("{content}{}", format_record(&rec));

    let new_text = anchor_text(&rec.uuid);
    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at, &new_text);
    Ok(AnnotationEdit {
        uri,
        disk_content,
        buffer_edit,
        path: path.to_path_buf(),
        range: lsp_types::Range::new(as_lsp(pos), as_lsp(pos)),
        new_text,
        uuid: rec.uuid,
        sidecar,
        sidecar_content,
    })
}

/// Prepares the edits deleting an annotation: removal of the anchor label
/// from whichever dependency file contains it, and of the sidecar entry.
pub fn prepare_delete(
    art: &LspCompiledArtifact,
    uuid: &str,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    if !valid_label(uuid) {
        return Err("bad annotation uuid".into());
    }
    let world = art.world();
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let content = std::fs::read_to_string(&sidecar).unwrap_or_default();
    let span = entry_span(&content, uuid)
        .ok_or_else(|| format!("unknown annotation: {uuid}"))?;
    let mut sidecar_content = content.clone();
    sidecar_content.replace_range(span, "");

    // Find the anchor label in the compiled project's files.
    let needle = anchor_text(uuid);
    let hit = art.depended_files().iter().find_map(|&file| {
        let source = world.source(file).ok()?;
        let at = source.text().find(&needle)?;
        Some((file, at))
    });
    let (file, at) = hit.ok_or_else(|| format!("uuid {needle} not found in any source file"))?;
    let source = world.source(file).map_err(|e| e.to_string())?;
    let path = world.path_for_id(file).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let start = to_lsp_position(at, encoding, &source);
    let end = to_lsp_position(at + needle.len(), encoding, &source);

    let (disk_content, buffer_edit) = disk_edit(&path, source.text(), at..at + needle.len(), "");
    Ok(AnnotationEdit {
        uuid: uuid.to_owned(),
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
    uuid: &str,
    modify: impl FnOnce(&mut AnnotationRecord),
) -> Result<(PathBuf, String), String> {
    if !valid_label(uuid) {
        return Err("bad annotation uuid".into());
    }
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let content = std::fs::read_to_string(&sidecar)
        .map_err(|e| format!("failed to read {}: {e}", sidecar.display()))?;
    let records = parse_records(&content);
    let mut record = records
        .into_iter()
        .find(|record| record.uuid == uuid)
        .ok_or_else(|| format!("unknown annotation: {uuid}"))?;
    let span = entry_span(&content, uuid)
        .ok_or_else(|| format!("cannot locate the entry of {uuid} in the sidecar"))?;
    modify(&mut record);
    let mut new_content = content.clone();
    new_content.replace_range(span, &format_record(&record));
    Ok((sidecar, new_content))
}

/// Appends a discussion reply to an annotation.
pub fn prepare_reply(
    art: &LspCompiledArtifact,
    uuid: &str,
    text: &str,
) -> Result<(PathBuf, String), String> {
    modify_record(art, uuid, |record| {
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
    uuid: &str,
    status: &str,
) -> Result<(PathBuf, String), String> {
    if !matches!(status, "created" | "ongoing" | "resolved") {
        return Err(format!("bad status: {status}"));
    }
    modify_record(art, uuid, |record| {
        record.status = status.to_owned();
    })
}

fn as_lsp(pos: LspPosition) -> lsp_types::Position {
    lsp_types::Position::new(pos.line, pos.character)
}
