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
use std::sync::Arc;

use lsp_types::Url;
use reflexo::debug_loc::LspPosition;
use serde::{Deserialize, Serialize};
use tinymist_project::LspCompiledArtifact;
use tinymist_query::{to_lsp_position, PositionEncoding};
use typst::syntax::SyntaxKind;
use typst::World;

use crate::tool::render::html::doc_file;

pub use crate::tool::asset::dev_asset_in;

/// An annotator asset, from `src/static/annos`.
pub fn dev_asset(rel: &str, embedded: &'static str) -> String {
    dev_asset_in("annos", rel, embedded)
}

/// The schema documentation stamped at the top of new sidecar files.
pub fn annos_prelude() -> String {
    dev_asset("prelude.typ", include_str!("../../static/annos/prelude.typ"))
}

/// The sidecar entry template, with `${...}` placeholders.
fn entry_template() -> String {
    dev_asset("entry.tmpl", include_str!("../../static/annos/entry.tmpl"))
}

/// The discussion reply template, with `${...}` placeholders.
fn reply_template() -> String {
    dev_asset("reply.tmpl", include_str!("../../static/annos/reply.tmpl"))
}

/// The capture entry template, with `${...}` placeholders.
fn capture_template() -> String {
    dev_asset("capture.tmpl", include_str!("../../static/annos/capture.tmpl"))
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

/// One rendering of what a graphical annotation points at.
///
/// A picture only exists once the document has been laid out, so an annotation
/// on one keeps the renderings it has seen: the drawing as SVG, stored under its
/// hash in the server's private cache, and the time it first looked like that. A
/// new capture is recorded only when the hash moves — a compile that changed
/// nothing about the drawing is not a new picture of it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationCapture {
    /// When this rendering first appeared, ISO 8601 UTC.
    pub time: String,
    /// What the stored capture is: `svg` for a drawing taken out of the
    /// rendered page, and later `png` for a photograph or a raster image,
    /// `html` for a piece of the page kept as itself. The file is stored under
    /// `<hash>.<fmt>`, so the format is also how to read it back.
    #[serde(default)]
    pub fmt: String,
    /// The hash the capture is stored under.
    pub hash: String,
    /// How wide the capture is on the page, in CSS pixels, so that a reader of
    /// the sidecar knows the shape of the picture without fetching it.
    #[serde(default)]
    pub width: u32,
    /// How tall it is, in CSS pixels.
    #[serde(default)]
    pub height: u32,
    /// What the reader drew on top, as inline SVG in the capture's own
    /// coordinates. Absent until somebody draws something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markup: Option<String>,
}

/// A stored annotation record from the sidecar file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationRecord {
    /// The annotation kind: "comment" | "question" | "request".
    #[serde(rename = "type")]
    pub rtype: String,
    /// The unique id, e.g. "7C42"; the document anchor is `<anno.7C42.word>`.
    pub uuid: String,
    /// What the annotation refers to: the same word as the anchor's suffix,
    /// written out so the sidecar reads on its own, without the document.
    #[serde(default)]
    pub scope: String,
    /// The colour the client chose for it, as `#rrggbb`. Stored, not decided:
    /// the palette lives in the client, and an annotation keeps the colour it
    /// was given however the palette changes around it.
    #[serde(default)]
    pub color: String,
    /// The display letter shown on the pin: "a".."z", then "aa", ...
    pub letter: String,
    /// The author of the annotation.
    pub author: String,
    /// The message.
    pub content: String,
    /// Creation time, ISO 8601 UTC.
    pub time: String,
    /// Whether somebody is working on it. Set when an agent claims it, unset
    /// when it is given back.
    #[serde(default)]
    pub claimed: bool,
    /// Whether it is done. A resolved annotation is still drawn, dimmed, until
    /// somebody deletes it or reopens it.
    #[serde(default)]
    pub resolved: bool,
    /// When it was last changed — a reply, a flag, a capture. Creation time is
    /// `time`; this moves whenever anything about the entry does, which is what
    /// a reader needs to show what is new.
    #[serde(default)]
    pub mtime: String,
    /// What the thing it points at has looked like, oldest first. Only
    /// graphical annotations have any.
    #[serde(default)]
    pub captures: Vec<AnnotationCapture>,
    /// The discussion thread, in order.
    pub discussion: Vec<AnnotationReply>,
}

impl AnnotationRecord {
    /// The one-word state, for the reader: a resolved annotation is resolved
    /// whoever holds it, a claimed one is being worked on, and the rest are
    /// waiting. Derived rather than stored — two flags say more than a word can
    /// (an agent can hold something it has already answered), and the word is
    /// only ever what a page paints.
    pub fn status(&self) -> &'static str {
        match (self.resolved, self.claimed) {
            (true, _) => "resolved",
            (false, true) => "ongoing",
            (false, false) => "created",
        }
    }

    /// When it last changed, falling back to when it was made: an entry written
    /// before the field existed has only ever been created.
    pub fn changed(&self) -> &str {
        if self.mtime.is_empty() {
            &self.time
        } else {
            &self.mtime
        }
    }
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
    /// The colour the client drew it in, as `#rrggbb`, stored as given: the
    /// palette is the client's business, and the annotation keeps the colour
    /// it was made with.
    #[serde(default)]
    pub color: Option<String>,
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
    /// Who to record as the author. Resolved by the server from the request
    /// itself, never deserialized from the body: a client that could name its
    /// own author could sign a colleague's name to a comment.
    #[serde(skip)]
    pub author: Option<String>,
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
    /// Appends a reply to an annotation's discussion. `author` is the identity
    /// the server resolved for the request, if it found one.
    fn reply(&self, uuid: &str, text: &str, author: Option<&str>) -> Result<(), String>;
    /// Sets an annotation's flags — claimed, resolved, or both. Whichever is
    /// left out is left alone.
    fn set_flags(&self, uuid: &str, claimed: Option<bool>, resolved: Option<bool>)
        -> Result<(), String>;
    /// The block of source an annotation is anchored in — what an agent reads
    /// before rewriting it.
    fn block(&self, _uuid: &str, _context: bool) -> Result<SourceBlock, String> {
        Err("this server cannot read blocks".into())
    }

    /// Rewrites that block, returning the annotations whose anchors the
    /// rewrite deliberately dropped.
    fn replace_block(
        &self,
        _uuid: &str,
        _block_id: &str,
        _new_text: &str,
        _policy: AnchorPolicy,
    ) -> Result<Vec<String>, String> {
        Err("this server cannot rewrite blocks".into())
    }

    /// Which compile answered last: a caller that has just written a file
    /// waits for this to move before asking what the document says about it.
    fn compile_revision(&self) -> u64 {
        0
    }

    /// Whether the last compile succeeded, and what it said if not.
    fn diagnostics(&self) -> (bool, Vec<String>) {
        (true, vec![])
    }

    /// Every annotation of this document, as the sidecar holds them.
    fn records(&self) -> Result<Vec<AnnotationRecord>, String> {
        Err("this server cannot list annotations".into())
    }

    /// Every annotation with the source offsets of its anchors, which is what
    /// a page needs to draw them: the renderer labelled each element with the
    /// range it came from, and the client matches the two up.
    fn pins(&self) -> Vec<super::pins::HtmlPin> {
        vec![]
    }
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
    /// An inline equation, annotated whole: `$x + y$`. Typst calls these
    /// inline equations, as against block ones.
    Math,
    /// A block equation — `$ x + y $` on its own — which is a region of the
    /// document rather than a phrase in one, and is marked like a block.
    DisplayMath,
    /// A link, annotated whole: its text is one destination, not a run of
    /// words that happen to be underlined.
    Link,
    /// A fragment of raw text — `` `code` `` — annotated whole, for the same
    /// reason: it is one name, not a phrase.
    Raw,
    /// Anything else inline that a label cannot go inside: text a call
    /// produced from its arguments, where the annotation is about the call.
    Inline,
    /// A drawing — a cetz canvas, a fletcher diagram — which reaches HTML as
    /// one picture and is annotated as one.
    Svg,
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
            "math" => Scope::Math,
            "math.block" => Scope::DisplayMath,
            "link" => Scope::Link,
            "raw" => Scope::Raw,
            "inline" => Scope::Inline,
            "svg" => Scope::Svg,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Point => "point",
            Scope::Word => "word",
            Scope::Sentence => "sentence",
            Scope::Span => "span",
            Scope::Block => "block",
            Scope::Item => "item",
            Scope::Para => "para",
            Scope::Math => "math",
            Scope::DisplayMath => "math.block",
            Scope::Link => "link",
            Scope::Raw => "raw",
            Scope::Inline => "inline",
            Scope::Svg => "svg",
        }
    }
}

/// What every anchor and every sidecar entry is named after, so that one
/// plain search — no regular expression — finds every annotation in a
/// document: `<anno.`. A leading `.` would have been shorter, but Typst does
/// not accept it in a label: `<.7C42.word>` is not an anchor, it is text, and
/// it renders as text.
pub const ANCHOR_PREFIX: &str = "anno.";

/// The document anchor text for a uuid, e.g. `<anno.7C42.word>`.
fn anchor_text(uuid: &str, scope: Scope) -> String {
    match scope {
        Scope::Span => format!("<{ANCHOR_PREFIX}{uuid}.span.begin>"),
        scope => format!("<{ANCHOR_PREFIX}{uuid}.{}>", scope.as_str()),
    }
}

/// Finds every anchor of an annotation in a source: their byte offsets and
/// scopes, in document order.
pub fn find_anchors(text: &str, uuid: &str) -> Vec<(usize, Scope, String)> {
    // Both spellings are read; only the prefixed one is written. Documents
    // annotated before the prefix existed keep working, and keep their marks.
    let prefixes = [format!("<{ANCHOR_PREFIX}{uuid}."), format!("<{uuid}.")];
    let mut out = vec![];
    for prefix in prefixes {
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
        if !out.is_empty() {
            break;
        }
    }
    out.sort_by_key(|(at, _, _)| *at);
    out
}

/// The sidecar path for the current main file, e.g. `typing.annos.typ`
/// next to `typing.typ`.
pub fn sidecar_path(art: &LspCompiledArtifact) -> Option<PathBuf> {
    let world = art.world();
    let main = doc_file(world);
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

fn bool_of(dict: &typst::foundations::Dict, key: &str) -> bool {
    dict.get(key)
        .ok()
        .and_then(|value| value.clone().cast::<bool>().ok())
        .unwrap_or(false)
}

fn int_of(dict: &typst::foundations::Dict, key: &str) -> u32 {
    dict.get(key)
        .ok()
        .and_then(|value| value.clone().cast::<i64>().ok())
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
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
    let captures = dict
        .get("captures")
        .ok()
        .and_then(|value| value.clone().cast::<typst::foundations::Array>().ok())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| {
                    let capture = value.clone().cast::<typst::foundations::Dict>().ok()?;
                    Some(AnnotationCapture {
                        time: string_of(&capture, "time").unwrap_or_default(),
                        // Sidecars written before the field held SVG and only
                        // SVG, so that is what an unlabelled capture is.
                        fmt: string_of(&capture, "fmt").unwrap_or_else(|| "svg".into()),
                        hash: string_of(&capture, "hash")?,
                        width: int_of(&capture, "width"),
                        height: int_of(&capture, "height"),
                        markup: string_of(&capture, "markup"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let claimed = bool_of(&dict, "claimed");
    let resolved = bool_of(&dict, "resolved");
    Some(AnnotationRecord {
        rtype: string_of(&dict, "type").unwrap_or_else(|| "comment".into()),
        uuid: string_of(&dict, "uuid")?,
        scope: string_of(&dict, "scope").unwrap_or_default(),
        color: string_of(&dict, "color").unwrap_or_default(),
        letter: string_of(&dict, "letter").unwrap_or_default(),
        author: string_of(&dict, "author").unwrap_or_else(|| "unknown".into()),
        content: string_of(&dict, "content")?,
        time: string_of(&dict, "time").unwrap_or_default(),
        claimed,
        resolved,
        mtime: string_of(&dict, "mtime").unwrap_or_default(),
        captures,
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
    // The entry carries the same name as the anchors it belongs to, so one
    // search for `<anno.7C42` finds the annotation and everything it is
    // attached to. Sidecars written before that keep their `<note-7C42>`.
    let note = format!("<{ANCHOR_PREFIX}{uuid}>");
    let (note, note_at) = match find_in_code(content, &note, 0) {
        Some(at) => (note, at),
        None => {
            let legacy = format!("<note-{uuid}>");
            let at = find_in_code(content, &legacy, 0)?;
            (legacy, at)
        }
    };
    let start = rfind_in_code(content, "#metadata((", note_at)?;
    let mut end = note_at + note.len();
    if content[end..].starts_with('\n') {
        end += 1;
    }
    Some(start..end)
}

/// Whether an offset is inside a line comment. The prelude at the top of every
/// sidecar shows what an entry looks like — a whole `#metadata` block, uuid and
/// all — and an entry that happened to be named after the one in the example
/// would otherwise be "found" there, and the explanation rewritten in its
/// place. Only line comments matter: the prelude is written in them, and a
/// sidecar is a list of entries rather than a program with block comments in
/// it.
fn commented(content: &str, at: usize) -> bool {
    let line_start = content[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    content[line_start..at].contains("//")
}

/// The first occurrence of `needle` at or after `from` that is not commented
/// out.
fn find_in_code(content: &str, needle: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(rel) = content[at..].find(needle) {
        let hit = at + rel;
        if !commented(content, hit) {
            return Some(hit);
        }
        at = hit + needle.len();
    }
    None
}

/// The last occurrence of `needle` before `before` that is not commented out.
fn rfind_in_code(content: &str, needle: &str, before: usize) -> Option<usize> {
    let mut end = before;
    while let Some(hit) = content[..end].rfind(needle) {
        if !commented(content, hit) {
            return Some(hit);
        }
        end = hit;
    }
    None
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
    let captures = if rec.captures.is_empty() {
        "()".to_owned()
    } else {
        let capture_tmpl = capture_template();
        let entries: String = rec
            .captures
            .iter()
            .map(|capture| {
                // A capture with nothing drawn on it says nothing about markup,
                // rather than saying it is empty.
                let markup = match &capture.markup {
                    Some(markup) => format!("\n      markup: \"{}\",", escape(markup)),
                    None => String::new(),
                };
                capture_tmpl
                    .replace("${time}", &escape(&capture.time))
                    .replace("${fmt}", &escape(&capture.fmt))
                    .replace("${hash}", &escape(&capture.hash))
                    .replace("${width}", &capture.width.to_string())
                    .replace("${height}", &capture.height.to_string())
                    .replace("${markup}", &markup)
            })
            .collect();
        format!("(\n{entries}  )")
    };
    entry_template()
        .replace("${captures}", &captures)
        .replace("${type}", &escape(&rec.rtype))
        .replace("${uuid}", &escape(&rec.uuid))
        .replace("${scope}", &escape(&rec.scope))
        .replace("${color}", &escape(&rec.color))
        .replace("${letter}", &escape(&rec.letter))
        .replace("${author}", &escape(&rec.author))
        .replace("${content}", &escape(&rec.content))
        .replace("${time}", &escape(&rec.time))
        .replace("${claimed}", if rec.claimed { "true" } else { "false" })
        .replace("${resolved}", if rec.resolved { "true" } else { "false" })
        .replace("${mtime}", &escape(rec.changed()))
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

pub fn read_sidecar(path: &std::path::Path) -> (Vec<AnnotationRecord>, String) {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let records = parse_records(&content);
            (records, content)
        }
        Err(_) => (vec![], annos_prelude()),
    }
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

pub use tinymist_project::iso_now;

/// The author name recorded for annotations created via the preview.
pub fn local_author() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

/// The author to record for one write.
///
/// A server reached over a network resolves this per request, from an identity
/// the request carries. Falling back to the local user is right rather than
/// merely convenient: a loopback visitor has no other identity, and is the
/// person running the server.
pub fn author_or_local(author: Option<&str>) -> String {
    author
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(local_author)
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
pub fn sentence_range(text: &str, at: usize) -> std::ops::Range<usize> {
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

/// Prepares the edits creating an annotation: the anchor label written into
/// the document, and the entry written into the sidecar.
///
/// Where the anchor goes is the client's decision — it knows what the reader
/// pointed at — so it arrives as a source range or an offset rather than as a
/// place on a page.
pub fn prepare_annotate(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
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
    Err("a source range is required".into())
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
    let id = doc_file(world);
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
        scope: scope.as_str().into(),
        color: req.color.clone().unwrap_or_default(),
        letter: next_letter(&records),
        author: author_or_local(req.author.as_deref()),
        content: req.text.clone(),
        time: iso_now(),
        mtime: String::new(),
        claimed: false,
        resolved: false,
        captures: vec![],
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

/// Creates a span annotation over a known source range.
fn prepare_span_range(
    art: &LspCompiledArtifact,
    req: &AnnotateRequest,
    range: std::ops::Range<usize>,
    encoding: PositionEncoding,
) -> Result<AnnotationEdit, String> {
    let world = art.world();
    let id = doc_file(world);
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
        scope: Scope::Span.as_str().into(),
        color: req.color.clone().unwrap_or_default(),
        letter: next_letter(&records),
        author: author_or_local(req.author.as_deref()),
        content: req.text.clone(),
        time: iso_now(),
        mtime: String::new(),
        claimed: false,
        resolved: false,
        captures: vec![],
        discussion: vec![],
    };
    let sidecar_content = format!("{content}{}", format_record(&rec));

    // Insert the end anchor first: inserting the begin anchor would shift
    // every later offset.
    let begin = format!("<{ANCHOR_PREFIX}{}.span.begin>", rec.uuid);
    let end = format!("<{ANCHOR_PREFIX}{}.span.end>", rec.uuid);
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

/// Removes one entry from the sidecar, returning (path, new content).
///
/// The document half is not touched: this is for a rewrite that already took
/// the anchor out with the text it replaced.
pub fn remove_record(
    art: &LspCompiledArtifact,
    uuid: &str,
) -> Result<(PathBuf, String), String> {
    if !valid_label(uuid) {
        return Err("bad annotation uuid".into());
    }
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let content = std::fs::read_to_string(&sidecar).unwrap_or_default();
    let span = entry_span(&content, uuid).ok_or_else(|| format!("unknown annotation: {uuid}"))?;
    let mut out = content;
    out.replace_range(span, "");
    Ok((sidecar, out))
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
    // Every write is a change, and the entry says when it last changed: the
    // callers each have their own reason to write, and none of them should have
    // to remember this one.
    record.mtime = iso_now();
    let mut new_content = content.clone();
    new_content.replace_range(span, &format_record(&record));
    Ok((sidecar, new_content))
}

/// Appends a discussion reply to an annotation.
pub fn prepare_reply(
    art: &LspCompiledArtifact,
    uuid: &str,
    text: &str,
    author: Option<&str>,
) -> Result<(PathBuf, String), String> {
    modify_record(art, uuid, |record| {
        record.discussion.push(AnnotationReply {
            author: author_or_local(author),
            time: iso_now(),
            content: text.to_owned(),
        });
    })
}

/// The hash of the last capture an annotation recorded, if it has any.
///
/// Read before storing a new one: a compile that changed nothing about a
/// drawing should leave the sidecar alone, and comparing hashes is how that is
/// known without writing anything.
pub fn last_capture(art: &LspCompiledArtifact, uuid: &str) -> Option<String> {
    let sidecar = sidecar_path(art)?;
    let (records, _) = read_sidecar(&sidecar);
    let record = records.into_iter().find(|record| record.uuid == uuid)?;
    record.captures.last().map(|capture| capture.hash.clone())
}

/// Appends a capture to an annotation, written to the sidecar.
pub fn add_capture(
    art: &LspCompiledArtifact,
    uuid: &str,
    capture: &AnnotationCapture,
) -> Result<(), String> {
    commit_sidecar(art, || {
        let (path, content) = modify_record(art, uuid, |record| {
            record.captures.push(capture.clone());
        })?;
        Ok((path, content, ()))
    })
}

/// Sets an annotation's flags. Either can be left alone.
pub fn prepare_flags(
    art: &LspCompiledArtifact,
    uuid: &str,
    claimed: Option<bool>,
    resolved: Option<bool>,
) -> Result<(PathBuf, String), String> {
    if claimed.is_none() && resolved.is_none() {
        return Err("nothing to set: pass claimed, resolved, or both".into());
    }
    modify_record(art, uuid, |record| {
        if let Some(claimed) = claimed {
            record.claimed = claimed;
        }
        if let Some(resolved) = resolved {
            record.resolved = resolved;
        }
    })
}

fn as_lsp(pos: LspPosition) -> lsp_types::Position {
    lsp_types::Position::new(pos.line, pos.character)
}

#[cfg(test)]
mod anchor_tests {
    use super::{block_range, snap_past_marker};

    /// The source of the block an anchor names, which is what an agent is
    /// handed and what it rewrites.
    fn block_of(text: &str) -> String {
        let at = text.find("<anno.").expect("the test source has an anchor");
        let source = typst::syntax::Source::detached(text);
        text[block_range(&source, at)].to_owned()
    }

    #[test]
    fn a_block_anchor_names_the_whole_call() {
        // The label sits beside the call, not inside it, and the call has a
        // blank line in the middle of it: scanning for blank lines would hand
        // back the second half of a figure.
        let text = "Before.\n\n#figure(\n  table(\n    [a], [b],\n\n    [c], [d],\n  ),\n  caption: [Two rows.],\n)<anno.T001.block>\n\nAfter.\n";
        let block = block_of(text);
        assert!(block.starts_with("#figure("), "cut off at the blank line: {block:?}");
        // The call, and the label that names it.
        assert!(block.ends_with(")<anno.T001.block>"), "stops short: {block:?}");
    }

    #[test]
    fn a_block_carries_the_anchors_attached_to_it() {
        // The label attaches to the heading, and sits past the heading's own
        // node: the block has to reach over it or an agent is handed a block
        // with no anchors in it.
        let text = "= Annotated example<anno.A100.word>\n\nBody.\n";
        let at = text.find("<anno.").unwrap();
        let source = typst::syntax::Source::detached(text);
        let block = &text[block_range(&source, at)];
        assert_eq!(block, "= Annotated example<anno.A100.word>");
    }

    #[test]
    fn a_paragraph_anchor_still_names_its_paragraph() {
        let text = "One.\n\nA paragraph <anno.X002.para> with an anchor in it.\n\nThree.\n";
        let block = block_of(text);
        assert!(block.starts_with("A paragraph"), "{block:?}");
        assert!(block.trim_end().ends_with("in it."), "{block:?}");
    }

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
        // a heading: the "=" run is a marker too, and a label before it turns
        // the heading into a paragraph that starts with "=".
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

    #[test]
    fn entry_span_ignores_the_prelude_example() {
        // The prelude shows what an entry looks like, uuid and all. An entry
        // that shares that uuid must still be found where it actually is —
        // not in the explanation, which was how a sidecar lost its header.
        let content = concat!(
            "// Entry shape:\n",
            "//\n",
            "//   #metadata((\n",
            "//     uuid: str,\n",
            "//   )) <note-7C42>\n",
            "#metadata((\n",
            "  uuid: \"7C42\",\n",
            ")) <note-7C42>\n",
        );
        let span = super::entry_span(content, "7C42").expect("the real entry");
        assert_eq!(&content[span.clone()], "#metadata((\n  uuid: \"7C42\",\n)) <note-7C42>\n");
        assert!(super::entry_span(content, "0000").is_none());
    }
}

// ---------------------------------------------------------------- blocks
//
// What an agent works in. An annotation says "this sentence is wrong" and an
// agent has to answer with source: the smallest piece of the document it can
// rewrite whole and mean it. That is the syntax node the anchor sits in whose
// parent is markup — a paragraph, a list item, a heading, a figure call — the
// same rule a `block` annotation is resolved with, so the two agree about what
// a block is.
//
// Resolution is syntactic, so it still works when the document does not
// compile, which is when an agent is most likely to be asked for help.

/// A piece of a document, and everything needed to rewrite it safely.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBlock {
    /// What this block is, now: a digest of where it is and what it says. An
    /// edit carries it back, and is refused if the block has moved on — the
    /// human editing the same file always wins.
    pub block_id: String,
    /// The file the block is in, which is not always the document being
    /// served: an anchor can sit in an included file.
    pub file: String,
    /// The byte range of the block in that file.
    pub range: (usize, usize),
    /// The 1-based line range, for saying where it is.
    pub lines: (usize, usize),
    /// The source of the block, exactly.
    pub text: String,
    /// The headings above it, outermost first: a paragraph out of its section
    /// is usually not enough to act on, and this is far cheaper than the
    /// section.
    pub heading_path: Vec<String>,
    /// Every annotation anchored inside this block, with the offset of its
    /// anchor *within the block* — what a rewrite has to carry across.
    pub anchors: Vec<BlockAnchor>,
    /// The blocks either side, when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The block after this one, when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// One annotation's anchor, as it sits inside a block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockAnchor {
    /// The annotation this anchor belongs to.
    pub uuid: String,
    /// The anchor's scope, as its label spells it.
    pub scope: String,
    /// Where the label starts, relative to the block.
    pub at: usize,
    /// The label itself, so a rewrite can put it back verbatim.
    pub label: String,
}

/// A digest of a block: the file it is in, where it starts, and what it says.
fn block_id(file: &str, start: usize, text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in file
        .as_bytes()
        .iter()
        .chain(start.to_le_bytes().iter())
        .chain(text.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The block containing an offset: the innermost thing that can be rewritten
/// whole and still mean something.
///
/// A list item, an enumeration item, a term, a heading, a block equation, or a
/// call written at the head of a line — a figure, a callout, a diagram. The
/// innermost, so that a nested bullet is a bullet rather than the list it is
/// in; but not the argument of a call, since a caption rewritten without its
/// figure is a caption for nothing. Failing all of those, the paragraph: markup holds its lines directly, so
/// a paragraph is not a node and is bounded the way a reader bounds one, by the
/// blank lines around it.
fn block_range(source: &typst::syntax::Source, at: usize) -> std::ops::Range<usize> {
    // Where the anchor sits, and — for the scopes that name the thing before
    // them — what it follows. `#figure(…)<anno.T001.block>` puts the label
    // beside the call rather than inside it, so asked about the label alone the
    // tree answers with the markup around it and the block would be found by
    // scanning for blank lines instead: a figure with a blank line in the
    // middle would come back cut in half, which is not source anybody can
    // rewrite.
    let range = block_at(source, at)
        .or_else(|| block_at(source, at.checked_sub(1)?))
        .unwrap_or_else(|| paragraph_range(source.text(), at));
    with_trailing_anchors(source.text(), range)
}

/// Extends a range over the anchors written immediately after it.
///
/// A label attaches to what precedes it, so `= Title<anno.A100.word>` puts the
/// anchor just past the heading's own node — outside the block, on the wrong
/// side of the line an agent may rewrite. Left there, the block it reads has no
/// anchors in it, and the rule that refuses a rewrite for dropping one has
/// nothing to look at.
fn with_trailing_anchors(text: &str, mut range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let open = format!("<{ANCHOR_PREFIX}");
    while text[range.end..].starts_with(&open) {
        match text[range.end..].find('>') {
            Some(close) => range.end += close + 1,
            None => break,
        }
    }
    range
}

/// The innermost thing at an offset that can be rewritten whole, if there is
/// one.
fn block_at(source: &typst::syntax::Source, at: usize) -> Option<std::ops::Range<usize>> {
    use typst_shim::syntax::LinkedNodeExt;
    let root = typst::syntax::LinkedNode::new(source.root());
    let leaf = root.leaf_at_compat(at)?;
    let mut cursor = leaf;
    loop {
        if matches!(
            cursor.kind(),
            SyntaxKind::ListItem
                | SyntaxKind::EnumItem
                | SyntaxKind::TermItem
                | SyntaxKind::Heading
                | SyntaxKind::Equation
                | SyntaxKind::Raw
                | SyntaxKind::CodeBlock
                | SyntaxKind::FuncCall
        ) {
            // A call is only a block when it stands on its own: `#figure(…)` at
            // the head of a line is one, `#src("…")` inside a sentence is part
            // of the sentence.
            //
            // In markup the call's node begins at its name and the `#` sits
            // beside it, so the block is the call *and* its hash — otherwise
            // the hash is neither part of what is read nor part of what is
            // written back, and a rewrite leaves it stranded.
            let mut range = cursor.range();
            if matches!(cursor.kind(), SyntaxKind::FuncCall)
                && range.start > 0
                && source.text().as_bytes()[range.start - 1] == b'#'
            {
                range.start -= 1;
            }
            let standalone = source.text()[..range.start]
                .rfind('\n')
                .map(|line| source.text()[line + 1..range.start].trim().is_empty())
                .unwrap_or(range.start == 0);
            if standalone || !matches!(cursor.kind(), SyntaxKind::FuncCall | SyntaxKind::Raw) {
                return Some(range);
            }
        }
        match cursor.parent().cloned() {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    None
}

/// The headings above an offset, outermost first.
fn heading_path(source: &typst::syntax::Source, at: usize) -> Vec<String> {
    let text = source.text();
    let mut path: Vec<(usize, String)> = vec![];
    for line in text[..at].lines() {
        let depth = line.len() - line.trim_start_matches('=').len();
        if depth == 0 || !line[depth..].starts_with(' ') {
            continue;
        }
        let title: String = line[depth..]
            .trim()
            .chars()
            .take_while(|c| *c != '<')
            .collect();
        let title = title.trim().to_owned();
        if title.is_empty() {
            continue;
        }
        while path.last().is_some_and(|(last, _)| *last >= depth) {
            path.pop();
        }
        path.push((depth, title));
    }
    path.into_iter().map(|(_, title)| title).collect()
}

/// The block an annotation is anchored in.
pub fn block_of(
    art: &LspCompiledArtifact,
    uuid: &str,
    context: bool,
) -> Result<SourceBlock, String> {
    if !valid_label(uuid) {
        return Err("bad annotation uuid".into());
    }
    let world = art.world();
    // The document first, then whatever else the compile read: an anchor in an
    // included file belongs to that file, and is edited there.
    let files = std::iter::once(doc_file(world)).chain(art.depended_files().iter().copied());
    let hit = files.into_iter().find_map(|file| {
        let source = world.source(file).ok()?;
        let anchors = find_anchors(source.text(), uuid);
        let (at, _, _) = anchors.first().cloned()?;
        Some((file, source, at))
    });
    let (file, source, at) = hit.ok_or_else(|| format!("no anchor for {uuid}"))?;
    let path = world
        .path_for_id(file)
        .map_err(|err| err.to_string())?
        .to_err()
        .map_err(|err| err.to_string())?;
    let range = block_range(&source, at);
    let text = source.text()[range.clone()].to_owned();
    let file_name = path.display().to_string();

    // Every annotation anchored in this block, not only the one asked about: a
    // rewrite has to carry all of them, so it has to be told all of them.
    let mut anchors = vec![];
    let mut scan = range.start;
    while let Some(rel) = source.text()[scan..range.end].find("<anno.") {
        let start = scan + rel;
        let Some(close) = source.text()[start..range.end].find('>') else {
            break;
        };
        let label = source.text()[start..start + close + 1].to_owned();
        let inner = &label["<anno.".len()..label.len() - 1];
        if let Some((uuid, scope)) = inner.split_once('.') {
            if Scope::from_suffix(scope).is_some() {
                anchors.push(BlockAnchor {
                    uuid: uuid.to_owned(),
                    scope: scope.to_owned(),
                    at: start - range.start,
                    label,
                });
            }
        }
        scan = start + close + 1;
    }

    let line_of = |offset: usize| source.text()[..offset].lines().count().max(1);
    let (before, after) = if context {
        let prev = range
            .start
            .checked_sub(1)
            .map(|at| block_range(&source, at))
            .filter(|prev| prev.end <= range.start)
            .map(|prev| source.text()[prev].to_owned());
        let next = (range.end + 1 < source.text().len())
            .then(|| block_range(&source, range.end + 1))
            .filter(|next| next.start >= range.end)
            .map(|next| source.text()[next].to_owned());
        (prev, next)
    } else {
        (None, None)
    };

    Ok(SourceBlock {
        block_id: block_id(&file_name, range.start, &text),
        file: file_name,
        range: (range.start, range.end),
        lines: (line_of(range.start), line_of(range.end)),
        heading_path: heading_path(&source, range.start),
        text,
        anchors,
        before,
        after,
    })
}

/// What a rewrite is allowed to do with the anchors it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorPolicy {
    /// Every anchor in the old text must be in the new text. The default: an
    /// agent rewriting a paragraph would otherwise quietly orphan every
    /// annotation in it.
    Keep,
    /// Missing anchors are put back at the head of the block.
    Reattach,
    /// Missing anchors are meant, and their annotations go with them.
    Drop,
}

impl AnchorPolicy {
    /// Reads the policy an agent asked for.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "" | "keep" => Ok(Self::Keep),
            "reattach" => Ok(Self::Reattach),
            "drop" => Ok(Self::Drop),
            other => Err(format!("unknown anchor policy: {other}")),
        }
    }
}

/// A rewrite of one block, ready to be written.
#[derive(Debug, Clone)]
pub struct BlockEdit {
    /// The file to write.
    pub path: PathBuf,
    /// Its content afterwards.
    pub content: String,
    /// The annotations whose anchors the rewrite dropped, to be deleted with
    /// it.
    pub dropped: Vec<String>,
}

/// Prepares a rewrite of the block an annotation is anchored in.
///
/// The block is identified by what it was when it was read: if the file has
/// moved on — the author is editing it, another agent got there first — the
/// rewrite is refused rather than applied to the wrong text.
pub fn prepare_block_replace(
    art: &LspCompiledArtifact,
    uuid: &str,
    block_id_given: &str,
    new_text: &str,
    policy: AnchorPolicy,
) -> Result<BlockEdit, String> {
    let block = block_of(art, uuid, false)?;
    if block.block_id != block_id_given {
        return Err(format!(
            "stale block: {} is now {}; read it again",
            block_id_given, block.block_id
        ));
    }
    let path = PathBuf::from(&block.file);
    let disk = std::fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", block.file))?;
    // The block as it is on disk, which is what is being replaced. The compile
    // may be a moment behind an editor's buffer; the digest above is what
    // decides whether that matters.
    let (start, end) = block.range;
    if disk.len() < end || disk[start..end] != block.text {
        return Err("stale block: the file changed since it was read".into());
    }

    // An anchor the rewrite invented belongs to no annotation, and would
    // resolve to nothing.
    let mut scan = 0;
    while let Some(rel) = new_text[scan..].find("<anno.") {
        let at = scan + rel;
        let Some(close) = new_text[at..].find('>') else { break };
        let label = &new_text[at..at + close + 1];
        let inner = &label["<anno.".len()..label.len() - 1];
        let named = inner.split_once('.').map(|(uuid, _)| uuid).unwrap_or(inner);
        if !block.anchors.iter().any(|anchor| anchor.uuid == named) {
            return Err(format!("{label} is not an anchor this block had"));
        }
        scan = at + close + 1;
    }

    let missing: Vec<_> = block
        .anchors
        .iter()
        .filter(|anchor| !new_text.contains(&anchor.label))
        .cloned()
        .collect();
    let mut replacement = new_text.to_owned();
    let mut dropped = vec![];
    match policy {
        AnchorPolicy::Keep if !missing.is_empty() => {
            let names: Vec<_> = missing
                .iter()
                .map(|anchor| anchor.label.as_str())
                .collect();
            return Err(format!(
                "the rewrite drops {}; keep them, or say anchors=reattach or anchors=drop",
                names.join(", ")
            ));
        }
        AnchorPolicy::Reattach => {
            for anchor in missing.iter().rev() {
                replacement.insert_str(0, &anchor.label);
            }
        }
        AnchorPolicy::Drop => {
            dropped = missing.iter().map(|anchor| anchor.uuid.clone()).collect();
        }
        AnchorPolicy::Keep => {}
    }

    let mut content = disk;
    content.replace_range(start..end, &replacement);
    Ok(BlockEdit {
        path,
        content,
        dropped,
    })
}

/// The annotation server of a served document: every edit is written straight
/// to disk.
///
/// There is no editor to keep in step — the world compiles from disk, so its
/// sources cannot diverge except for a
/// brief window after an external change, which the best-effort patch
/// covers).
pub struct DiskAnnotationServer {
    /// The most recent compiled artifact.
    pub last_art: Arc<parking_lot::Mutex<Option<tinymist_project::LspCompiledArtifact>>>,
    /// The preview watchers holding the SSE diagnostics channel.
    pub watchers: crate::project::ProjectPreviewState,
    /// The project instance id.
    pub project_id: tinymist_project::ProjectInsId,
    /// Whether to emit annotation events as JSON lines on stdout, for
    /// driving agents: annotation_added, discussion_extended,
    /// annotation_deleted, annotation_status_changed.
    pub emit_events: bool,
}

impl DiskAnnotationServer {
    fn art(&self) -> Result<tinymist_project::LspCompiledArtifact, String> {
        self.last_art
            .lock()
            .clone()
            .ok_or_else(|| "no compiled artifact yet".to_owned())
    }

    /// Tells the pages that the annotations have moved. What changed is not
    /// sent — a page asks for them itself — so this is a number going up.
    fn push_pins(&self) {
        if let Some(diag_tx) = self.watchers.diag_tx(&self.project_id) {
            diag_tx.send_modify(|state| state.anno_version += 1);
        }
    }

    /// Applies the document half of an annotation edit (the sidecar half
    /// goes through [`commit_sidecar`]).
    fn apply_doc(&self, edit: &AnnotationEdit) -> Result<(), String> {
        let content = edit
            .disk_content
            .as_ref()
            .ok_or("cannot apply the edit: the file has diverged on disk")?;
        std::fs::write(&edit.path, content)
            .map_err(|e| format!("failed to write {}: {e}", edit.path.display()))?;
        self.push_pins();
        Ok(())
    }

    /// One event, as one line of JSON on stdout — where a driving agent reads
    /// them, unlike the server's own narration, which goes to stderr.
    ///
    /// Written field by field rather than serialised from a map, for the same
    /// reason the narration is: `type` first, `ts` last, and the rest in the
    /// order they were written, so the line reads the way it was designed to.
    fn emit(&self, kind: &str, fields: &[(&str, serde_json::Value)]) {
        if !self.emit_events {
            return;
        }
        let line = tinymist_project::event_line(kind, fields);
        // Kept as well as printed: an agent that asks what happened while it
        // was thinking gets these alongside everything else the server said.
        tinymist_project::record_event(serde_json::from_str(&line).unwrap_or_default());
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

impl crate::tool::serve::AnnotationServer for DiskAnnotationServer {
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String> {
        let art = self.art()?;
        let edit = commit_sidecar(&art, || {
            let edit = prepare_annotate(
                &art,
                &req,
                tinymist_query::PositionEncoding::Utf16,
            )?;
            Ok((edit.sidecar.clone(), edit.sidecar_content.clone(), edit))
        })?;
        self.apply_doc(&edit)?;
        let record = parse_records(&edit.sidecar_content)
            .into_iter()
            .find(|rec| rec.uuid == edit.uuid);
        if let Some(record) = record {
            self.emit(
                "annotation_added",
                &[("value", serde_json::to_value(&record).unwrap_or_default())],
            );
        }
        Ok(edit.uuid)
    }

    fn remove(&self, uuid: &str) -> Result<(), String> {
        let art = self.art()?;
        let edit = commit_sidecar(&art, || {
            let edit = prepare_delete(
                &art,
                uuid,
                tinymist_query::PositionEncoding::Utf16,
            )?;
            Ok((edit.sidecar.clone(), edit.sidecar_content.clone(), edit))
        })?;
        self.apply_doc(&edit)?;
        self.emit("annotation_deleted", &[("uuid", uuid.into())]);
        Ok(())
    }

    fn reply(&self, uuid: &str, text: &str, author: Option<&str>) -> Result<(), String> {
        let art = self.art()?;
        commit_sidecar(&art, || {
            let (path, content) = prepare_reply(&art, uuid, text, author)?;
            Ok((path, content, ()))
        })?;
        self.push_pins();
        self.emit(
            "discussion_extended",
            &[
                ("uuid", uuid.into()),
                ("author", local_author().into()),
                ("text", text.into()),
            ],
        );
        Ok(())
    }

    fn set_flags(
        &self,
        uuid: &str,
        claimed: Option<bool>,
        resolved: Option<bool>,
    ) -> Result<(), String> {
        let art = self.art()?;
        commit_sidecar(&art, || {
            let (path, content) = prepare_flags(&art, uuid, claimed, resolved)?;
            Ok((path, content, ()))
        })?;
        self.push_pins();
        // Both flags, whichever moved: an agent reading the line wants the
        // state of the annotation, not the diff that got it there.
        let now = self
            .records()
            .ok()
            .and_then(|records| records.into_iter().find(|record| record.uuid == uuid));
        let claimed = now.as_ref().map(|rec| rec.claimed).unwrap_or(false);
        let resolved = now.as_ref().map(|rec| rec.resolved).unwrap_or(false);
        self.emit(
            "annotation_status_changed",
            &[
                ("uuid", uuid.into()),
                ("claimed", claimed.into()),
                ("resolved", resolved.into()),
            ],
        );
        Ok(())
    }


    fn block(&self, uuid: &str, context: bool) -> Result<SourceBlock, String> {
        block_of(&self.art()?, uuid, context)
    }

    fn replace_block(
        &self,
        uuid: &str,
        block_id: &str,
        new_text: &str,
        policy: AnchorPolicy,
    ) -> Result<Vec<String>, String> {
        let art = self.art()?;
        let edit = prepare_block_replace(&art, uuid, block_id, new_text, policy)?;
        std::fs::write(&edit.path, &edit.content)
            .map_err(|err| format!("cannot write {}: {err}", edit.path.display()))?;
        // The anchors the rewrite dropped were dropped on purpose, so their
        // annotations go too — an entry with nothing to point at is litter.
        for uuid in &edit.dropped {
            // The anchor is already gone with the text; only the entry is left.
            if let Ok((path, content)) = remove_record(&art, uuid) {
                let _ = std::fs::write(path, content);
            }
        }
        self.emit(
            "block_replaced",
            &[
                ("uuid", uuid.into()),
                ("file", edit.path.display().to_string().into()),
                ("dropped", serde_json::to_value(&edit.dropped).unwrap_or_default()),
            ],
        );
        Ok(edit.dropped)
    }

    fn compile_revision(&self) -> u64 {
        self.last_art
            .lock()
            .as_ref()
            .map(|art| {
                let rev = art.graph.snap.world.revision();
                rev.get() as u64
            })
            .unwrap_or(0)
    }

    fn diagnostics(&self) -> (bool, Vec<String>) {
        let Ok(art) = self.art() else {
            return (false, vec!["nothing compiled yet".into()]);
        };
        let payload = crate::tool::preview::diagnostics_payload(&art, None);
        (payload.ok, payload.messages)
    }

    fn records(&self) -> Result<Vec<AnnotationRecord>, String> {
        let art = self.art()?;
        let sidecar =
            sidecar_path(&art).ok_or("cannot determine the sidecar path")?;
        Ok(read_sidecar(&sidecar).0)
    }

    fn pins(&self) -> Vec<super::pins::HtmlPin> {
        match self.art() {
            Ok(art) => super::pins::html_pins(&art),
            Err(_) => vec![],
        }
    }



}
