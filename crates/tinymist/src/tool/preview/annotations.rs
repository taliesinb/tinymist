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
    /// What the annotation refers to: point, word, sentence, span, block
    /// or para. The frontend decorates each scope differently.
    pub scope: String,
    /// The rendered boxes of the referent, one per line (empty for a
    /// point anchor).
    pub rects: Vec<AnnotRect>,
    /// The x of the page's leftmost ink, in pt: the fallback rail for region
    /// marks when the referent has no gutter of its own.
    pub rail_x: f64,
    /// The x, in pt, of the leftmost thing the referent covers — including
    /// the bullets and numbers of any list items inside it, which are not
    /// part of its own boxes. Region marks hang off this.
    pub gutter_x: f64,
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
    /// The 1-based page number of the click, when created by clicking.
    #[serde(default)]
    pub page: Option<usize>,
    /// The x coordinate of the click, in pt.
    #[serde(default)]
    pub x: Option<f64>,
    /// The y coordinate of the click, in pt.
    #[serde(default)]
    pub y: Option<f64>,
    /// The comment text.
    pub text: String,
    /// The scope to create, when the client has already decided (a click
    /// between words makes a point, not a word).
    #[serde(default)]
    pub scope: Option<String>,
    /// A source range, when the client already knows the span it wants.
    #[serde(default)]
    pub s: Option<usize>,
    /// The end of that source range.
    #[serde(default)]
    pub e: Option<usize>,
    /// The end point of a drag, when the annotation is a span.
    #[serde(default)]
    pub page2: Option<usize>,
    /// The x coordinate of the drag end, in pt.
    #[serde(default)]
    pub x2: Option<f64>,
    /// The y coordinate of the drag end, in pt.
    #[serde(default)]
    pub y2: Option<f64>,
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
    /// The document's annotatable structure, for local hit-testing.
    fn layout(&self) -> Result<LayoutMap, String>;
    /// The words of one region, for local word and span hit-testing.
    fn words(&self, s: usize, e: usize) -> Result<Vec<LayoutWord>, String>;
    /// Resolves a drag to the span it would create.
    fn probe_span(
        &self,
        a: (usize, f64, f64),
        b: (usize, f64, f64),
    ) -> Result<ProbeResult, String>;
}

/// What an annotation refers to. The scope is carried by the anchor label
/// itself (`<7C42.word>`), so it is always visible in the document and
/// never has to be stored — or kept in sync — in the sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A position between words: "insert something here". No decoration.
    Point,
    /// The word the anchor follows.
    Word,
    /// The sentence containing the anchor.
    Sentence,
    /// The text between a `.span.begin` and `.span.end` anchor pair.
    Span,
    /// A generated block: a figure, a `#lorem(..)` call, ...
    Block,
    /// A single list item: a bullet, a numbered entry, a term.
    Item,
    /// The paragraph containing the anchor; marked in the gutter.
    Para,
}

impl Scope {
    fn from_suffix(suffix: &str) -> Option<Self> {
        Some(match suffix {
            "point" => Scope::Point,
            "word" => Scope::Word,
            "sentence" => Scope::Sentence,
            "span.begin" | "span.end" => Scope::Span,
            "block" => Scope::Block,
            "item" => Scope::Item,
            "para" => Scope::Para,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Scope::Point => "point",
            Scope::Word => "word",
            Scope::Sentence => "sentence",
            Scope::Span => "span",
            Scope::Block => "block",
            Scope::Item => "item",
            Scope::Para => "para",
        }
    }
}

/// The document anchor text for a uuid, e.g. `<7C42.word>`.
fn anchor_text(uuid: &str, scope: Scope) -> String {
    match scope {
        Scope::Span => format!("<{uuid}.span.begin>"),
        scope => format!("<{uuid}.{}>", scope.as_str()),
    }
}

/// Finds every anchor of an annotation in a source: their byte offsets and
/// scopes, in document order.
fn find_anchors(text: &str, uuid: &str) -> Vec<(usize, Scope, String)> {
    let prefix = format!("<{uuid}.");
    let mut out = vec![];
    let mut from = 0;
    while let Some(rel) = text[from..].find(&prefix) {
        let start = from + rel;
        let Some(close) = text[start..].find('>') else {
            break;
        };
        let suffix = &text[start + prefix.len()..start + close];
        if let Some(scope) = Scope::from_suffix(suffix) {
            out.push((start, scope, text[start..start + close + 1].to_owned()));
        }
        from = start + close + 1;
    }
    out
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

/// Serializes sidecar mutations in-process; external writers (agents
/// editing the file directly) are detected via the mtime check below.
static SIDECAR_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn sidecar_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Commits a sidecar mutation with optimistic concurrency: runs `prepare`
/// (which reads the sidecar), and writes its content only if the file's
/// mtime is unchanged since just before the read — retrying the whole
/// prepare a few times when an external writer (e.g. an agent editing the
/// file) got in between. In-process callers are serialized by a lock.
pub fn commit_sidecar<E>(
    art: &LspCompiledArtifact,
    mut prepare: impl FnMut() -> Result<(PathBuf, String, E), String>,
) -> Result<E, String> {
    let _guard = SIDECAR_LOCK.lock();
    for _ in 0..3 {
        let seen = sidecar_path(art).and_then(|p| sidecar_mtime(&p));
        let (path, content, extra) = prepare()?;
        if sidecar_path(art).and_then(|p| sidecar_mtime(&p)) != seen {
            // The file changed while we were preparing; re-read and retry.
            continue;
        }
        std::fs::write(&path, &content)
            .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
        return Ok(extra);
    }
    Err("the sidecar kept changing concurrently; giving up".into())
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
    let world = art.world();
    records
        .iter()
        .filter_map(|rec| {
            // The anchors carry the scope in their labels, so what an
            // annotation refers to is read straight off the document.
            let (file, source, anchors) = art.depended_files().iter().find_map(|&file| {
                let source = world.source(file).ok()?;
                let anchors = find_anchors(source.text(), &rec.uuid);
                (!anchors.is_empty()).then(|| (file, source, anchors))
            })?;
            let text = source.text();
            let (at, scope, label) = anchors.first().cloned()?;
            let after = at + label.len();

            // The source range the annotation refers to, per scope.
            let range = match scope {
                Scope::Point => None,
                Scope::Word => Some(word_range(text, at)),
                Scope::Sentence => Some(sentence_range(text, at)),
                Scope::Para => Some(paragraph_range(text, at)),
                Scope::Span => {
                    let end = anchors
                        .iter()
                        .rev()
                        .find(|(off, _, l)| *off > at && l.ends_with(".end>"));
                    Some(after..end.map(|(off, _, _)| *off).unwrap_or(after))
                }
                Scope::Item => {
                    // Any block of text: a list item, a heading, or failing
                    // both, the paragraph. The marker points at its first
                    // line, so this is uniform across block kinds.
                    use typst_shim::syntax::LinkedNodeExt;
                    let node = typst::syntax::LinkedNode::new(source.root())
                        .leaf_at_compat(at)?;
                    let mut cursor = node;
                    let mut found = None;
                    loop {
                        if matches!(
                            cursor.kind(),
                            SyntaxKind::ListItem
                                | SyntaxKind::EnumItem
                                | SyntaxKind::TermItem
                                | SyntaxKind::Heading
                        ) {
                            found = Some(cursor.range());
                            break;
                        }
                        match cursor.parent().cloned() {
                            Some(parent) => cursor = parent,
                            None => break,
                        }
                    }
                    Some(found.unwrap_or_else(|| paragraph_range(text, at)))
                }
                Scope::Block => {
                    use typst_shim::syntax::LinkedNodeExt;
                    let node = typst::syntax::LinkedNode::new(source.root())
                        .leaf_at_compat(at)?;
                    let mut cursor = node;
                    loop {
                        let parent = cursor.parent().cloned();
                        match parent {
                            Some(parent) if parent.kind() != SyntaxKind::Markup => {
                                cursor = parent
                            }
                            _ => break,
                        }
                    }
                    Some(cursor.range())
                }
            };
            let mut rects = range
                .as_ref()
                .map(|range| range_rects(paged, file, &source, range, scope == Scope::Block))
                .unwrap_or_default();

            // The marker's own position: the anchor point for scopes that
            // point at a spot, else derived from the rects by the frontend.
            let gutter_x = gutter_of(paged, &rects);
            let point = exact_anchor_position(paged, &source, at);
            let (page, x, y) = match (rects.first(), point) {
                (Some(first), _) if scope != Scope::Point => {
                    let last = rects.last().unwrap_or(first);
                    match scope {
                        // An item is marked beside its bullet, which is not
                        // part of its boxes.
                        Scope::Item => (first.page, gutter_x, (first.y0 + first.y1) / 2.0),
                        Scope::Block | Scope::Para => {
                            (first.page, first.x0, (first.y0 + last.y1) / 2.0)
                        }
                        _ => (last.page, (last.x0 + last.x1) / 2.0, last.y1),
                    }
                }
                (_, Some(pos)) => (
                    pos.page.into(),
                    pos.point.x.to_pt(),
                    pos.point.y.to_pt(),
                ),
                _ => return None,
            };
            let size = paged.pages().get(page.checked_sub(1)?)?.frame.size();
            Some(AnnotationPin {
                rtype: rec.rtype.clone(),
                uuid: rec.uuid.clone(),
                letter: rec.letter.clone(),
                author: rec.author.clone(),
                content: rec.content.clone(),
                time: rec.time.clone(),
                status: rec.status.clone(),
                discussion: rec.discussion.clone(),
                page,
                x,
                y,
                page_width: size.x.to_pt(),
                page_height: size.y.to_pt(),
                scope: scope.as_str().into(),
                rail_x: page_rail(&paged.pages().get(page - 1)?.frame),
                gutter_x,
                rects,
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
    /// The scope a new annotation here would get: "word" on text, "block"
    /// on generated content.
    pub scope: String,
    /// The rendered boxes of the would-be referent.
    pub rects: Vec<AnnotRect>,
    /// The 1-based page number.
    pub page: usize,
    /// The x coordinate of the would-be anchor, in pt.
    pub x: f64,
    /// The y coordinate of the would-be anchor, in pt.
    pub y: f64,
}

/// One rendered box of an annotation's target, in pt, on a page.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotRect {
    /// The 1-based page number.
    pub page: usize,
    /// The left edge.
    pub x0: f64,
    /// The top edge.
    pub y0: f64,
    /// The right edge.
    pub x1: f64,
    /// The bottom edge.
    pub y1: f64,
}

/// Collects the rendered boxes of everything whose span lies in a source
/// range, one per rendered line (text wraps, so a range is a sequence of
/// boxes rather than one).
fn range_rects(
    paged: &reflexo_typst::TypstPagedDocument,
    id: typst::syntax::FileId,
    source: &typst::syntax::Source,
    range: &std::ops::Range<usize>,
    // Generated blocks (figures, diagrams) have spans only on a few of
    // their items, so a hit expands to the enclosing group. Text scopes
    // must not do that, or a word inflates to its whole line.
    use_group: bool,
) -> Vec<AnnotRect> {
    fn walk(
        frame: &typst::layout::Frame,
        id: typst::syntax::FileId,
        source: &typst::syntax::Source,
        range: &std::ops::Range<usize>,
        origin: Point,
        rows: &mut Vec<(f64, f64, f64, f64)>,
        group: Option<(f64, f64, f64, f64)>,
        use_group: bool,
    ) {
        use typst::layout::FrameItem;
        fn add(
            rows: &mut Vec<(f64, f64, f64, f64)>,
            group: Option<(f64, f64, f64, f64)>,
            x0: f64,
            y0: f64,
            x1: f64,
            y1: f64,
        ) {
            let (x0, y0, x1, y1) = match group {
                Some(g) => g,
                None => (x0, y0, x1, y1),
            };
            // Merge into a row sharing (roughly) the same baseline.
            for row in rows.iter_mut() {
                if (row.3 - y1).abs() < 1.5 {
                    row.0 = row.0.min(x0);
                    row.1 = row.1.min(y0);
                    row.2 = row.2.max(x1);
                    row.3 = row.3.max(y1);
                    return;
                }
            }
            rows.push((x0, y0, x1, y1));
        }
        let in_range = |span: typst::syntax::Span| {
            span.id() == Some(id)
                && typst_shim::syntax::source_range(source, span)
                    .is_some_and(|r| r.start >= range.start && r.end <= range.end)
        };
        for &(pos, ref item) in frame.items() {
            let at = origin + pos;
            match item {
                FrameItem::Group(inner) => {
                    let size = inner.frame.size();
                    let gb = use_group.then(|| {
                        (
                            at.x.to_pt(),
                            at.y.to_pt(),
                            at.x.to_pt() + size.x.to_pt(),
                            at.y.to_pt() + size.y.to_pt(),
                        )
                    });
                    walk(
                        &inner.frame, id, source, range, at, rows,
                        group.or(gb), use_group,
                    );
                }
                FrameItem::Text(text) => {
                    let mut x = at.x;
                    for glyph in &text.glyphs {
                        let advance = glyph.x_advance.at(text.size);
                        // A glyph's span covers its whole text node, so the
                        // glyph's own source range is that span's start plus
                        // the glyph's offset within it.
                        let glyph_in_range = glyph.span.0.id() == Some(id)
                            && typst_shim::syntax::source_range(source, glyph.span.0)
                                .is_some_and(|r| {
                                    let start = r.start + glyph.span.1 as usize;
                                    let end = (start + glyph.range().len()).min(r.end);
                                    start >= range.start && end <= range.end
                                });
                        if glyph_in_range {
                            add(
                                rows,
                                group,
                                x.to_pt(),
                                at.y.to_pt() - text.size.to_pt() * 0.78,
                                (x + advance).to_pt(),
                                at.y.to_pt() + text.size.to_pt() * 0.22,
                            );
                        }
                        x += advance;
                    }
                }
                FrameItem::Shape(shape, span) => {
                    if let typst::visualize::Geometry::Rect(size) = shape.geometry {
                        if in_range(*span) {
                            add(
                                rows,
                                group,
                                at.x.to_pt(),
                                at.y.to_pt(),
                                at.x.to_pt() + size.x.to_pt(),
                                at.y.to_pt() + size.y.to_pt(),
                            );
                        }
                    }
                }
                FrameItem::Image(_, size, span) => {
                    if in_range(*span) {
                        add(
                            rows,
                            group,
                            at.x.to_pt(),
                            at.y.to_pt(),
                            at.x.to_pt() + size.x.to_pt(),
                            at.y.to_pt() + size.y.to_pt(),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    let mut out = vec![];
    for (idx, page) in paged.pages().iter().enumerate() {
        let mut rows = vec![];
        walk(&page.frame, id, source, range, Point::zero(), &mut rows, None, use_group);
        rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
        out.extend(rows.into_iter().map(|(x0, y0, x1, y1)| AnnotRect {
            page: idx + 1,
            x0,
            y0,
            x1,
            y1,
        }));
    }
    out
}

/// List markers ("1.", "•") are laid out by the list, not by the item's own
/// markup, so they fall outside the item's source range. Extends a rect
/// leftward to the nearest such glyph on the same line, so an item's box
/// starts at its marker.
/// The leftmost x a set of line boxes reaches once each line is extended to
/// the marker in front of it, in pt: for a region built out of list items,
/// the bullet column; for ordinary text, its own left edge.
fn gutter_of(paged: &reflexo_typst::TypstPagedDocument, rects: &[AnnotRect]) -> f64 {
    let mut best = f64::INFINITY;
    for rect in rects {
        let mut probe = *rect;
        if let Some(page) = paged.pages().get(rect.page - 1) {
            extend_to_marker(&page.frame, &mut probe);
        }
        best = best.min(probe.x0);
    }
    if best.is_finite() {
        best
    } else {
        0.0
    }
}

/// The x of the leftmost ink on a page, in pt. Markers, bullets and text all
/// count, so a strip placed here is outside everything the page draws — the
/// one thing the client cannot work out from a region's own boxes.
fn page_rail(page: &typst::layout::Frame) -> f64 {
    fn walk(frame: &typst::layout::Frame, origin: Point, best: &mut f64) {
        use typst::layout::FrameItem;
        for &(pos, ref item) in frame.items() {
            let at = origin + pos;
            match item {
                FrameItem::Group(inner) => walk(&inner.frame, at, best),
                FrameItem::Text(_) | FrameItem::Shape(..) | FrameItem::Image(..) => {
                    *best = best.min(at.x.to_pt());
                }
                _ => {}
            }
        }
    }
    let mut best = f64::INFINITY;
    walk(page, Point::zero(), &mut best);
    if best.is_finite() {
        best
    } else {
        0.0
    }
}

fn extend_to_marker(page: &typst::layout::Frame, rect: &mut AnnotRect) {
    fn walk(
        frame: &typst::layout::Frame,
        origin: Point,
        rect: &AnnotRect,
        best: &mut Option<f64>,
    ) {
        use typst::layout::FrameItem;
        for &(pos, ref item) in frame.items() {
            let at = origin + pos;
            match item {
                FrameItem::Group(inner) => walk(&inner.frame, at, rect, best),
                FrameItem::Text(text) => {
                    let baseline = at.y.to_pt();
                    let x0 = at.x.to_pt();
                    let width: f64 = text
                        .glyphs
                        .iter()
                        .map(|g| g.x_advance.at(text.size).to_pt())
                        .sum();
                    let on_line = (baseline - rect.y1).abs() < 3.0;
                    let to_the_left = x0 < rect.x0 && x0 + width <= rect.x0 + 1.0;
                    let near = rect.x0 - x0 < 60.0;
                    if on_line && to_the_left && near {
                        *best = Some(best.map_or(x0, |b: f64| b.min(x0)));
                    }
                }
                _ => {}
            }
        }
    }
    let mut best = None;
    walk(page, Point::zero(), rect, &mut best);
    if let Some(x0) = best {
        rect.x0 = x0;
    }
}

/// The source range of the paragraph containing a position: markup between
/// blank lines.
fn paragraph_range(text: &str, at: usize) -> std::ops::Range<usize> {
    let start = text[..at].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    let end = text[at..]
        .find("\n\n")
        .map(|i| at + i)
        .unwrap_or_else(|| text.len());
    start..end
}

/// The source range of the sentence containing a position, clipped to its
/// paragraph. Sentence ends are `.`, `!` or `?` followed by whitespace —
/// a heuristic, but one an agent can re-derive from the same source.
fn sentence_range(text: &str, at: usize) -> std::ops::Range<usize> {
    let para = paragraph_range(text, at);
    let bytes = text.as_bytes();
    let is_end = |i: usize| {
        matches!(bytes[i], b'.' | b'!' | b'?')
            && bytes
                .get(i + 1)
                .is_none_or(|c| c.is_ascii_whitespace())
    };
    let mut start = para.start;
    for i in (para.start..at.min(para.end)).rev() {
        if is_end(i) {
            start = i + 1;
            break;
        }
    }
    while start < para.end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let mut end = para.end;
    for i in at.max(para.start)..para.end {
        if is_end(i) {
            end = i + 1;
            break;
        }
    }
    start..end.max(start)
}

/// The word ending at a position: back to the preceding whitespace.
fn word_range(text: &str, at: usize) -> std::ops::Range<usize> {
    let bytes = text.as_bytes();
    let mut start = at;
    while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    start..at
}

/// The rendered extent of a source range on a page: the union of the boxes
/// of every frame item whose span falls inside the range.
#[derive(Debug, Clone, Copy)]
pub struct BlockBox {
    /// The 1-based page number.
    pub page: usize,
    /// The left edge, in pt.
    pub x0: f64,
    /// The top edge, in pt.
    pub y0: f64,
    /// The bottom edge, in pt.
    pub y1: f64,
}

fn union_range_box(
    frame: &typst::layout::Frame,
    id: typst::syntax::FileId,
    range: &std::ops::Range<usize>,
    source: &typst::syntax::Source,
    origin: Point,
    out: &mut Option<(f64, f64, f64)>,
    // The enclosing group's box, if any: a hit inside a group (a figure, a
    // boxed diagram) marks that whole group rather than the single item,
    // since the rest of the group usually has no spans of its own.
    group: Option<(f64, f64, f64)>,
) {
    use typst::layout::FrameItem;
    fn hit(
        out: &mut Option<(f64, f64, f64)>,
        id: typst::syntax::FileId,
        range: &std::ops::Range<usize>,
        source: &typst::syntax::Source,
        pos: Point,
        height: Abs,
        item_span: typst::syntax::Span,
        group: Option<(f64, f64, f64)>,
    ) {
        if item_span.id() != Some(id) {
            return;
        }
        let Some(r) = typst_shim::syntax::source_range(source, item_span) else {
            return;
        };
        if r.start < range.start || r.end > range.end {
            return;
        }
        let (x, y0, y1) = match group {
            Some(g) => g,
            None => (pos.x.to_pt(), pos.y.to_pt() - height.to_pt(), pos.y.to_pt()),
        };
        *out = Some(match *out {
            None => (x, y0, y1),
            Some((ox, oy0, oy1)) => (ox.min(x), oy0.min(y0), oy1.max(y1)),
        });
    }
    for &(pos, ref item) in frame.items() {
        let at = origin + pos;
        match item {
            FrameItem::Group(inner) => {
                let size = inner.frame.size();
                let gb = Some((
                    at.x.to_pt(),
                    at.y.to_pt(),
                    at.y.to_pt() + size.y.to_pt(),
                ));
                union_range_box(&inner.frame, id, range, source, at, out, group.or(gb))
            }
            FrameItem::Text(text) => {
                for glyph in &text.glyphs {
                    hit(out, id, range, source, at, text.size, glyph.span.0, group);
                }
            }
            FrameItem::Shape(shape, span) => {
                let h = match shape.geometry {
                    typst::visualize::Geometry::Rect(size) => size.y,
                    _ => Abs::zero(),
                };
                hit(out, id, range, source, at + Point::new(Abs::zero(), h), h, *span, group);
            }
            FrameItem::Image(_, size, span) => hit(
                out, id, range, source,
                at + Point::new(Abs::zero(), size.y), size.y, *span, group,
            ),
            _ => {}
        }
    }
}

/// The rendered extent of a source range, searched across all pages.
fn block_box(
    paged: &reflexo_typst::TypstPagedDocument,
    id: typst::syntax::FileId,
    source: &typst::syntax::Source,
    range: &std::ops::Range<usize>,
) -> Option<BlockBox> {
    for (idx, page) in paged.pages().iter().enumerate() {
        let mut out = None;
        union_range_box(&page.frame, id, range, source, Point::zero(), &mut out, None);
        if let Some((x0, y0, y1)) = out {
            return Some(BlockBox {
                page: idx + 1,
                x0,
                y0,
                y1,
            });
        }
    }
    None
}

/// The first source span found anywhere in a frame.
fn first_span(frame: &typst::layout::Frame) -> Option<typst::syntax::Span> {
    use typst::layout::FrameItem;
    for (_, item) in frame.items() {
        let span = match item {
            FrameItem::Group(group) => first_span(&group.frame),
            FrameItem::Text(text) => text.glyphs.first().map(|g| g.span.0),
            FrameItem::Shape(_, span) | FrameItem::Image(_, _, span) => Some(*span),
            _ => None,
        };
        if let Some(span) = span {
            if span.id().is_some() {
                return Some(span);
            }
        }
    }
    None
}

/// Finds the text span closest to a click, with its distance in pt.
fn nearest_text_span(
    frame: &typst::layout::Frame,
    click: Point,
    origin: Point,
    best: &mut Option<(f64, typst::syntax::Span)>,
) {
    use typst::layout::FrameItem;
    for &(pos, ref item) in frame.items() {
        let at = origin + pos;
        match item {
            FrameItem::Group(group) => nearest_text_span(&group.frame, click, at, best),
            FrameItem::Text(text) => {
                for glyph in &text.glyphs {
                    if glyph.span.0.id().is_none() {
                        continue;
                    }
                    let dx = (at.x - click.x).to_pt();
                    let dy = (at.y - click.y).to_pt();
                    let d = (dx * dx + dy * dy).sqrt();
                    if best.map_or(true, |(bd, _)| d < bd) {
                        *best = Some((d, glyph.span.0));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Finds the innermost frame item containing a click that carries a source
/// span, for clicks that miss text (figures, shapes, images).
fn span_under_click(
    frame: &typst::layout::Frame,
    click: Point,
    origin: Point,
    best: &mut Option<(f64, typst::syntax::Span)>,
) {
    use typst::layout::FrameItem;
    for &(pos, ref item) in frame.items() {
        let at = origin + pos;
        match item {
            FrameItem::Group(group) => {
                span_under_click(&group.frame, click, at, best);
                // A group (a figure, a box) is itself a candidate: its span
                // stands in when nothing inside carries one.
                let size = group.frame.size();
                let inside = click.x >= at.x
                    && click.x <= at.x + size.x
                    && click.y >= at.y
                    && click.y <= at.y + size.y;
                let area = size.x.to_pt() * size.y.to_pt();
                if inside {
                    let span = first_span(&group.frame);
                    if let Some(span) = span {
                        if best.map_or(true, |(a, _)| area < a) {
                            *best = Some((area, span));
                        }
                    }
                }
            }
            FrameItem::Shape(shape, span) => {
                if let typst::visualize::Geometry::Rect(size) = shape.geometry {
                    let inside = click.x >= at.x
                        && click.x <= at.x + size.x
                        && click.y >= at.y
                        && click.y <= at.y + size.y;
                    let area = size.x.to_pt() * size.y.to_pt();
                    if inside && span.id().is_some() && best.map_or(true, |(a, _)| area < a) {
                        *best = Some((area, *span));
                    }
                }
            }
            FrameItem::Image(_, size, span) => {
                let inside = click.x >= at.x
                    && click.x <= at.x + size.x
                    && click.y >= at.y
                    && click.y <= at.y + size.y;
                let area = size.x.to_pt() * size.y.to_pt();
                if inside && span.id().is_some() && best.map_or(true, |(a, _)| area < a) {
                    *best = Some((area, *span));
                }
            }
            _ => {}
        }
    }
}

/// A word as rendered: its box and the source range behind it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutWord {
    /// The 1-based page number.
    pub page: usize,
    /// The left edge, in pt.
    pub x0: f64,
    /// The top edge, in pt.
    pub y0: f64,
    /// The right edge, in pt.
    pub x1: f64,
    /// The bottom edge, in pt.
    pub y1: f64,
    /// The start of the word in the source.
    pub s: usize,
    /// The end of the word in the source.
    pub e: usize,
}

/// A structural region of the document: what a click there would annotate
/// at block granularity.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutBlock {
    /// "heading", "item", "para" or "block".
    pub kind: String,
    /// The start of the region in the source.
    pub s: usize,
    /// The end of the region in the source.
    pub e: usize,
    /// The rendered boxes of the region, one per line.
    pub rects: Vec<AnnotRect>,
    /// The x, in pt, where this region's own marker belongs: left of a list
    /// item's bullet or number, or the text edge for anything else. The
    /// client cannot derive this — a bullet is not part of the region's
    /// boxes — so it is measured here.
    pub gutter_x: f64,
    /// The rail for the region's page: the leftmost ink on it, in pt.
    pub rail_x: f64,
}

/// The document's annotatable structure, sent to the frontend in bulk so it
/// can hit-test locally instead of probing the server on every mouse move.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutMap {
    /// Every structural region, innermost last. Word boxes are fetched per
    /// region on demand — they are the bulky part and are only needed once
    /// the cursor is actually inside a region.
    pub blocks: Vec<LayoutBlock>,
}

/// Collects every rendered word of the main file with its source range.
fn layout_words(
    paged: &reflexo_typst::TypstPagedDocument,
    id: typst::syntax::FileId,
    source: &typst::syntax::Source,
) -> Vec<LayoutWord> {
    fn walk(
        frame: &typst::layout::Frame,
        id: typst::syntax::FileId,
        source: &typst::syntax::Source,
        origin: Point,
        page: usize,
        out: &mut Vec<LayoutWord>,
    ) {
        use typst::layout::FrameItem;
        let text_of = source.text();
        for &(pos, ref item) in frame.items() {
            let at = origin + pos;
            match item {
                FrameItem::Group(inner) => walk(&inner.frame, id, source, at, page, out),
                FrameItem::Text(text) => {
                    let mut x = at.x;
                    let mut cur: Option<LayoutWord> = None;
                    for glyph in &text.glyphs {
                        let advance = glyph.x_advance.at(text.size);
                        let src = (glyph.span.0.id() == Some(id))
                            .then(|| typst_shim::syntax::source_range(source, glyph.span.0))
                            .flatten()
                            .map(|r| r.start + glyph.span.1 as usize);
                        let is_space = src
                            .and_then(|s| text_of.get(s..).and_then(|t| t.chars().next()))
                            .is_none_or(|c| c.is_whitespace());
                        match (src, is_space) {
                            (Some(s), false) => {
                                let e = s + glyph.range().len().max(1);
                                let (x0, y0, x1, y1) = (
                                    x.to_pt(),
                                    at.y.to_pt() - text.size.to_pt() * 0.78,
                                    (x + advance).to_pt(),
                                    at.y.to_pt() + text.size.to_pt() * 0.22,
                                );
                                match cur.as_mut() {
                                    // Same word: contiguous in the source.
                                    Some(w) if w.e >= s && (w.y1 - y1).abs() < 1.5 => {
                                        w.x1 = x1.max(w.x1);
                                        w.e = e.max(w.e);
                                    }
                                    _ => {
                                        if let Some(w) = cur.take() {
                                            out.push(w);
                                        }
                                        cur = Some(LayoutWord {
                                            page,
                                            x0,
                                            y0,
                                            x1,
                                            y1,
                                            s,
                                            e,
                                        });
                                    }
                                }
                            }
                            _ => {
                                if let Some(w) = cur.take() {
                                    out.push(w);
                                }
                            }
                        }
                        x += advance;
                    }
                    if let Some(w) = cur.take() {
                        out.push(w);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = vec![];
    for (idx, page) in paged.pages().iter().enumerate() {
        walk(&page.frame, id, source, Point::zero(), idx + 1, &mut out);
    }
    out
}

/// Groups words into per-line boxes for a source range.
fn rects_from_words(words: &[LayoutWord], range: &std::ops::Range<usize>) -> Vec<AnnotRect> {
    let mut rows: Vec<AnnotRect> = vec![];
    for w in words.iter().filter(|w| w.s >= range.start && w.e <= range.end) {
        match rows.last_mut() {
            Some(row) if row.page == w.page && (row.y1 - w.y1).abs() < 1.5 => {
                row.x0 = row.x0.min(w.x0);
                row.y0 = row.y0.min(w.y0);
                row.x1 = row.x1.max(w.x1);
                row.y1 = row.y1.max(w.y1);
            }
            _ => rows.push(AnnotRect {
                page: w.page,
                x0: w.x0,
                y0: w.y0,
                x1: w.x1,
                y1: w.y1,
            }),
        }
    }
    rows
}

/// Builds the document's structural map: every word, and every heading,
/// list item and paragraph with its rendered boxes.
pub fn layout_map(art: &LspCompiledArtifact) -> LayoutMap {
    let world = art.world();
    let Some(doc) = art.success_doc() else {
        return LayoutMap { blocks: vec![] };
    };
    let TypstDocument::Paged(paged) = &doc else {
        return LayoutMap { blocks: vec![] };
    };
    let id = world.main();
    let Ok(source) = world.source(id) else {
        return LayoutMap { blocks: vec![] };
    };
    let words = layout_words(paged, id, &source);

    let mut blocks = vec![];
    // Headings and list items come from the syntax tree.
    fn collect(
        node: &typst::syntax::LinkedNode,
        words: &[LayoutWord],
        out: &mut Vec<LayoutBlock>,
    ) {
        let kind = match node.kind() {
            SyntaxKind::Heading => Some("heading"),
            SyntaxKind::ListItem | SyntaxKind::EnumItem | SyntaxKind::TermItem => Some("item"),
            _ => None,
        };
        if let Some(kind) = kind {
            let range = node.range();
            let rects = rects_from_words(words, &range);
            if !rects.is_empty() {
                out.push(LayoutBlock {
                    kind: kind.into(),
                    s: range.start,
                    e: range.end,
                    gutter_x: rects[0].x0,
                    rail_x: 0.0,
                    rects,
                });
            }
        }
        for child in node.children() {
            collect(&child, words, out);
        }
    }
    collect(&typst::syntax::LinkedNode::new(source.root()), &words, &mut blocks);

    // Paragraphs are the markup between blank lines.
    let text = source.text();
    let mut at = 0;
    while at < text.len() {
        let range = paragraph_range(text, at);
        if range.end > range.start {
            let rects = rects_from_words(&words, &range);
            if !rects.is_empty() {
                blocks.push(LayoutBlock {
                    kind: "para".into(),
                    s: range.start,
                    e: range.end,
                    gutter_x: rects[0].x0,
                    rail_x: 0.0,
                    rects,
                });
            }
        }
        at = (range.end + 2).max(at + 1);
    }

    // The marker's true left edge, and the page rail, measured off the frames.
    let rails: Vec<f64> = paged.pages().iter().map(|p| page_rail(&p.frame)).collect();
    for block in &mut blocks {
        let Some(first) = block.rects.first().copied() else {
            continue;
        };
        block.rail_x = rails.get(first.page - 1).copied().unwrap_or(0.0);
        block.gutter_x = first.x0;
        if block.kind == "item" {
            if let Some(page) = paged.pages().get(first.page - 1) {
                let mut probe = first;
                extend_to_marker(&page.frame, &mut probe);
                block.gutter_x = probe.x0;
            }
        }
    }
    // A region reaches as far left as anything it contains: a paragraph made
    // of bullets owns their markers, so its own mark clears them too.
    let own: Vec<(usize, usize, f64)> = blocks
        .iter()
        .map(|b| (b.s, b.e, b.gutter_x))
        .collect();
    for block in &mut blocks {
        for &(s, e, gutter) in &own {
            if s >= block.s && e <= block.e && gutter < block.gutter_x {
                block.gutter_x = gutter;
            }
        }
    }
    LayoutMap { blocks }
}

/// The rendered words of one source range, for local hit-testing while the
/// cursor is inside that region.
pub fn words_in_range(art: &LspCompiledArtifact, range: std::ops::Range<usize>) -> Vec<LayoutWord> {
    let world = art.world();
    let Some(doc) = art.success_doc() else {
        return vec![];
    };
    let TypstDocument::Paged(paged) = &doc else {
        return vec![];
    };
    let id = world.main();
    let Ok(source) = world.source(id) else {
        return vec![];
    };
    layout_words(paged, id, &source)
        .into_iter()
        .filter(|w| w.s >= range.start && w.e <= range.end)
        .collect()
}

/// Resolves a click to the source position where an anchor label would be
/// inserted: the end of the clicked word. Errors when the click does not
/// hit markup text (margins, whitespace past the end of the document, math,
/// generated content).
#[allow(clippy::type_complexity)]
fn resolve_click(
    art: &LspCompiledArtifact,
    page_no: usize,
    x: f64,
    y: f64,
) -> Result<
    (
        typst::syntax::FileId,
        typst::syntax::Source,
        usize,
        Option<std::ops::Range<usize>>,
    ),
    String,
> {
    let world = art.world();
    let Some(TypstDocument::Paged(doc)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    let page = doc
        .pages()
        .get(page_no.checked_sub(1).ok_or("bad page")?)
        .ok_or("no such page")?;
    let click = Point::new(Abs::pt(x), Abs::pt(y));
    // Text first; otherwise the innermost shape or image under the click
    // (figures, diagrams), whose span points at the code that made it.
    // `direct` means the click landed on real text; otherwise it was
    // resolved via a shape, a group, or the nearest caption — in which case
    // the annotation belongs to the whole block, not to that text.
    let mut direct = true;
    // The offset within the span matters: it is what distinguishes the
    // clicked word from the first word of its text run.
    let mut offset = 0;
    let span = match jump_from_click(world, &page.frame, click) {
        Some((start, _)) => {
            offset = start.offset;
            start.span
        }
        None => {
            direct = false;
            let mut best = None;
            span_under_click(&page.frame, click, Point::zero(), &mut best);
            match best {
                Some((_, span)) => span,
                None => {
                    // Figures and diagrams often carry no span on their
                    // shapes; fall back to the nearest text nearby (a
                    // caption, a label inside the drawing), whose enclosing
                    // expression is the block the click meant.
                    let mut near: Option<(f64, typst::syntax::Span)> = None;
                    nearest_text_span(&page.frame, click, Point::zero(), &mut near);
                    match near {
                        Some((d, span)) if d < 120.0 => span,
                        _ => return Err("nothing annotatable under the click".into()),
                    }
                }
            }
        }
    };
    let start = reflexo::debug_loc::SourceSpanOffset { span, offset };
    let id = start.span.id().ok_or("clicked text has no source")?;
    let source = world.source(id).map_err(|e| e.to_string())?;
    let node = source.find(start.span).ok_or("span not found in source")?;
    let at = if direct && node.kind() == SyntaxKind::Text {
        let node_range = node.range();
        // Advance to the end of the clicked word: a label sticks to the
        // element before it, and mid-word insertion would split the word.
        let mut at = (node_range.start + start.offset).min(node_range.end);
        let text = source.text();
        while at < node_range.end {
            match text[at..].chars().next() {
                Some(c) if !c.is_whitespace() => at += c.len_utf8(),
                _ => break,
            }
        }
        return Ok((id, source, at, None));
    } else {
        // Generated content (`#lorem(60)`, math, figures, ...) has no
        // markup text to carry a label; anchor after the whole expression
        // that produced it, which is itself in markup.
        let mut cursor = node;
        loop {
            let parent = cursor.parent().cloned();
            match parent {
                Some(parent) if parent.kind() != SyntaxKind::Markup => cursor = parent,
                _ => break,
            }
        }
        if cursor.parent().is_none() {
            return Err("annotations can only anchor in markup".into());
        }
        let range = cursor.range();
        return Ok((id, source, range.end, Some(range)));
    };
    #[allow(unreachable_code)]
    Ok((id, source, at, None))
}

/// Resolves a click to the exact would-be anchor position, without editing
/// anything.
/// The word-level source range a drag between two clicks covers.
fn span_range(
    art: &LspCompiledArtifact,
    a: (usize, f64, f64),
    b: (usize, f64, f64),
) -> Result<(typst::syntax::FileId, typst::syntax::Source, std::ops::Range<usize>), String> {
    let (id_a, source, at_a, _) = resolve_click(art, a.0, a.1, a.2)?;
    let (id_b, _, at_b, _) = resolve_click(art, b.0, b.1, b.2)?;
    if id_a != id_b {
        return Err("a span cannot cross files".into());
    }
    let text = source.text();
    let (lo, hi) = if at_a <= at_b { (at_a, at_b) } else { (at_b, at_a) };
    // Word-level: from the start of the first word to the end of the last.
    let start = word_range(text, lo).start;
    Ok((id_a, source, start..hi))
}

pub fn probe_annotate(
    art: &LspCompiledArtifact,
    page_no: usize,
    x: f64,
    y: f64,
) -> Result<ProbeResult, String> {
    let (id, source, at, block) = resolve_click(art, page_no, x, y)?;
    let Some(TypstDocument::Paged(paged)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    // Clicking generated content annotates that whole block; clicking text
    // annotates the word, which is what the marker previews.
    let (scope, range) = match block {
        Some(range) => (Scope::Block, range),
        None => (Scope::Word, word_range(source.text(), at)),
    };
    let rects = range_rects(&paged, id, &source, &range, scope == Scope::Block);
    let (page, x, y) = match rects.first() {
        Some(first) => {
            let last = rects.last().unwrap_or(first);
            match scope {
                Scope::Block => (first.page, first.x0, (first.y0 + last.y1) / 2.0),
                _ => (last.page, (last.x0 + last.x1) / 2.0, last.y1),
            }
        }
        None => match exact_anchor_position(&paged, &source, at) {
            Some(pos) => (pos.page.into(), pos.point.x.to_pt(), pos.point.y.to_pt()),
            None => (page_no, x, y),
        },
    };
    Ok(ProbeResult {
        scope: scope.as_str().into(),
        rects,
        page,
        x,
        y,
    })
}

/// Resolves a drag to the span it would create.
pub fn probe_span(
    art: &LspCompiledArtifact,
    a: (usize, f64, f64),
    b: (usize, f64, f64),
) -> Result<ProbeResult, String> {
    let (id, source, range) = span_range(art, a, b)?;
    let Some(TypstDocument::Paged(paged)) = art.success_doc() else {
        return Err("no rendered document".into());
    };
    let rects = range_rects(&paged, id, &source, &range, false);
    let (page, x, y) = match (rects.first(), rects.last()) {
        (Some(_), Some(last)) => (last.page, (last.x0 + last.x1) / 2.0, last.y1),
        _ => (a.0, a.1, a.2),
    };
    Ok(ProbeResult {
        scope: Scope::Span.as_str().into(),
        rects,
        page,
        x,
        y,
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
    // A client-side drag sends the source range it highlighted.
    if let (Some(s), Some(e)) = (req.s, req.e) {
        return prepare_span_range(art, req, s..e, encoding);
    }
    // A bare offset anchors at a point the client picked (between words).
    if let Some(at) = req.s {
        let scope = req
            .scope
            .as_deref()
            .and_then(Scope::from_suffix)
            .unwrap_or(Scope::Point);
        return prepare_at(art, req, at, scope, encoding);
    }
    let (page, x, y) = match (req.page, req.x, req.y) {
        (Some(page), Some(x), Some(y)) => (page, x, y),
        _ => return Err("a click position or a source range is required".into()),
    };
    // A drag creates a span: two anchors bracketing the dragged words.
    if let (Some(page2), Some(x2), Some(y2)) = (req.page2, req.x2, req.y2) {
        return prepare_span(art, req, (page, x, y), (page2, x2, y2), encoding);
    }
    let (id, source, at, block) = resolve_click(art, page, x, y)?;
    let scope = if block.is_some() { Scope::Block } else { Scope::Word };

    prepare_at(art, req, at, scope, encoding)
}

/// A label placed where content has not started yet — at the head of a line,
/// or before a list marker — attaches to whatever came *before* it: the
/// previous item, or nothing at all, and a marker pushed off the line start
/// stops being a marker. Region scopes (which anchor at the start of the
/// region they cover) therefore snap forward past the marker to the end of
/// the region's first word, where the label binds to the text it belongs to.
fn snap_past_marker(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // Already inside the text? Leave the anchor where the caller put it.
    if text[line_start..at].chars().any(|c| !c.is_whitespace()) {
        return at;
    }
    let mut i = at;
    let space = |b: u8| b == b' ' || b == b'\t';
    while i < bytes.len() && space(bytes[i]) {
        i += 1;
    }
    // A list marker: "-", "+", "/", a heading's run of "=", or an enumerator
    // like "12." / "12)".
    let before_marker = i;
    let mut heading = false;
    if i < bytes.len() {
        match bytes[i] {
            b'-' | b'+' | b'/' => i += 1,
            b'=' => {
                while i < bytes.len() && bytes[i] == b'=' {
                    i += 1;
                }
                heading = true;
            }
            b'0'..=b'9' => {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'.' || bytes[j] == b')') {
                    i = j + 1;
                }
            }
            _ => {}
        }
    }
    // Only a marker if a space follows it; otherwise it was ordinary text.
    if i > before_marker && !(i < bytes.len() && space(bytes[i])) {
        i = before_marker;
        heading = false;
    }
    // A heading holds a label only at its end — one written into the middle
    // closes the heading there and leaves the rest as a paragraph — so its
    // anchor goes after the last word of the line.
    if heading {
        let mut end = text[i..].find('\n').map(|at| i + at).unwrap_or(text.len());
        while end > i && space(bytes[end - 1]) {
            end -= 1;
        }
        return end;
    }
    while i < bytes.len() && space(bytes[i]) {
        i += 1;
    }
    // Past the first word of the body, so the label has content to bind to.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    if i >= text.len() {
        at
    } else {
        i
    }
}

/// Creates an annotation whose anchor goes at a known source offset.
fn prepare_at(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    at: usize,
    scope: Scope,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    let world = art.world();
    let id = world.main();
    let source = world.source(id).map_err(|e| e.to_string())?;
    let path = world.path_for_id(id).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    // Region scopes anchor at the region's start, which may sit before a list
    // marker or at a line head; move into the text so the label binds there.
    let at = match scope {
        // Every region scope anchors at the head of the region it covers,
        // which is where its marker lives — a bullet, an enumerator, the
        // "=" of a heading. A label written there stops the marker being one.
        Scope::Item | Scope::Para | Scope::Block => snap_past_marker(source.text(), at),
        _ => at,
    };
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

    let new_text = anchor_text(&rec.uuid, scope);
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

/// Prepares the edits creating a span annotation from a drag: a
/// `<uuid.span.begin>` before the first word and a `<uuid.span.end>` after
/// the last, plus the sidecar entry.
fn prepare_span(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    a: (usize, f64, f64),
    b: (usize, f64, f64),
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    let (_, _, range) = span_range(art, a, b)?;
    prepare_span_range(art, req, range, encoding)
}

/// Creates a span annotation over a known source range.
fn prepare_span_range(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    range: std::ops::Range<usize>,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    let world = art.world();
    let id = world.main();
    let source = world.source(id).map_err(|e| e.to_string())?;
    if range.is_empty() {
        return Err("empty span".into());
    }
    let path = world.path_for_id(id).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;

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

    // Insert the end anchor first: inserting the begin anchor would shift
    // every later offset.
    let begin = format!("<{}.span.begin>", rec.uuid);
    let end = format!("<{}.span.end>", rec.uuid);
    let mut new_source = source.text().to_owned();
    new_source.insert_str(range.end, &end);
    new_source.insert_str(range.start, &begin);
    let disk_content = match std::fs::read_to_string(&path) {
        Ok(disk) if disk == source.text() => Some(new_source.clone()),
        _ => None,
    };
    let pos = to_lsp_position(range.start, encoding, &source);
    Ok(AnnotationEdit {
        uuid: rec.uuid,
        uri,
        disk_content,
        // The whole file is rewritten on disk; the editor gets the begin
        // anchor as an insertion and the end anchor follows on the next
        // compile from disk.
        buffer_edit: false,
        path: path.to_path_buf(),
        range: lsp_types::Range::new(as_lsp(pos), as_lsp(pos)),
        new_text: begin,
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
    let needle_prefix = format!("<{uuid}.");
    let hit = art.depended_files().iter().find_map(|&file| {
        let source = world.source(file).ok()?;
        let anchors = find_anchors(source.text(), uuid);
        (!anchors.is_empty()).then(|| (file, anchors))
    });
    let (file, anchors) = hit.ok_or_else(|| {
        format!("no anchor {needle_prefix}..> found in any source file")
    })?;
    let (at, _, needle) = anchors.first().cloned().unwrap();
    let source = world.source(file).map_err(|e| e.to_string())?;
    let path = world.path_for_id(file).map_err(|e| e.to_string())?;
    let path = path.to_err().map_err(|e| e.to_string())?;
    let uri = Url::from_file_path(&path).map_err(|_| "bad file path".to_string())?;
    let start = to_lsp_position(at, encoding, &source);
    let end = to_lsp_position(at + needle.len(), encoding, &source);

    // Remove every anchor of this annotation (a span has two), last first
    // so earlier offsets stay valid.
    let mut stripped = source.text().to_owned();
    for (off, _, label) in anchors.iter().rev() {
        stripped.replace_range(*off..*off + label.len(), "");
    }
    let (mut disk_content, buffer_edit) =
        disk_edit(&path, source.text(), at..at + needle.len(), "");
    if anchors.len() > 1 {
        // The single-range disk patch cannot express two removals.
        disk_content = match std::fs::read_to_string(&path) {
            Ok(disk) if disk == source.text() => Some(stripped),
            _ => None,
        };
    }
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

#[cfg(test)]
mod anchor_tests {
    use super::snap_past_marker;

    fn snapped(text: &str, at: usize) -> String {
        let i = snap_past_marker(text, at);
        format!("{}<L>{}", &text[..i], &text[i..])
    }

    #[test]
    fn snaps_past_list_markers() {
        // bullet: the label lands after the first word of the body
        let t = "- Agents watch the sidecar\n";
        assert_eq!(snapped(t, 0), "- Agents<L> watch the sidecar\n");
        // indented bullet
        let t = "text\n  - A nested bullet here\n";
        assert_eq!(snapped(t, 5), "text\n  - A<L> nested bullet here\n");
        // enumerator
        let t = "+ A numbered item, first\n";
        assert_eq!(snapped(t, 0), "+ A<L> numbered item, first\n");
        let t = "12. A numbered item\n";
        assert_eq!(snapped(t, 0), "12. A<L> numbered item\n");
        // plain paragraph start
        let t = "This document demonstrates\n";
        assert_eq!(snapped(t, 0), "This<L> document demonstrates\n");
        // a hyphen that is not a marker stays put
        let t = "-notamarker word\n";
        assert_eq!(snapped(t, 0), "-notamarker<L> word\n");
        // an offset already inside the text is left alone
        let t = "- Agents watch\n";
        assert_eq!(snapped(t, 9), "- Agents <L>watch\n");
        // A heading takes its label at the end of the line, where Typst binds
        // it to the heading instead of ending it.
        let t = "= A short document\n";
        assert_eq!(snapped(t, 0), "= A short document<L>\n");
        let t = "== A second heading   \n";
        assert_eq!(snapped(t, 0), "== A second heading<L>   \n");
        // "=" without a space after it is an equation or plain text, not a
        // heading, and nothing is skipped
        let t = "=x is not a heading\n";
        assert_eq!(snapped(t, 0), "=x<L> is not a heading\n");
    }
}
