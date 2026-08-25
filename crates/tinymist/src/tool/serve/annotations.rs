//! Annotations, as a server keeps them.
//!
//! The model and the file format are `tinymist-annos`; this is the part that
//! needs a compiled document. It finds the block of source an annotation sits
//! in, rewrites that block on request, converts locations between the rendering
//! and the document, and writes both halves — the anchor in the `.typ`, the
//! record in the `.annos.json` — to disk.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tinymist_annos::location::HtmlLocation;
use tinymist_annos::{anchor, Annotation, Sidecar};
use tinymist_project::LspCompiledArtifact;
use typst::World;
use typst::syntax::SyntaxKind;

/// An annotation as this server holds it.
pub type AnnotationRecord = Annotation;
/// One reply in a discussion.
pub type AnnotationReply = tinymist_annos::Reply;
/// A picture of what an annotation pointed at.
pub type AnnotationCapture = tinymist_annos::Capture;

/// The static assets the annotator needs, from `src/static/annos`.
pub fn dev_asset(rel: &str, embedded: &'static str) -> String {
    crate::tool::asset::dev_asset_in("annos", rel, embedded)
}

/// Where a document's annotations are kept.
pub fn sidecar_path(art: &LspCompiledArtifact) -> Option<PathBuf> {
    let world = art.world();
    let main = world.main();
    let path = world.path_for_id(main).ok()?.to_err().ok()?;
    Some(tinymist_annos::sidecar_path(&path))
}

/// The document's own path.
pub fn document_path(art: &LspCompiledArtifact) -> Option<PathBuf> {
    let world = art.world();
    world.path_for_id(world.main()).ok()?.to_err().ok()
}

/// Reads a document's annotations.
pub fn read_sidecar(path: &std::path::Path) -> Sidecar {
    Sidecar::read(path).unwrap_or_default()
}

/// The same, for whoever is about to write it back.
///
/// A file that cannot be read is not a file with nothing in it. Reading one as
/// empty and then writing it back is how a sidecar full of somebody's work
/// becomes a sidecar with one annotation in it, so anything that is going to
/// write says here that it could not read.
fn read_sidecar_to_change(path: &std::path::Path) -> Result<Sidecar, String> {
    Sidecar::read(path)
}

/// Serialised so that two writes of the same annotations produce the same
/// bytes, since these files are kept in a repository beside the documents.
pub fn write_sidecar(path: &std::path::Path, sidecar: &Sidecar) -> Result<(), String> {
    sidecar.write(path)
}

/// Held while a sidecar is read, changed and written, so that two requests
/// arriving together do not each write what the other did not see.
static SIDECAR_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Reads a document's annotations, changes them, and writes them back.
pub fn revise<T>(
    path: &std::path::Path,
    change: impl FnOnce(&mut Sidecar) -> Result<T, String>,
) -> Result<T, String> {
    let _held = SIDECAR_LOCK.lock();
    let mut sidecar = read_sidecar_to_change(path)?;
    let out = change(&mut sidecar)?;
    write_sidecar(path, &sidecar)?;
    Ok(out)
}

/// Who to record as the author of something written through this server.
pub fn local_author() -> String {
    std::env::var("TALIMIST_AUTHOR")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "someone".to_owned())
}

/// The author of a request: the one the server resolved, or this machine's.
pub fn author_or_local(author: Option<&str>) -> String {
    author
        .map(str::to_owned)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(local_author)
}

/// A uuid for a new annotation: sixty-four bits of hex, from the clock and the
/// text, and never seen in the document.
pub fn fresh_uuid(seed: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64)
        .unwrap_or(0);
    for byte in seed.as_bytes().iter().chain(&now.to_le_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What a page asks for when somebody writes an annotation.
#[derive(Debug, Clone, Deserialize)]
pub struct AnnotateRequest {
    /// Where it points, as the page sees the document.
    pub location: HtmlLocation,
    /// The rendering the location was taken against.
    pub render: String,
    /// The comment text.
    pub text: String,
    /// What kind of remark it is: a comment, a question, a request.
    #[serde(default)]
    pub kind: Option<String>,
    /// The colour the page drew it in, as `#rrggbb`.
    #[serde(default)]
    pub color: Option<String>,
    /// What was there when the annotation was made, for saying what it was
    /// about if the anchor is later deleted.
    #[serde(default)]
    pub snapshot: Option<String>,
    /// Who to record as the author. Resolved by the server from the request
    /// itself, never read from the body: a client that could name its own
    /// author could sign a colleague's name to a comment.
    #[serde(skip)]
    pub author: Option<String>,
    /// What was drawn while writing it, if anything was.
    #[serde(default)]
    pub scribble: Option<NewScribble>,
}

/// A drawing as it arrives from a page: the shapes, and the picture they were
/// drawn on, which the server has not seen before.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NewScribble {
    /// The name the page gave it.
    pub id: String,
    /// What was drawn, in the picture's coordinates.
    pub shapes: Vec<tinymist_annos::Mark>,
    /// The picture itself.
    pub picture: NewPicture,
}

/// A picture a page took of what an annotation is about.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NewPicture {
    /// `svg` or `png`.
    pub fmt: String,
    /// SVG text, or base64 for a PNG.
    pub data: String,
    /// How wide it is on the page, in CSS pixels.
    pub width: u32,
    /// How tall.
    pub height: u32,
}

/// Stores the picture a scribble was drawn on and returns the scribble, filed
/// against the capture it belongs to.
///
/// A picture that is already there is not stored twice: the same drawing
/// scribbled on a second time is the same capture, and a scribble names it.
pub fn keep_scribble(
    record: &mut AnnotationRecord,
    new: NewScribble,
) -> Result<tinymist_annos::Scribble, String> {
    let bytes = match new.picture.fmt.as_str() {
        "svg" => new.picture.data.into_bytes(),
        "png" => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(new.picture.data.as_bytes())
                .map_err(|err| format!("the picture is not base64: {err}"))?
        }
        other => return Err(format!("a capture cannot be {other}")),
    };
    let hash = crate::tool::serve::capture::store(&bytes, &new.picture.fmt)
        .ok_or("cannot store the capture")?;
    // The same bytes are the same picture, and a picture that says it is a
    // different size is a different capture: a scribble's coordinates are
    // relative to what its capture says it measures.
    let held = record.captures.iter().find(|capture| {
        capture.hash == hash
            && capture.fmt == new.picture.fmt
            && capture.width == new.picture.width
            && capture.height == new.picture.height
    });
    let capture = match held {
        Some(capture) => capture.id.clone(),
        None => {
            let id = fresh_uuid(&hash);
            record.captures.push(tinymist_annos::Capture {
                id: id.clone(),
                time: tinymist_project::iso_now(),
                fmt: new.picture.fmt,
                hash,
                width: new.picture.width,
                height: new.picture.height,
            });
            id
        }
    };
    Ok(tinymist_annos::Scribble {
        id: new.id,
        capture,
        shapes: new.shapes,
    })
}


/// One of a document's files, as it stands and as it was.
pub struct DocumentFile {
    /// Where it is.
    pub path: std::path::PathBuf,
    /// What it says now.
    pub source: typst::syntax::Source,
    /// What it said when the rendering was made.
    pub was: String,
}

/// Every file a rendering drew on, read from disk.
///
/// In the order the map numbers them, so that a file index in a location or an
/// edit means the same thing here as it does there. A file that cannot be read
/// now is given the text it had, which keeps the numbering and fails only the
/// annotations that are actually in it.
pub fn document_files(
    stored: &tinymist_annos::store::StoredRender,
) -> Result<Vec<DocumentFile>, String> {
    let was = &stored.texts;
    Ok(stored
        .map
        .files
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = std::path::PathBuf::from(&entry.path);
            let before = was.get(index).cloned().unwrap_or_default();
            let text = std::fs::read_to_string(&path).unwrap_or_else(|_| before.clone());
            DocumentFile {
                path,
                source: typst::syntax::Source::detached(text),
                was: before,
            }
        })
        .collect())
}

pub trait AnnotationServer: Send + Sync {
    /// Creates an annotation at a clicked position. Returns the new uuid.
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String>;
    /// Deletes an annotation by uuid.
    fn remove(&self, uuid: &str) -> Result<(), String>;
    /// Appends a reply to an annotation's discussion. `author` is the identity
    /// the server resolved for the request, if it found one; `scribble` is what
    /// was drawn while writing it, if anything was.
    fn reply(
        &self,
        uuid: &str,
        text: &str,
        author: Option<&str>,
        scribble: Option<NewScribble>,
    ) -> Result<(), String>;
    /// Sets an annotation's flags — claimed, resolved, or both. Whichever is
    /// left out is left alone.
    fn set_flags(&self, uuid: &str, claimed: Option<bool>, resolved: Option<bool>)
        -> Result<(), String>;
    /// The block of source an annotation is anchored in — what an agent reads
    /// before rewriting it.
    fn block(&self, _uuid: &str, _context: bool) -> Result<SourceBlock, String> {
        Err("this server cannot read blocks".into())
    }

    /// What the document and its sidecar say about each other: anchors nothing
    /// points at, annotations pointing at anchors that are gone.
    fn audit(&self) -> Result<serde_json::Value, String> {
        Err("this server cannot audit annotations".into())
    }

    /// Whether the document compiles, what it is made of, and how many readers
    /// are connected.
    fn status(&self) -> Result<serde_json::Value, String> {
        Err("this server cannot report its status".into())
    }

    /// The file the document is, when the server knows.
    fn document(&self) -> Option<PathBuf> {
        None
    }

    /// What the document says about being generated, when it says anything.
    ///
    /// A generated document is rewritten whole whenever whatever generates it
    /// runs, and an anchor written into it does not survive that. So it cannot
    /// be annotated in place, and a page showing one says so instead of
    /// offering to.
    fn generated(&self) -> Option<String> {
        None
    }

    /// Compiles a fragment of Typst with the document's root and libraries, and
    /// returns the rendering.
    fn render_snippet(
        &self,
        _source: &str,
        _format: &str,
    ) -> Result<serde_json::Value, String> {
        Err("this server cannot render snippets".into())
    }

    /// Locations taken against an older rendering, expressed against the
    /// current one. A location that cannot be found again comes back as
    /// `None`.
    fn relocate(
        &self,
        _render: &str,
        _locations: &[tinymist_annos::HtmlLocation],
    ) -> Result<Vec<Option<tinymist_annos::HtmlLocation>>, String> {
        Err("this server cannot relocate".into())
    }

    /// Rewrites that block, returning the annotations whose anchors the
    /// rewrite deliberately dropped.
    fn replace_block(
        &self,
        _uuid: &str,
        _block_id: &str,
        _new_text: &str,
        _policy: AnchorPolicy,
        _force: bool,
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


/// Creates an annotation whose anchor goes at a known source offset.


#[cfg(test)]
mod anchor_tests {
    use super::block_range;

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
    pub anchors: Vec<Anchor>,
    /// The line in which the file declares itself generated, if there is one.
    /// An edit to such a file is undone by the next run of its generator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    /// The blocks either side, when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The block after this one, when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// An anchor as it sits inside a block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    /// The label, as an annotation's location refers to it: `anno.A100`.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Where the label starts, relative to the block.
    pub at: usize,
    /// The label itself, so a rewrite can put it back verbatim.
    pub label: String,
    /// The annotations that point at this anchor. Several may: an element
    /// carries one label, and two remarks about the same word share it.
    pub annotations: Vec<String>,
}

/// The line in which a file says it is generated, if there is one.
///
/// Editing a generated file has no lasting effect: the generator overwrites it
/// on its next run. Such files usually declare themselves in the first few
/// lines, and the declaration is returned verbatim so a caller can quote it.
///
/// `GENERATED` has to be shouted. A comment mentioning that a file was
/// generated from something is prose, and prose about a document is not the
/// document saying that editing it will not last; the shouted form is the
/// marker a generator writes. `AUTOGENERATED` and `AUTO-GENERATED` both contain
/// it, so one test covers the three.
pub fn generated_note(text: &str) -> Option<String> {
    text.lines().take(8).find_map(|line| {
        let said = line.trim();
        if !said.starts_with("//") && !said.starts_with("#") {
            return None;
        }
        let claims = said.contains("GENERATED") || said.to_ascii_uppercase().contains("DO NOT EDIT");
        claims.then(|| said.trim_start_matches('/').trim().to_owned())
    })
}

#[cfg(test)]
mod generated_tests {
    use super::generated_note;

    #[test]
    fn a_shouted_marker_says_the_file_is_generated() {
        assert_eq!(
            generated_note("// GENERATED by report.py. Do not edit by hand.\n= A report\n").as_deref(),
            Some("GENERATED by report.py. Do not edit by hand.")
        );
        assert!(generated_note("// AUTOGENERATED\n").is_some());
        assert!(generated_note("// AUTO-GENERATED from parts\n").is_some());
    }

    #[test]
    fn prose_about_being_generated_is_only_prose() {
        assert_eq!(generated_note("// The figures are generated by plot.py\n"), None);
        assert_eq!(generated_note("// Generated from the run files\n"), None);
    }

    #[test]
    fn only_a_comment_at_the_top_counts() {
        assert_eq!(generated_note("= A report\n\nIt is GENERATED, in a way.\n"), None);
        let deep = "\n".repeat(9) + "// GENERATED by report.py\n";
        assert_eq!(generated_note(&deep), None);
    }
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
    let open = format!("<{}", tinymist_annos::ANCHOR_PREFIX);
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

/// Every file the compile read, the document first, each with its text.
///
/// An anchor may be written into a file the document includes, so anything
/// that looks for one looks in all of them. Files that are not Typst source,
/// and files that cannot be read, are left out. The list has no repeats.
pub fn compiled_files(art: &LspCompiledArtifact) -> Vec<(PathBuf, typst::syntax::Source)> {
    let world = art.world();
    let mut out: Vec<(PathBuf, typst::syntax::Source)> = vec![];
    for id in std::iter::once(world.main()).chain(art.depended_files().iter().copied()) {
        let Ok(source) = world.source(id) else { continue };
        let Ok(path) = world.path_for_id(id).and_then(|path| path.to_err()) else {
            continue;
        };
        if out.iter().any(|(seen, _)| *seen == path) {
            continue;
        }
        out.push((path, source));
    }
    out
}

/// The block an annotation is anchored in.
pub fn block_of(
    art: &LspCompiledArtifact,
    uuid: &str,
    context: bool,
) -> Result<SourceBlock, String> {
    // Which anchor the annotation points at. A span has two; the block is the
    // one the first end sits in.
    let sidecar = sidecar_path(art).ok_or("cannot determine the sidecar path")?;
    let record = read_sidecar(&sidecar)
        .find(uuid)
        .cloned()
        .ok_or_else(|| format!("no annotation {uuid}"))?;
    let label = record
        .labels()
        .first()
        .map(|label| label.to_string())
        .ok_or_else(|| format!("annotation {uuid} names no anchor"))?;
    let id = tinymist_annos::anchor_id(&label).unwrap_or(&label).to_owned();

    // Searched across every file, since the anchor may be in one the document
    // includes. The block is then read and rewritten in that file.
    let hit = compiled_files(art).into_iter().find_map(|(path, source)| {
        let at = anchor::find(&source, &id)?.at();
        Some((path, source, at))
    });
    let (path, source, at) = hit.ok_or_else(|| format!("no anchor {label} in the document"))?;
    let range = block_range(&source, at);
    let text = source.text()[range.clone()].to_owned();
    let file_name = path.display().to_string();

    // Every annotation anchored in this block, not only the one asked about: a
    // rewrite has to carry all of them, so it has to be told all of them.
    // The labels inside the block, so that a rewrite can be checked against
    // them. Which annotations each one belongs to is read from the sidecar: an
    // anchor no longer says.
    let records = sidecar_path(art)
        .map(|path| read_sidecar(&path).annotations)
        .unwrap_or_default();
    let anchors: Vec<Anchor> = anchor::anchors_in(&source)
        .into_iter()
        .filter(|found| range.start <= found.at() && found.label.end <= range.end)
        .map(|found| {
            let reference = found.name();
            Anchor {
                at: found.at() - range.start,
                label: source.text()[found.label.clone()].to_owned(),
                annotations: records
                    .iter()
                    .filter(|rec| rec.labels().iter().any(|label| *label == reference))
                    .map(|rec| rec.uuid.clone())
                    .collect(),
                reference,
            }
        })
        .collect();

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
        generated: generated_note(source.text()),
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
    /// Missing anchors are meant. What pointed at them is kept, and becomes an
    /// annotation about the document: somebody wrote it, and the rewrite says
    /// nothing about whether it still has something to say.
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
    /// The anchors the rewrite dropped. What pointed at them is now about the
    /// document.
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
        // Labels of the rewriter's own are fine; anchors it invented are not,
        // since nothing would point at them.
        if !block.anchors.iter().any(|anchor| anchor.label == label) {
            return Err(format!(
                "{label} is not an anchor this block had. Labels of your own are fine; \
                 anchors must be the ones you were given."
            ));
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
            // The labels the rewrite left out, so that whatever pointed at
            // them can be removed with them.
            dropped = missing
                .iter()
                .map(|anchor| anchor.reference.clone())
                .collect();
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

impl DiskAnnotationServer {
    /// Writes an annotation about the document as a whole.
    ///
    /// Nothing is resolved and nothing is written into the document: the
    /// location names no anchor, so there is no place to find and none to make.
    fn annotate_document(&self, req: &AnnotateRequest) -> Result<String, String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let sidecar_path = tinymist_annos::sidecar_path(&document);
        let now = tinymist_project::iso_now();
        let record = Annotation {
            uuid: fresh_uuid(&req.text),
            letter: String::new(),
            location: tinymist_annos::TypstLocation::Document,
            snapshot: req.snapshot.clone(),
            kind: req.kind.clone().unwrap_or_else(|| "comment".to_owned()),
            color: req.color.clone().unwrap_or_default(),
            author: author_or_local(req.author.as_deref()),
            time: now.clone(),
            mtime: now,
            claimed: false,
            resolved: false,
            content: req.text.clone(),
            scribble: None,
            discussion: vec![],
            captures: vec![],
        };
        let uuid = record.uuid.clone();
        let mut drawn = req.scribble.clone();
        let record = revise(&sidecar_path, |sidecar| {
            let mut record = record;
            if let Some(new) = drawn.take() {
                record.scribble = Some(keep_scribble(&mut record, new)?);
            }
            record.letter = sidecar.next_letter();
            sidecar.put(record.clone());
            Ok(record)
        })?;
        self.push_pins();
        self.emit(
            "annotation_added",
            &[("value", serde_json::to_value(&record).unwrap_or_default())],
        );
        Ok(uuid)
    }

    /// The document, its sidecar, and the rendering a request refers to.
    fn context(&self, render: &str) -> Result<(PathBuf, PathBuf, tinymist_annos::StoredRender), String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let sidecar = tinymist_annos::sidecar_path(&document);
        let stored = super::renders::get(&document, render)
            .ok_or("that rendering is no longer held; reload the page and try again")?;
        Ok((document, sidecar, stored))
    }
}

impl crate::tool::serve::AnnotationServer for DiskAnnotationServer {
    fn annotate(&self, req: AnnotateRequest) -> Result<String, String> {
        // About the document rather than a place in it: nothing to resolve, no
        // anchor to write, and no rendering to do either of those against. An
        // agent can leave one without a page ever having been open.
        if matches!(req.location, tinymist_annos::HtmlLocation::Document) {
            return self.annotate_document(&req);
        }
        let (_document, sidecar_path, stored) = self.context(&req.render)?;
        // Every file of the document, as each stands now, since the anchor may
        // have to be written into an included file.
        let files = document_files(&stored)?;
        let held: Vec<tinymist_annos::resolve::FileText> = files
            .iter()
            .map(|file| tinymist_annos::resolve::FileText {
                source: &file.source,
                was: &file.was,
            })
            .collect();

        // Resolve against the files as they stand, since they may have been
        // edited since the rendering the page is looking at was made.
        let ctx = tinymist_annos::resolve::Context {
            map: &stored.map,
            files: &held,
            seed: self.compile_revision(),
        };
        let resolution = tinymist_annos::resolve::resolve(&ctx, &req.location)
            .map_err(|err| describe(&err))?;

        // An anchor written into a generated file lasts until whatever
        // generates it runs again, and then the annotation points at a label
        // no file has. So nothing is written, and the annotation is kept as one
        // about the document — which is what it would have become anyway, only
        // now rather than at the next build, and with what was said still here.
        let generated = files
            .iter()
            .enumerate()
            .filter(|(index, _)| resolution.edits.iter().any(|edit| edit.file == *index))
            .find_map(|(_, file)| {
                generated_note(file.source.text()).map(|said| (file.path.clone(), said))
            });
        let location = match &generated {
            Some((path, said)) => {
                self.emit(
                    "annotation_orphaned",
                    &[
                        ("file", path.display().to_string().into()),
                        ("said", said.clone().into()),
                    ],
                );
                tinymist_annos::TypstLocation::Document
            }
            None => {
                // The anchor goes into whichever file the place is in, if it
                // did not have one already.
                for (index, file) in files.iter().enumerate() {
                    if !resolution.edits.iter().any(|edit| edit.file == index) {
                        continue;
                    }
                    let written = anchor::apply(index, file.source.text(), &resolution.edits);
                    std::fs::write(&file.path, &written)
                        .map_err(|err| format!("cannot write {}: {err}", file.path.display()))?;
                }
                resolution.location
            }
        };

        let now = tinymist_project::iso_now();
        let uuid = fresh_uuid(&req.text);
        let record = Annotation {
            uuid: uuid.clone(),
            letter: String::new(),
            location,
            snapshot: req.snapshot.clone(),
            kind: req.kind.clone().unwrap_or_else(|| "comment".to_owned()),
            color: req.color.clone().unwrap_or_default(),
            author: author_or_local(req.author.as_deref()),
            time: now.clone(),
            mtime: now,
            claimed: false,
            resolved: false,
            content: req.text.clone(),
            scribble: None,
            discussion: vec![],
            captures: vec![],
        };
        let mut drawn = req.scribble.clone();
        let record = revise(&sidecar_path, |sidecar| {
            // The letter is the server's to give: two pages composing at once
            // would otherwise both think they are `c`.
            let mut record = record;
            if let Some(new) = drawn.take() {
                record.scribble = Some(keep_scribble(&mut record, new)?);
            }
            record.letter = sidecar.next_letter();
            sidecar.put(record.clone());
            Ok(record)
        })?;

        self.push_pins();
        self.emit(
            "annotation_added",
            &[("value", serde_json::to_value(&record).unwrap_or_default())],
        );
        Ok(uuid)
    }

    fn remove(&self, uuid: &str) -> Result<(), String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let sidecar_path = tinymist_annos::sidecar_path(&document);

        let orphaned = revise(&sidecar_path, |sidecar| {
            let record = sidecar
                .find(uuid)
                .cloned()
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            sidecar.remove(uuid);
            // Anchors this annotation was the last user of.
            let still_used = sidecar.labels_in_use();
            Ok(record
                .labels()
                .into_iter()
                .filter(|label| !still_used.iter().any(|used| used == label))
                .map(str::to_owned)
                .collect::<Vec<_>>())
        })?;

        // An anchor nothing points at is removed from the document.
        if !orphaned.is_empty() {
            let text = std::fs::read_to_string(&document)
                .map_err(|err| format!("cannot read {}: {err}", document.display()))?;
            let source = typst::syntax::Source::detached(text.clone());
            let mut cuts: Vec<std::ops::Range<usize>> = anchor::anchors_in(&source)
                .into_iter()
                .filter(|found| orphaned.iter().any(|label| *label == found.name()))
                .map(|found| found.label)
                .collect();
            cuts.sort_by_key(|range| std::cmp::Reverse(range.start));
            let mut written = text;
            for range in cuts {
                written.replace_range(range, "");
            }
            std::fs::write(&document, &written)
                .map_err(|err| format!("cannot write {}: {err}", document.display()))?;
        }

        self.push_pins();
        self.emit("annotation_deleted", &[("uuid", uuid.into())]);
        Ok(())
    }

    fn reply(
        &self,
        uuid: &str,
        text: &str,
        author: Option<&str>,
        scribble: Option<NewScribble>,
    ) -> Result<(), String> {
        let art = self.art()?;
        let sidecar_path = sidecar_path(&art).ok_or("cannot determine the sidecar path")?;
        let author = author_or_local(author);
        let now = tinymist_project::iso_now();
        let mut scribble = scribble;
        revise(&sidecar_path, |sidecar| {
            let record = sidecar
                .find_mut(uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            let drawn = match scribble.take() {
                Some(new) => Some(keep_scribble(record, new)?),
                None => None,
            };
            record.discussion.push(AnnotationReply {
                author: author.clone(),
                time: now.clone(),
                content: text.to_owned(),
                scribble: drawn,
            });
            record.mtime = now.clone();
            Ok(())
        })?;
        self.push_pins();
        self.emit(
            "discussion_extended",
            &[
                ("uuid", uuid.into()),
                ("author", author.into()),
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
        let sidecar_path = sidecar_path(&art).ok_or("cannot determine the sidecar path")?;
        let now = tinymist_project::iso_now();
        let (claimed, resolved) = revise(&sidecar_path, |sidecar| {
            let record = sidecar
                .find_mut(uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            if let Some(claimed) = claimed {
                record.claimed = claimed;
            }
            if let Some(resolved) = resolved {
                record.resolved = resolved;
            }
            record.mtime = now.clone();
            Ok((record.claimed, record.resolved))
        })?;
        self.push_pins();
        // Both flags, whichever moved: an agent reading the line wants the
        // state of the annotation, not the change that got it there.
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
        force: bool,
    ) -> Result<Vec<String>, String> {
        let art = self.art()?;
        let edit = prepare_block_replace(&art, uuid, block_id, new_text, policy)?;
        // Refused rather than written: an edit to a generated file is undone
        // by the next run of whatever generates it.
        if !force {
            if let Some(said) = std::fs::read_to_string(&edit.path)
                .ok()
                .and_then(|text| generated_note(&text))
            {
                return Err(format!(
                    "{} is generated — it says so: \"{said}\". An edit here lasts \
                     until whatever generates it runs again; fix that instead, or \
                     pass force to edit it anyway.",
                    edit.path.display()
                ));
            }
        }
        std::fs::write(&edit.path, &edit.content)
            .map_err(|err| format!("cannot write {}: {err}", edit.path.display()))?;
        // The anchors the rewrite dropped were dropped on purpose. What
        // pointed at them is not: it is about the document now, which is where
        // an annotation goes when the place it was about is gone.
        if let Some(sidecar_path) = sidecar_path(&art) {
            let _ = revise(&sidecar_path, |sidecar| {
                for record in &mut sidecar.annotations {
                    let lost = record
                        .labels()
                        .iter()
                        .any(|label| edit.dropped.iter().any(|gone| gone == label));
                    if lost {
                        record.location = tinymist_annos::TypstLocation::Document;
                    }
                }
                Ok(())
            });
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

    fn relocate(
        &self,
        render: &str,
        locations: &[tinymist_annos::HtmlLocation],
    ) -> Result<Vec<Option<tinymist_annos::HtmlLocation>>, String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let was = super::renders::get(&document, render)
            .ok_or("that rendering is no longer held; reload the page and try again")?;
        let now = super::renders::latest().ok_or("nothing has been rendered yet")?;
        let now_text = std::fs::read_to_string(&document)
            .map_err(|err| format!("cannot read {}: {err}", document.display()))?;
        // Only the document's own text is compared. An unsent annotation is
        // one the reader is still writing, and the page it is on is a page of
        // the document being served.
        let ctx = tinymist_annos::relocate::Between {
            was: &was.map,
            was_text: was.texts.first().map(String::as_str).unwrap_or_default(),
            now: &now,
            now_text: &now_text,
        };
        Ok(locations
            .iter()
            .map(|location| tinymist_annos::relocate::relocate(&ctx, location))
            .collect())
    }

    fn document(&self) -> Option<PathBuf> {
        document_path(&self.art().ok()?)
    }

    fn generated(&self) -> Option<String> {
        generated_note(&std::fs::read_to_string(self.document()?).ok()?)
    }

    fn status(&self) -> Result<serde_json::Value, String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let (ok, messages) = self.diagnostics();
        let world = art.world();
        // Every file the last compile read: the document, its imports, its data
        // files, its images. A change to any of them triggers a rebuild.
        let mut watched: Vec<String> = art
            .depended_files()
            .iter()
            .filter_map(|id| {
                let path = world.path_for_id(*id).ok()?.to_err().ok()?;
                Some(path.display().to_string())
            })
            .collect();
        watched.sort();
        watched.dedup();
        let text = std::fs::read_to_string(&document).unwrap_or_default();
        Ok(serde_json::json!({
            "document": document.display().to_string(),
            "compiles": ok,
            "diagnostics": messages,
            // The rendering the readers currently have. A caller that has just
            // written to the document waits for this to change.
            "render": super::renders::latest().map(|map| map.render.clone()),
            "revision": self.compile_revision(),
            "readers": crate::tool::serve::http::live_clients(),
            "generated": generated_note(&text),
            // Watched by the server; no tool call is needed to trigger a
            // rebuild after one of them changes.
            "watchedDependencies": watched,
        }))
    }

    fn render_snippet(&self, source: &str, format: &str) -> Result<serde_json::Value, String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        super::snippet::render(&document, source, format)
    }

    fn audit(&self) -> Result<serde_json::Value, String> {
        let art = self.art()?;
        let document = document_path(&art).ok_or("cannot determine the document path")?;
        let sidecar_path = tinymist_annos::sidecar_path(&document);
        let sidecar = read_sidecar(&sidecar_path);
        // Every file the compile read, not the document alone: an annotation
        // on a heading in an included file is anchored in that file, and
        // reading the document alone would report it as an annotation whose
        // anchor is gone.
        let files = compiled_files(&art);
        let mut names: Vec<String> = vec![];
        let mut per_file = vec![];
        for (path, source) in &files {
            let found = tinymist_annos::anchor::anchors_in(source);
            per_file.push(serde_json::json!({
                "file": path.display().to_string(),
                "anchors": found.len(),
            }));
            names.extend(found.iter().map(|anchor| anchor.name()));
        }
        let report = tinymist_annos::audit::audit(names.iter().map(String::as_str), &sidecar);
        let dangling = tinymist_annos::audit::dangling(&sidecar, &report);
        Ok(serde_json::json!({
            "document": document.display().to_string(),
            // Each file the document is made of, with how many anchors it
            // holds. The document itself is first.
            "files": per_file,
            "annotations": sidecar.annotations.len(),
            "anchors": names.len(),
            // Anchors nothing points at any more: the next collection removes
            // them, and they are not a problem.
            "unusedAnchors": report.unused,
            // Annotations naming an anchor the document does not have: these
            // are the ones nobody can see, and they are about the document
            // until an anchor comes back.
            "missingAnchors": report.missing,
            "orphaned": dangling
                .iter()
                .map(|(record, lost)| {
                    serde_json::json!({
                        "uuid": record.uuid,
                        "letter": record.letter,
                        "content": record.content,
                        "snapshot": record.snapshot,
                        "lostAnchors": lost,
                    })
                })
                .collect::<Vec<_>>(),
        }))
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
        let sidecar = sidecar_path(&art).ok_or("cannot determine the sidecar path")?;
        Ok(read_sidecar(&sidecar).annotations)
    }

    fn pins(&self) -> Vec<super::pins::HtmlPin> {
        match self.art() {
            Ok(art) => super::pins::html_pins(&art),
            Err(_) => vec![],
        }
    }



}

/// A conversion failure, in words for whoever asked.
pub fn describe(failure: &tinymist_annos::resolve::Failure) -> String {
    use tinymist_annos::resolve::Failure;
    match failure {
        Failure::NoSuchNode(uid) => format!("the rendering has nothing called {uid}"),
        Failure::WrongKind(uid) => format!("{uid} is not the kind of thing that can take that"),
        Failure::NoSource(uid) => format!(
            "{uid} is not part of this document: it was generated, or it came from a file that \
             is not being annotated"
        ),
        Failure::Lost => {
            "the text this pointed at has changed since the page was drawn".to_owned()
        }
        Failure::Unwritable => {
            "an anchor cannot be written there: it is inside a string, a comment or raw text"
                .to_owned()
        }
    }
}
