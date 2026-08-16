//! A document as HTML: what the exporter drops put back, and every element
//! labelled with where in the source it came from.
//!
//! Rendering, not serving. An editor's previewer and a document server both
//! show the same page — this is where that page is made — and only the server
//! goes on to hang annotations off it. What the annotator adds is in
//! `tool/serve`; what it needs from a document is here.
//!
//! Typst's HTML export is incomplete: layout elements are dropped along with
//! everything inside them, so a diagram becomes an empty box. The document is
//! compiled through a wrapper that installs show rules recovering them
//! (`static/html/shims.typ`), which also means the compile's main file is the
//! wrapper rather than the document — hence [`doc_file`], which says where the
//! document went.

use std::sync::Arc;

use serde::Serialize;
use tinymist_project::LspCompiledArtifact;
use tinymist_std::typst::TypstDocument;
use typst::World;
use typst::syntax::{FileId, Source, Span};
use typst_html::{HtmlElement, HtmlNode};

use crate::tool::asset::{dev_asset_in, dev_asset_path};

/// An asset of the HTML client, from `src/static/html`.
fn asset(rel: &str, embedded: &'static str) -> String {
    dev_asset_in("html", rel, embedded)
}

/// The attribute carrying an element's source range, as `start:end` byte
/// offsets into the file being served.
pub const SRC_ATTR: &str = "data-typst-src";

/// The attribute marking a run that cannot take an annotation inside it: its
/// value is the offset just past the expression that produced it, which is
/// where an annotation on the whole thing anchors.
pub const ATOM_ATTR: &str = "data-typst-atom";

/// The attribute carrying the range of a run of *text*, which is what
/// annotations actually land on. It sits on the element that holds the run
/// where there is one, so the document's structure is left as the exporter
/// wrote it.
pub const TEXT_ATTR: &str = "data-typst-text";

/// The page that hosts an HTML-mode document: its own shell, its own script,
/// its own stylesheet. It shares nothing with the paged frontend.
pub fn shell_html() -> String {
    asset("shell.html", include_str!("../../static/html/shell.html"))
}

/// The HTML-mode client script, read from the source tree when there is one so
/// it can be edited without rebuilding.
pub fn client_js() -> String {
    asset("annotate.js", include_str!("../../static/html/annotate.js"))
}

/// The HTML-mode stylesheet.
pub fn client_css() -> String {
    asset("annotate.css", include_str!("../../static/html/annotate.css"))
}

/// The export shims: show rules that recover what Typst's HTML export drops.
/// Read from the source tree when there is one, so they can be edited live.
pub fn shims_typ() -> String {
    asset("shims.typ", include_str!("../../static/html/shims.typ"))
}

/// The source-tree path of the shims, for watching.
pub fn shims_path() -> std::path::PathBuf {
    dev_asset_path("html", "shims.typ")
}

/// Compiles the document with the shims installed as its default show rules.
///
/// Nothing is written beside the document and nothing wraps it: the rules are
/// evaluated once and put into the library this universe compiles against, so
/// the document is still the compile's main file and still the file every
/// source range refers to.
pub fn install_shims(verse: &mut tinymist_project::LspUniverse) -> Result<(), String> {
    use reflexo_typst::{Bytes, ShadowApi};

    // The rules are Typst, so they have to be somewhere the compiler can read
    // them — but somewhere that is not the user's project.
    verse
        .map_shadow_by_id(
            super::shims::shims_id(),
            Bytes::from_string(shims_typ()),
        )
        .map_err(|err| format!("cannot install the HTML shims: {err}"))?;

    let base = super::shims::base_library(verse.inputs().clone());
    let world = verse.snapshot();
    let library = super::shims::library_with_shims(&world, &base)?;
    verse.set_library(Some(std::sync::Arc::new(typst::utils::LazyHash::new(library))));
    Ok(())
}

/// The source-tree paths of the HTML-mode assets, for dev asset watching.
pub fn asset_paths() -> Vec<std::path::PathBuf> {
    ["shell.html", "annotate.js", "annotate.css", "shims.typ"]
        .iter()
        .map(|name| dev_asset_path("html", name))
        .collect()
}

/// The document as HTML, with the map that says what it was made of.
///
/// The rendering carries an id on everything that can be pointed at, and the
/// map says which stretch of which file each of those ids came from. A browser
/// can then name a place in what it can see, and the server can understand it
/// in terms of the source.
pub fn html_document_with_map(
    art: &LspCompiledArtifact,
) -> Result<(String, tinymist_annos::RenderMap), String> {
    let Some(doc) = art.success_doc() else {
        return Err("no document".into());
    };
    let TypstDocument::Html(doc) = &doc else {
        return Err("this document is not compiled for HTML; run with --format html".into());
    };
    let world = art.world();
    let main = world.main();
    let source = world
        .source(main)
        .map_err(|err| format!("cannot read the document's source: {err}"))?;

    let mut labelled = (**doc).clone();
    let mut mapper = super::map::Mapper::new(world, main, vec![super::shims::shims_id()]);
    label_within(labelled.root_mut(), &source, main, false, &mut mapper);
    let html = typst_html::html(&labelled, &typst_html::HtmlOptions::default())
        .map_err(|err| format!("cannot encode the document as HTML: {err:?}"))?;
    // Named after itself: a document that compiles to the same rendering keeps
    // the same name for it, so a page that reconnects after a restart is still
    // talking about something the server knows.
    let render = hash_of(html.as_bytes());
    Ok((html, mapper.finish(render, source.text())))
}

/// The document as HTML, with every element labelled with its source range.
pub fn html_document(art: &LspCompiledArtifact) -> Result<String, String> {
    let Some(doc) = art.success_doc() else {
        return Err("no document".into());
    };
    let TypstDocument::Html(doc) = &doc else {
        return Err("this document is not compiled for HTML; run with --format html".into());
    };
    let world = art.world();
    // The document, not the compile's main file: with the shims installed
    // those differ, and what is labelled is what the reader is looking at.
    let main = world.main();
    let source = world
        .source(main)
        .map_err(|err| format!("cannot read the document's source: {err}"))?;

    // Labelled in place on a copy: the document's parts are private, and its
    // introspector is not derived from the output, so it cannot be rebuilt from
    // pieces.
    let mut labelled = (**doc).clone();
    let mut mapper = super::map::Mapper::new(world, main, vec![super::shims::shims_id()]);
    label_within(labelled.root_mut(), &source, main, false, &mut mapper);
    typst_html::html(&labelled, &typst_html::HtmlOptions::default())
        .map_err(|err| format!("cannot encode the document as HTML: {err:?}"))
}

/// The document as the client wants it: its title, and the contents of its
/// body. The shell owns the page — its head, its overlay, its script — so the
/// document arrives as something to put inside, not as a page of its own.
#[derive(Debug, Clone, Serialize)]
pub struct HtmlFragment {
    /// The document's title, if it declared one.
    pub title: String,
    /// Everything between `<body>` and `</body>`.
    pub body: String,
}

/// Splits a rendered document into the parts the shell needs.
pub fn fragment(html: &str) -> HtmlFragment {
    let between = |open_tag: &str, close: &str| -> Option<String> {
        let open = html.find(open_tag)?;
        let start = html[open..].find('>')? + open + 1;
        let end = html[start..].find(close)? + start;
        Some(html[start..end].to_owned())
    };
    HtmlFragment {
        title: between("<title", "</title>").unwrap_or_default(),
        body: between("<body", "</body>").unwrap_or_else(|| html.to_owned()),
    }
}

/// `in_math` says whether this element is inside a `<math>`, where the
/// children of an element are characters rather than markup: a `<span>` under
/// an `<mi>` is not math to a browser, and the equation renders as body text.
/// Elements there still carry their range; only text is left alone, so an
/// equation is one atom to annotate rather than a run of words.
/// Returns the stretch of the document this element and its contents came
/// from, which is what an element with no source position of its own is given.
fn label_within(
    element: &mut HtmlElement,
    source: &Source,
    main: FileId,
    in_math: bool,
    mapper: &mut super::map::Mapper,
) -> Option<tinymist_annos::SrcRange> {
    let tag = element.tag.resolve().as_str().to_owned();
    // Everything that can be pointed at gets an id, whether or not it turned
    // out to have a source range: a reader can see it, so a reader can click
    // it, and the map says honestly whether anything is known about where it
    // came from.
    let uid = if matches!(tag.as_str(), "html" | "head" | "title" | "script" | "style") {
        None
    } else {
        let uid = mapper.uid();
        let display_block = element
            .attrs
            .get(typst_html::HtmlAttr::intern("display").unwrap_or_else(|_| typst_html::attr::id))
            .map(|value| value.as_str() == "block")
            .unwrap_or(false);
        mapper.node(&uid, super::map::kind_of(&tag, display_block), element.span);
        if let Ok(attr) = typst_html::HtmlAttr::intern(super::map::UID_ATTR) {
            element.attrs.push(attr, uid.clone());
        }
        Some(uid)
    };
    if let Some(range) = span_range(element.span, source, main) {
        if let Ok(attr) = typst_html::HtmlAttr::intern(SRC_ATTR) {
            element
                .attrs
                .push(attr, format!("{}:{}", range.start, range.end));
        }
    }
    let in_math = in_math || tag == "math";
    let opaque = matches!(tag.as_str(), "head" | "title" | "script" | "style");
    if opaque {
        return uid.as_ref().and_then(|uid| mapper.range_at(uid));
    }

    // An equation is one thing, not a run of words: its parts are operators
    // and identifiers whose separate positions mean nothing to a reader, and
    // labelling them only offers to annotate half an arrow. The `<math>`
    // element keeps its own range, and that is what an annotation on an
    // equation attaches to.
    if in_math {
        for child in element.children.make_mut() {
            if let HtmlNode::Element(child) = child {
                label_within(child, source, main, true, mapper);
            }
        }
        return uid.as_ref().and_then(|uid| mapper.range_at(uid));
    }

    // An element holding nothing but one run of text can say where that run
    // came from itself; a wrapper would only be somewhere to hang an attribute
    // that already has a home.
    if element.children.len() == 1 {
        if let HtmlNode::Text(text, span) = &element.children[0] {
            if let Some(range) = span_range(*span, source, main) {
                if let Some(uid) = &uid {
                    match atom_expression(source, &range) {
                        // Text a call produced: the call is what can be named.
                        Some(expr) => mapper.node_at(
                            uid,
                            tinymist_annos::NodeKind::Inline,
                            Some(tinymist_annos::SrcRange {
                                file: mapper.document_index(),
                                start: expr.start,
                                end: expr.end,
                            }),
                        ),
                        // Text the reader wrote, in an element that is only
                        // carrying its style: a bold run, a coloured word, the
                        // text of a heading. Its characters are addressable.
                        None => mapper.set_text(uid, text, *span),
                    }
                }
                let (name, value) = run_attr(source, &range);
                if let Ok(attr) = typst_html::HtmlAttr::intern(name) {
                    element.attrs.push(attr, value);
                }
            }
            return uid.as_ref().and_then(|uid| mapper.range_at(uid));
        }
    }

    // What the contents came from, for an element whose own span says nothing.
    let mut covered: Option<tinymist_annos::SrcRange> = None;
    let mut widen = |range: Option<tinymist_annos::SrcRange>| {
        let Some(range) = range else { return };
        if range.file != 0 {
            return;
        }
        covered = Some(match covered {
            Some(known) => tinymist_annos::SrcRange {
                file: 0,
                start: known.start.min(range.start),
                end: known.end.max(range.end),
            },
            None => range,
        });
    };
    for child in element.children.make_mut() {
        match child {
            HtmlNode::Element(child) => {
                widen(label_within(child, source, main, in_math, mapper))
            }
            // A run with siblings has nowhere of its own to keep its range,
            // so it gets a wrapper.
            HtmlNode::Text(text, span) => {
                let Some(range) = span_range(*span, source, main) else {
                    continue;
                };
                let (name, value) = run_attr(source, &range);
                let (Ok(tag), Ok(attr), Ok(uid_attr)) = (
                    typst_html::HtmlTag::intern("span"),
                    typst_html::HtmlAttr::intern(name),
                    typst_html::HtmlAttr::intern(super::map::UID_ATTR),
                ) else {
                    continue;
                };
                let run = mapper.uid();
                widen(Some(tinymist_annos::SrcRange {
                    file: mapper.document_index(),
                    start: range.start,
                    end: range.end,
                }));
                match atom_expression(source, &range) {
                    // Text a call produced: its characters cannot be addressed
                    // one by one, since a label cannot be written among them.
                    // What can be named is the call, so that is what the map
                    // records.
                    Some(expr) => mapper.node_at(
                        &run,
                        tinymist_annos::NodeKind::Inline,
                        Some(tinymist_annos::SrcRange {
                            file: mapper.document_index(),
                            start: expr.start,
                            end: expr.end,
                        }),
                    ),
                    None => mapper.text(&run, tinymist_annos::NodeKind::Text, text, *span),
                }
                let inner = HtmlNode::Text(text.clone(), *span);
                *child = HtmlNode::Element(
                    HtmlElement::new(tag)
                        .with_attr(attr, value)
                        .with_attr(uid_attr, run)
                        .with_children(std::iter::once(inner).collect())
                        .spanned(*span),
                );
            }
            // A drawing is not an element, so it cannot be given an attribute;
            // what it has is the id Typst assigns to the SVG, which is
            // otherwise only used when something links to it. An id already
            // there is a link destination, and is adopted rather than replaced.
            HtmlNode::Frame(frame) => {
                let uid = match &frame.id {
                    Some(id) => id.to_string(),
                    None => {
                        let uid = mapper.uid();
                        frame.id = Some(uid.as_str().into());
                        uid
                    }
                };
                // Where the picture came from, asked of the picture: the frame
                // itself is attributed to the shim that framed it, which is the
                // same place for every drawing in the document.
                let range = super::drawing::source_of(&frame.inner, source, main).map(|range| {
                    tinymist_annos::SrcRange {
                        file: mapper.document_index(),
                        start: range.start,
                        end: range.end,
                    }
                });
                mapper.node_at(&uid, tinymist_annos::NodeKind::Svg, range);
                widen(range);
            }
            _ => {}
        }
    }

    if let Some(uid) = &uid {
        if let Some(range) = covered {
            mapper.cover(uid, range);
        }
        return mapper.range_at(uid).or(covered);
    }
    covered
}

/// How a run says where it came from: its range, or — where a label cannot go
/// inside it — the end of the expression that produced it.
fn run_attr(source: &Source, range: &std::ops::Range<usize>) -> (&'static str, String) {
    match atom_end(source, range) {
        Some(end) => (ATOM_ATTR, end.to_string()),
        None => (TEXT_ATTR, format!("{}:{}", range.start, range.end)),
    }
}

/// Where a run of text can take a label beside it, and where it cannot.
///
/// Text written as markup can: a label goes next to the word it names. Text
/// that came out of code cannot — `#src("proof.rs")` renders its argument, and
/// a label written into that range would land inside the string and break the
/// call. What can be annotated there is the expression itself, so this returns
/// the offset just past it, and the run is marked as one thing rather than as
/// a row of words.
fn atom_end(source: &Source, range: &std::ops::Range<usize>) -> Option<usize> {
    atom_expression(source, range).map(|expr| expr.end)
}

/// The expression a run of text came out of, when it came out of one.
///
/// `atom_end` gives its end, which is where a label attaching to the whole
/// expression goes. The whole range is what the render map records, so that a
/// location can name the expression as one thing.
fn atom_expression(
    source: &Source,
    range: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    use typst::syntax::SyntaxKind;
    use typst_shim::syntax::LinkedNodeExt;

    // Asked at a boundary, the tree answers with whichever side it likes — the
    // closing paren of the call before this text as readily as the text — so
    // the question is asked from inside the run.
    let text = source.text();
    let mut at = range.start + (range.end - range.start) / 2;
    while at > range.start && !text.is_char_boundary(at) {
        at -= 1;
    }
    let root = typst::syntax::LinkedNode::new(source.root());
    let leaf = root.leaf_at_compat(at)?;
    if matches!(
        leaf.kind(),
        SyntaxKind::Text | SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::SmartQuote
    ) {
        return None;
    }
    // Out to the expression that markup contains: `#src("proof.rs")` as a
    // whole, not the string inside it.
    let mut cursor = leaf;
    loop {
        let parent = cursor.parent()?;
        if parent.kind() == SyntaxKind::Markup {
            return Some(cursor.range());
        }
        cursor = parent.clone();
    }
}

/// The byte range a span covers in the file being served, if it is from there.
fn span_range(span: Span, source: &Source, main: FileId) -> Option<std::ops::Range<usize>> {
    if span.id()? != main {
        return None;
    }
    typst_shim::syntax::source_range(source, span)
}

/// A drawing found in a rendered body: the source range it belongs to, and the
/// SVG itself.
#[derive(Debug, Clone)]
pub struct FramedDrawing {
    /// The source range of the innermost thing around it that came from the
    /// document.
    pub range: std::ops::Range<usize>,
    /// The drawing, as it stands in the page.
    pub svg: String,
}

/// Every framed drawing in a rendered body, with the piece of document it sits
/// inside.
///
/// Read out of the rendered text rather than out of the document tree. A framed
/// drawing is not an element there — it is a laid-out frame that only becomes
/// SVG when the page is written — so it cannot be labelled on the way past, and
/// what it belongs to has to be recovered here: the innermost enclosing element
/// that says where it came from, which the walk keeps on a stack.
pub fn framed_drawings(body: &str) -> Vec<FramedDrawing> {
    /// Tags that never close, and so never nest anything.
    const VOID: &[&str] = &[
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source",
        "track", "wbr",
    ];
    let mut found = Vec::new();
    let mut open_ranges: Vec<Option<std::ops::Range<usize>>> = Vec::new();
    let mut at = 0;
    while let Some(rel) = body[at..].find('<') {
        let open = at + rel;
        let rest = &body[open + 1..];
        if rest.starts_with('/') {
            open_ranges.pop();
            at = open + 1 + rest.find('>').map(|i| i + 1).unwrap_or(1);
            continue;
        }
        if rest.starts_with('!') {
            at = open + 1 + rest.find('>').map(|i| i + 1).unwrap_or(1);
            continue;
        }
        let Some(tag_end) = tag_end(body, open) else {
            break;
        };
        let tag: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if tag.is_empty() {
            at = open + 1;
            continue;
        }
        if tag == "svg" {
            // The SVG is the drawing; what it is a picture of is whatever the
            // document last opened around it.
            if let Some(range) = open_ranges.iter().rev().flatten().next().cloned() {
                if let Some(end) = closing_svg(body, tag_end) {
                    found.push(FramedDrawing {
                        range,
                        svg: body[open..end].to_owned(),
                    });
                    at = end;
                    continue;
                }
            }
        }
        let closes_itself = body[open..tag_end].ends_with("/>");
        if !closes_itself && !VOID.contains(&tag.as_str()) {
            open_ranges.push(attr_range(&body[open..tag_end]));
        }
        at = tag_end;
    }
    found
}

/// Where an open tag beginning at `open` ends, ignoring `>` inside attribute
/// values.
fn tag_end(body: &str, open: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut quote: Option<u8> = None;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match (quote, byte) {
            (Some(q), b) if *b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"') | (None, b'\'') => quote = Some(*byte),
            (None, b'>') => return Some(offset + 1),
            _ => {}
        }
    }
    None
}

/// The source range an open tag carries, if it carries one.
fn attr_range(open_tag: &str) -> Option<std::ops::Range<usize>> {
    let needle = format!("{SRC_ATTR}=\"");
    let at = open_tag.find(&needle)? + needle.len();
    let end = at + open_tag[at..].find('"')?;
    let (start, stop) = open_tag[at..end].split_once(':')?;
    Some(start.parse().ok()?..stop.parse().ok()?)
}

/// Where the `</svg>` that closes an SVG opened before `from` sits, counting
/// the ones nested inside it.
fn closing_svg(body: &str, from: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut at = from;
    loop {
        let open = body[at..].find("<svg").map(|i| at + i);
        let close = body[at..].find("</svg>").map(|i| at + i)?;
        match open {
            Some(open) if open < close => {
                depth += 1;
                at = open + 4;
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return Some(close + "</svg>".len());
                }
                at = close + "</svg>".len();
            }
        }
    }
}

/// What an HTML-mode server can answer.
pub trait HtmlBody: Send + Sync {
    /// The document as HTML, labelled with source ranges.
    fn document(&self) -> Result<String, String>;
    /// The document and the map of the rendering, which say together what the
    /// page is showing and what each part of it came from.
    fn rendering(&self) -> Result<(String, tinymist_annos::RenderMap), String> {
        Err("this server does not keep render maps".into())
    }
    /// The rendered body and the version it was rendered at: read from the
    /// site the server writes as it compiles, and made here only if the first
    /// compile has not landed yet.
    fn body(&self) -> (String, u64) {
        (String::new(), 0)
    }
}

/// Holds the last compiled artifact, so both answers come from one document.
pub struct ArtifactHtmlServer {
    /// The most recent successful compile.
    pub last_art: Arc<parking_lot::Mutex<Option<LspCompiledArtifact>>>,
}

impl HtmlBody for ArtifactHtmlServer {
    fn document(&self) -> Result<String, String> {
        let art = self.last_art.lock().clone().ok_or("nothing compiled yet")?;
        html_document(&art)
    }

    fn rendering(&self) -> Result<(String, tinymist_annos::RenderMap), String> {
        let art = self.last_art.lock().clone().ok_or("nothing compiled yet")?;
        html_document_with_map(&art)
    }

    fn body(&self) -> (String, u64) {
        let art = self.last_art.lock().clone();
        let Some(art) = art else {
            return (String::new(), 0);
        };
        // What was written when this document last compiled, if anything writes
        // to disk: a document server renders once for everyone who asks, while
        // an editor's previewer has one reader and renders on request.
        #[cfg(feature = "serve")]
        let cached = crate::tool::serve::site_cache().and_then(|cache| {
            let world = art.world();
            let path = world
                .path_for_id(world.main())
                .ok()
                .and_then(|path| path.to_err().ok())?;
            let body = cache.body(path.as_ref())?;
            Some((std::fs::read_to_string(&body.path).ok()?, body.version))
        });
        #[cfg(not(feature = "serve"))]
        let cached: Option<(String, u64)> = None;
        cached.unwrap_or_else(|| {
            let body = html_document(&art)
                .map(|html| fragment(&html).body)
                .unwrap_or_default();
            (body, 0)
        })
    }

}

/// A drawing's identity: enough of SHA-256 to never collide in a document,
/// short enough to read in a sidecar. What a capture is stored under, and how
/// an unchanged drawing is recognised as the one already kept.
/// document, short enough to read in a sidecar.
pub fn hash_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How big a drawing is on the page, in CSS pixels.
///
/// Best effort, and calibrated against the page rather than the file: the
/// exporter writes an SVG's size in points, a browser lays it out at 4/3 of
/// that, and the stylesheet only ever shrinks it to fit the column. So this is
/// what the reader sees when the column is wide enough, which is the usual case
/// and the only one the file itself can answer for.
pub fn pixel_size(svg: &str) -> Option<(u32, u32)> {
    let open = &svg[..svg.find('>')?];
    let attr = |name: &str| -> Option<f32> {
        let needle = format!(" {name}=\"");
        let at = open.find(&needle)? + needle.len();
        let value = &open[at..at + open[at..].find('"')?];
        css_px(value)
    };
    let sized = attr("width").zip(attr("height"));
    let (w, h) = match sized {
        Some(size) => size,
        // No size of its own: a `viewBox` is user units, which a browser treats
        // as pixels.
        None => {
            let needle = " viewBox=\"";
            let at = open.find(needle)? + needle.len();
            let value = &open[at..at + open[at..].find('"')?];
            let mut parts = value.split_whitespace().skip(2);
            (
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            )
        }
    };
    Some((w.round().max(0.0) as u32, h.round().max(0.0) as u32))
}

/// A CSS length in pixels, for the handful of units an exporter writes.
fn css_px(value: &str) -> Option<f32> {
    let value = value.trim();
    let (number, factor) = match value {
        _ if value.ends_with("pt") => (&value[..value.len() - 2], 4.0 / 3.0),
        _ if value.ends_with("px") => (&value[..value.len() - 2], 1.0),
        _ if value.ends_with("mm") => (&value[..value.len() - 2], 96.0 / 25.4),
        _ if value.ends_with("cm") => (&value[..value.len() - 2], 96.0 / 2.54),
        _ if value.ends_with("in") => (&value[..value.len() - 2], 96.0),
        // A bare number is user units, which are pixels.
        _ => (value, 1.0),
    };
    Some(number.trim().parse::<f32>().ok()? * factor)
}
