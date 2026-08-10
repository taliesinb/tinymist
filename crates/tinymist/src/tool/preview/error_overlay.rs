//! Injects compile diagnostics and a cursor indicator into the preview
//! frontend.
//!
//! When `preview.errorOverlay` or `preview.cursorIndicator` is enabled, the
//! preview http server exposes a `/dev/diagnostics` SSE endpoint and the
//! served frontend html gets a small script injected. On compile errors, the
//! script highlights the error locations on the last successful render and
//! shows the error messages in a floating panel. With the cursor indicator,
//! it additionally draws a small dot at the editor's cursor position.

use std::path::Path;

use reflexo::debug_loc::LspPosition;
use reflexo_typst::TypstDocument;
use serde::Serialize;
use tinymist_project::{LspCompiledArtifact, LspWorld};
use tinymist_query::jump_from_cursor;
use typst::diag::Severity;
use typst::introspection::PagedPosition;
use typst::syntax::{LinkedNode, Source, SyntaxKind};
use typst::World;
use typst_shim::syntax::LinkedNodeExt;

/// The sender half of a per-preview overlay channel.
pub type DiagTx = tokio::sync::watch::Sender<OverlayPayload>;
/// The receiver half of a per-preview overlay channel.
pub type DiagRx = tokio::sync::watch::Receiver<OverlayPayload>;

/// The maximum number of message lines shown in the floating panel.
const MAX_MESSAGE_LINES: usize = 10;
/// The maximum number of resolved error locations.
const MAX_LOCATIONS: usize = 4;
/// How many lines to walk backwards when the exact position does not resolve
/// on the last successful render.
const MAX_ANCHOR_WALK_BACK: usize = 100;

/// A source location resolved onto the last successful render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayLocation {
    /// The 1-based page number.
    page: usize,
    /// The x coordinate on the page, in pt.
    x: f64,
    /// The y coordinate on the page, in pt.
    y: f64,
    /// An optional end y coordinate, in pt. When present, the highlight is a
    /// band from `y` to `y_end` (bracketing a region that could not be
    /// resolved exactly, e.g. a code block that generates a figure) instead
    /// of a single line.
    y_end: Option<f64>,
    /// The page width, in pt.
    page_width: f64,
    /// The page height, in pt.
    page_height: f64,
}

/// The overlay payload sent over the SSE channel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayPayload {
    /// Whether the last compilation succeeded.
    pub ok: bool,
    /// The first [`MAX_MESSAGE_LINES`] lines of the error output.
    pub messages: Vec<String>,
    /// The resolved error locations.
    pub locations: Vec<OverlayLocation>,
    /// The extent of the paragraph-level block containing the editor's
    /// cursor, if it resolves.
    pub cursor: Option<BlockExtent>,
    /// Whether `invertColors: "smart"` is active: the frontend inverts colors
    /// only when the viewer is in dark mode but the document rendered light.
    pub smart_invert: bool,
    /// Whether the last successfully rendered document has a dark page
    /// background. `None` means unknown; the frontend keeps its previous
    /// value.
    pub doc_dark: Option<bool>,
    /// The annotations of the current main file, resolved onto the last
    /// successful render.
    pub annotations: Vec<super::annotations::AnnotationPin>,
}

/// The vertical extent of a block on a page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockExtent {
    /// The 1-based page number.
    page: usize,
    /// The left x coordinate of the block's content, in pt.
    x0: f64,
    /// The top y coordinate, in pt.
    y0: f64,
    /// The bottom y coordinate, in pt.
    y1: f64,
    /// The page width, in pt.
    page_width: f64,
    /// The page height, in pt.
    page_height: f64,
}

impl Default for OverlayPayload {
    fn default() -> Self {
        Self {
            ok: true,
            messages: vec![],
            locations: vec![],
            cursor: None,
            smart_invert: false,
            doc_dark: None,
            annotations: vec![],
        }
    }
}

/// Determines whether the rendered document's first page has a dark
/// background. `None` when there is no paged document or the page fill is not
/// a solid color.
pub fn doc_is_dark(art: &LspCompiledArtifact) -> Option<bool> {
    use typst::foundations::Smart;
    use typst::visualize::Paint;
    let TypstDocument::Paged(paged) = art.doc.as_ref()? else {
        return None;
    };
    let page = paged.pages().first()?;
    match &page.fill {
        // `auto` and `none` fills both render on a white ground in the
        // preview.
        Smart::Auto | Smart::Custom(None) => Some(false),
        Smart::Custom(Some(Paint::Solid(color))) => {
            let [r, g, b, _] = color.to_vec4();
            Some(0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5)
        }
        Smart::Custom(Some(_)) => None,
    }
}

/// Resolves a source position onto a rendered document. The position may sit
/// in freshly edited text whose spans don't exist in the rendered document;
/// in that case, walk backwards line by line to the nearest position that
/// does resolve. Note that `jump_from_cursor` only resolves on text leaves,
/// so we probe a few in-text positions per candidate line.
fn resolve_location(
    doc: &TypstDocument,
    source: &Source,
    cursor: usize,
    line: usize,
) -> Option<OverlayLocation> {
    let try_line = |line: usize| {
        let lines = source.lines();
        let start = lines.line_to_byte(line)?;
        let end = lines
            .line_to_byte(line + 1)
            .unwrap_or_else(|| source.text().len());
        let text = source.text().get(start..end)?;
        text.char_indices()
            .filter(|(_, ch)| ch.is_alphanumeric())
            .take(24)
            .find_map(|(off, ch)| {
                let cursor = start + off + ch.len_utf8();
                jump_from_cursor(doc, source, cursor).into_iter().next()
            })
    };
    // An exact resolution highlights that line alone.
    if let Some(pos) = jump_from_cursor(doc, source, cursor).into_iter().next() {
        return location_of(doc, pos);
    }

    // Otherwise bracket the unresolvable region (e.g. a code block that
    // generates a figure) between the nearest resolvable anchors above and
    // below, and highlight the whole band between them.
    let above = (line.saturating_sub(MAX_ANCHOR_WALK_BACK)..=line)
        .rev()
        .find_map(try_line);
    let below = ((line + 1)..=(line + MAX_ANCHOR_WALK_BACK)).find_map(try_line);
    match (above, below) {
        (Some(above), below) => {
            let loc = location_of(doc, above)?;
            let end = below
                .and_then(|below| location_of(doc, below))
                .filter(|end| end.page == loc.page && end.y > loc.y);
            Some(OverlayLocation {
                y_end: end.map(|end| end.y),
                ..loc
            })
        }
        (None, Some(below)) => location_of(doc, below),
        (None, None) => None,
    }
}

/// Converts a document position into an overlay location with page geometry.
fn location_of(doc: &TypstDocument, pos: PagedPosition) -> Option<OverlayLocation> {
    let page = pos.page.get();
    let TypstDocument::Paged(paged) = doc else {
        return None;
    };
    let size = paged.pages().get(page.checked_sub(1)?)?.frame.size();
    Some(OverlayLocation {
        page,
        x: pos.point.x.to_pt(),
        y: pos.point.y.to_pt(),
        y_end: None,
        page_width: size.x.to_pt(),
        page_height: size.y.to_pt(),
    })
}

/// Resolves the paragraph-level block containing the editor's cursor onto a
/// rendered document. Only cursors sitting on textual content resolve; a
/// cursor on e.g. a `#let` binding yields `None` (no highlight) rather than
/// an approximate position.
pub fn cursor_overlay(
    world: &LspWorld,
    doc: &TypstDocument,
    path: &Path,
    pos: LspPosition,
) -> Option<BlockExtent> {
    // Prefer the main file's id: `id_for_path` may mint a fresh
    // workspace-package id whose spans would never match the compiled
    // document's, and the focused file is the main file when the preview
    // follows the editor.
    let main = world.main();
    let main_path = world.path_for_id(main).ok().and_then(|p| p.to_err().ok());
    let id = if main_path.as_deref() == Some(path) {
        main
    } else {
        world.id_for_path(path)?
    };
    let source = world.source(id).ok()?;
    let cursor = source
        .lines()
        .line_column_to_byte(pos.line as usize, pos.character as usize)?;

    let root = LinkedNode::new(source.root());
    let leaf = root.leaf_at_compat(cursor)?;
    if !matches!(leaf.kind(), SyntaxKind::Text | SyntaxKind::MathText) {
        return None;
    }
    let point = jump_from_cursor(doc, &source, cursor).into_iter().next()?;
    let point = location_of(doc, point)?;

    // Find the ancestor that is a direct child of the root markup node.
    let mut node = leaf;
    while let Some(parent) = node.parent() {
        if parent.parent().is_none() {
            break;
        }
        node = parent.clone();
    }

    // Expand to the enclosing paragraph: standalone blocks (headings, list
    // items) stand for themselves, otherwise take the contiguous run of
    // siblings delimited by paragraph breaks or standalone blocks.
    let standalone = |kind: SyntaxKind| {
        matches!(
            kind,
            SyntaxKind::Heading | SyntaxKind::ListItem | SyntaxKind::EnumItem | SyntaxKind::TermItem
        )
    };
    let range = if standalone(node.kind()) {
        node.range()
    } else {
        let boundary =
            |n: &LinkedNode| n.kind() == SyntaxKind::Parbreak || standalone(n.kind());
        let children: Vec<LinkedNode> = root.children().collect();
        let idx = children
            .iter()
            .position(|child| child.offset() == node.offset())?;
        let mut lo = idx;
        while lo > 0 && !boundary(&children[lo - 1]) {
            lo -= 1;
        }
        let mut hi = idx;
        while hi + 1 < children.len() && !boundary(&children[hi + 1]) {
            hi += 1;
        }
        children[lo].range().start..children[hi].range().end
    };

    // Resolve the extent by probing textual positions near both ends of the
    // paragraph; `jump_from_cursor` only accepts text leaves, so non-text
    // probes are simply skipped.
    let text = source.text().get(range.clone())?;
    let probe = |cur: usize| jump_from_cursor(doc, &source, cur).into_iter().next();
    let start = text
        .char_indices()
        .filter(|(_, ch)| ch.is_alphanumeric())
        .take(40)
        .find_map(|(off, ch)| probe(range.start + off + ch.len_utf8()))?;
    let end = text
        .char_indices()
        .rev()
        .filter(|(_, ch)| ch.is_alphanumeric())
        .take(40)
        .find_map(|(off, ch)| probe(range.start + off + ch.len_utf8()))?;

    let start = location_of(doc, start)?;
    let end = location_of(doc, end)?;
    // If the paragraph spans pages, clip it to the cursor's page.
    let (y0, y1) = match (start.page == point.page, end.page == point.page) {
        (true, true) => (start.y, end.y),
        (true, false) => (start.y, start.page_height),
        (false, true) => (0.0, end.y),
        (false, false) => (0.0, point.page_height),
    };
    Some(BlockExtent {
        page: point.page,
        x0: start.x.min(end.x),
        // The resolved positions are text baselines; pad upward past the
        // ascent and downward past the descent.
        y0: (y0 - 12.0).max(0.0),
        y1: (y1 + 6.0).min(point.page_height),
        page_width: point.page_width,
        page_height: point.page_height,
    })
}

/// Renders the diagnostics of a compiled artifact as an overlay payload. The
/// `cursor` field is left empty; it is merged separately.
///
/// `last_edit` is the byte position of the most recent in-memory edit; when
/// no diagnostic resolves to a document position (e.g. an error raised during
/// deferred layout-time evaluation, whose call trace never reaches the
/// document), the edit that broke the compile is highlighted instead.
pub fn diagnostics_payload(
    art: &LspCompiledArtifact,
    last_edit: Option<(&Path, usize)>,
) -> OverlayPayload {
    let ok = art.doc.is_some();

    let mut messages = vec![];
    let mut locations = vec![];
    if !ok {
        let world = art.world();
        let success_doc = art.success_doc();

        let mut lines = 0usize;
        let mut truncated = 0usize;
        let errors = art
            .diagnostics()
            .filter(|diag| diag.severity == Severity::Error);
        for diag in errors {
            // Resolves a span to (source, byte offset, line, column).
            let locate = |id: Option<typst::syntax::FileId>,
                          range_of: &dyn Fn(&Source) -> Option<std::ops::Range<usize>>| {
                let id = id?;
                let source = world.source(id).ok()?;
                let range = range_of(&source)?;
                let line = source.lines().byte_to_line(range.start)?;
                let column = source.lines().byte_to_column(range.start)?;
                Some((source, range.start, line, column))
            };
            let format_at = |id: Option<typst::syntax::FileId>,
                             src: Option<&(Source, usize, usize, usize)>| {
                id.zip(src)
                    .map(|(id, (_, _, line, column))| {
                        format!(
                            " ({}:{}:{})",
                            id.vpath().get_with_slash(),
                            line + 1,
                            column + 1
                        )
                    })
                    .unwrap_or_default()
            };

            // The source location of the diagnostic itself.
            let src = locate(diag.span.id(), &|source| {
                typst_shim::syntax::source_range(source, diag.span)
            });
            // The trace frames, innermost first; the last one points at the
            // document-level code that triggered the failing call.
            let trace: Vec<_> = diag
                .trace
                .iter()
                .map(|point| {
                    let span = point.span;
                    (
                        point,
                        locate(span.id(), &|source| {
                            typst_shim::syntax::source_range(source, span)
                        }),
                    )
                })
                .collect();

            if lines >= MAX_MESSAGE_LINES {
                truncated += 1;
                continue;
            }
            let at = format_at(diag.span.id(), src.as_ref());
            messages.push(format!("error: {}{at}", diag.message));
            lines += 1;
            // Show the trace outermost first (document-level code) down to
            // the innermost frame (nearest the error). At most 4 frames: the
            // outer 3 and the innermost, eliding the middle.
            let total = trace.len();
            for (idx, (point, point_src)) in trace.iter().rev().enumerate() {
                if lines >= MAX_MESSAGE_LINES {
                    break;
                }
                if total > 4 && (3..total - 1).contains(&idx) {
                    if idx == 3 {
                        messages.push(format!("  … {} more frame(s) …", total - 4));
                        lines += 1;
                    }
                    continue;
                }
                let at = format_at(point.span.id(), point_src.as_ref());
                messages.push(format!("  {}{at}", point.v));
                lines += 1;
            }
            for hint in diag.hints.iter() {
                if lines >= MAX_MESSAGE_LINES {
                    break;
                }
                messages.push(format!("  hint: {}", hint.v));
                lines += 1;
            }

            // Resolve the error position against the last successful render.
            // Prefer the outermost trace frame: for an error inside a called
            // function (often in a library file), it points at the code in
            // the document itself, which is what the render can highlight.
            if locations.len() < MAX_LOCATIONS {
                if let Some(doc) = success_doc.as_ref() {
                    let candidates = trace
                        .iter()
                        .rev()
                        .filter_map(|(_, src)| src.as_ref())
                        .chain(src.as_ref());
                    let loc = candidates
                        .into_iter()
                        .find_map(|(source, cursor, line, _)| {
                            resolve_location(doc, source, *cursor, *line)
                        });
                    if let Some(loc) = loc {
                        locations.push(loc);
                    }
                }
            }
        }
        if truncated > 0 {
            messages.push(format!("… and {truncated} more error(s)"));
        }

        // No diagnostic resolved onto the render: fall back to the site of
        // the last edit, which is what most likely broke the compile.
        if locations.is_empty() {
            let fallback = || {
                let doc = success_doc.as_ref()?;
                let (path, offset) = last_edit?;
                let main = world.main();
                let main_path = world.path_for_id(main).ok().and_then(|p| p.to_err().ok());
                let id = if main_path.as_deref() == Some(path) {
                    main
                } else {
                    world.id_for_path(path)?
                };
                let source = world.source(id).ok()?;
                let cursor = offset.min(source.text().len());
                let line = source.lines().byte_to_line(cursor)?;
                resolve_location(doc, &source, cursor, line)
            };
            if let Some(loc) = fallback() {
                locations.push(loc);
            }
        }
    }

    OverlayPayload {
        ok,
        messages,
        locations,
        cursor: None,
        smart_invert: false,
        doc_dark: None,
        annotations: vec![],
    }
}

/// The script injected into the frontend html when the error overlay or the
/// cursor indicator is enabled.
pub const ERROR_OVERLAY_JS: &str = r#"
(() => {
  const PANEL_ID = "tinymist-error-panel";
  const OVERLAY_CLASS = "tinymist-error-overlay";
  const SVG_NS = "http://www.w3.org/2000/svg";
  const clear = () => {
    const panel = document.getElementById(PANEL_ID);
    if (panel) panel.remove();
    document.querySelectorAll("." + OVERLAY_CLASS).forEach((el) => el.remove());
  };
  const findPages = () => {
    let pages = document.querySelectorAll("g.typst-page");
    if (!pages.length) pages = document.querySelectorAll(".typst-page");
    return pages;
  };
  let lastData = null;
  let lastApplied = 0;
  let lastPageCount = 0;
  let lastScrollSig = null;
  // invertColors: "smart" — invert only when the viewer prefers dark but the
  // document rendered a light page (i.e. it ignored any dark theme inputs).
  // invert(1) hue-rotate(180deg) is an involution, so images and our own
  // overlay marks apply it a second time to restore their true colors.
  const SMART_INVERT_ID = "tinymist-smart-invert";
  const darkMedia = window.matchMedia("(prefers-color-scheme: dark)");
  let smartState = { enabled: false, docDark: null };
  const applySmartInvert = () => {
    const on =
      smartState.enabled && darkMedia.matches && smartState.docDark === false;
    let style = document.getElementById(SMART_INVERT_ID);
    if (on && !style) {
      style = document.createElement("style");
      style.id = SMART_INVERT_ID;
      // The page ground (`.typst-page-inner`) is a sibling of the page
      // groups, so the filter goes on the whole document svg.
      style.textContent =
        "svg.typst-doc { filter: invert(1) hue-rotate(180deg); }\n" +
        "svg.typst-doc image, svg.typst-doc ." + OVERLAY_CLASS +
        " { filter: invert(1) hue-rotate(180deg); }";
      document.head.appendChild(style);
    } else if (!on && style) {
      style.remove();
    }
  };
  darkMedia.addEventListener("change", applySmartInvert);
  const updateSmartInvert = (data) => {
    smartState.enabled = !!data.smartInvert;
    if (data.docDark != null) smartState.docDark = data.docDark;
    applySmartInvert();
  };
  // Highlight the paragraph containing the editor's cursor: a soft tint and
  // a left accent bar over its vertical extent.
  // Page groups use pt coordinates with the origin at the page's top-left,
  // so server-resolved positions can be used directly.
  const drawCursorBlock = (pages, b) => {
    const page = pages[b.page - 1];
    if (!page || !(page instanceof SVGGraphicsElement)) return;
    const bar = document.createElementNS(SVG_NS, "rect");
    bar.setAttribute("class", OVERLAY_CLASS);
    bar.setAttribute("x", Math.max(b.x0 - 10, 0));
    bar.setAttribute("width", 3.5);
    bar.setAttribute("y", b.y0);
    bar.setAttribute("height", Math.max(b.y1 - b.y0, 8));
    bar.setAttribute("rx", 1.75);
    bar.setAttribute("fill", "rgba(64,156,255,0.65)");
    bar.setAttribute("pointer-events", "none");
    page.appendChild(bar);
    lastApplied += 1;
  };
  // --- annotations: comments anchored to <A-XXXX> labels ---
  // A floating HTML box (compose or view) lives outside the svg so renderer
  // redraws can't wipe it while the user is typing.
  // While Alt is held, the preview becomes an annotation surface: text
  // caret, and the renderer's hover-highlight and click-splash are muted by
  // disabling pointer events inside the document svg.
  const ALT_STYLE_ID = "tinymist-alt-mode";
  const setAltMode = (on) => {
    let style = document.getElementById(ALT_STYLE_ID);
    if (on && !style) {
      style = document.createElement("style");
      style.id = ALT_STYLE_ID;
      style.textContent =
        "svg.typst-doc, svg.typst-doc * { cursor: text !important; }\n" +
        "svg.typst-doc * { pointer-events: none !important; }";
      document.head.appendChild(style);
    } else if (!on && style) {
      style.remove();
    }
  };
  window.addEventListener("keydown", (e) => {
    if (e.key === "Alt") setAltMode(true);
  });
  window.addEventListener("keyup", (e) => {
    if (e.key === "Alt") setAltMode(false);
  });
  window.addEventListener("blur", () => setAltMode(false));
  const ANNOT_BOX_ID = "tinymist-annot-box";
  let annotEscHandler = null;
  const closeAnnotBox = () => {
    const box = document.getElementById(ANNOT_BOX_ID);
    if (box) box.remove();
    if (annotEscHandler) {
      document.removeEventListener("keydown", annotEscHandler, true);
      annotEscHandler = null;
    }
  };
  const annotBox = (clientX, clientY) => {
    closeAnnotBox();
    const box = document.createElement("div");
    box.id = ANNOT_BOX_ID;
    box.style.cssText =
      "position:fixed;z-index:2147483647;background:#2b2b2b;color:#eee;" +
      "border:1px solid #f5a623;border-radius:6px;padding:8px;width:260px;" +
      "font:13px/1.4 system-ui,sans-serif;box-shadow:0 4px 16px rgba(0,0,0,0.4)";
    box.style.left = Math.max(Math.min(clientX, window.innerWidth - 280), 4) + "px";
    box.style.top = Math.max(Math.min(clientY + 8, window.innerHeight - 170), 4) + "px";
    // Keep keystrokes inside the box: the preview binds document-level
    // shortcuts (h/j/k scrolling etc.) that must not fire while typing.
    for (const type of ["keydown", "keyup", "keypress"]) {
      box.addEventListener(type, (e) => e.stopPropagation());
    }
    annotEscHandler = (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        closeAnnotBox();
      }
    };
    document.addEventListener("keydown", annotEscHandler, true);
    document.body.appendChild(box);
    return box;
  };
  const post = (path, payload) =>
    fetch(path, { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .then((r) => {
        if (!r.ok) console.warn("tinymist annotation:", r.error);
      })
      .catch((e) => console.warn("tinymist annotation:", e));
  const showAnnot = (pin, x, y) => {
    const box = annotBox(x, y);
    const text = document.createElement("div");
    text.style.cssText = "white-space:pre-wrap;margin-bottom:6px";
    text.textContent = pin.text;
    const meta = document.createElement("div");
    meta.style.cssText = "color:#999;font-size:11px;margin-bottom:6px";
    const when = pin.created ? new Date(pin.created * 1000).toLocaleString() : "";
    meta.textContent =
      pin.id + (when ? " · " + when : "") + (pin.completed ? " · completed" : "");
    const del = document.createElement("button");
    del.textContent = "Delete";
    del.style.cssText =
      "background:#5a2a2a;color:#ffb4b4;border:1px solid #a33;border-radius:4px;" +
      "padding:2px 10px;cursor:pointer";
    del.onclick = () => {
      post("/dev/annotate/delete", { id: pin.id });
      closeAnnotBox();
    };
    const close = document.createElement("button");
    close.textContent = "Close";
    close.style.cssText =
      "background:#333;color:#ddd;border:1px solid #555;border-radius:4px;" +
      "padding:2px 10px;cursor:pointer;margin-left:8px";
    close.onclick = closeAnnotBox;
    box.append(text, meta, del, close);
  };
  const composeAnnot = (pageNo, px, py, clientX, clientY) => {
    const box = annotBox(clientX, clientY);
    // The whole box is the text field; save/cancel float in its bottom-left.
    box.style.padding = "0";
    const ta = document.createElement("textarea");
    ta.rows = 3;
    ta.placeholder = "Comment…  (⇧⏎ save · esc cancel)";
    ta.style.cssText =
      "display:block;width:100%;box-sizing:border-box;background:transparent;" +
      "color:#eee;border:none;outline:none;resize:none;" +
      "padding:8px 8px 24px;font:13px/1.4 system-ui,sans-serif";
    const doSave = () => {
      const text = ta.value.trim();
      if (text) {
        // Stamp a short random id (16 bits of entropy); the server falls
        // back to its own if this one is taken.
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const id =
          "A-" +
          Array.from(rand, (b) =>
            b.toString(16).padStart(2, "0").toUpperCase(),
          ).join("");
        post("/dev/annotate", { id, page: pageNo, x: px, y: py, text });
      }
      closeAnnotBox();
    };
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        doSave();
      } else if (e.key === "Escape") {
        closeAnnotBox();
      }
    });
    const strip = document.createElement("div");
    strip.style.cssText =
      "position:absolute;left:8px;bottom:5px;display:flex;gap:10px;" +
      "font:12px system-ui,sans-serif";
    const save = document.createElement("button");
    save.textContent = "save";
    save.style.cssText =
      "background:none;border:none;padding:0;cursor:pointer;" +
      "color:rgb(123,216,143);font:inherit";
    save.onclick = doSave;
    const cancel = document.createElement("button");
    cancel.textContent = "cancel";
    cancel.style.cssText =
      "background:none;border:none;padding:0;cursor:pointer;" +
      "color:rgb(150,150,150);font:inherit";
    cancel.onclick = closeAnnotBox;
    strip.append(save, cancel);
    box.append(ta, strip);
    ta.focus();
  };
  const drawAnnotations = (pages, list) => {
    list.forEach((pin, idx) => {
      const page = pages[pin.page - 1];
      if (!page || !(page instanceof SVGGraphicsElement)) return;
      const g = document.createElementNS(SVG_NS, "g");
      g.setAttribute("class", OVERLAY_CLASS);
      g.style.cursor = "pointer";
      const cx = pin.pageWidth - 16;
      const cy = pin.y - 4;
      const done = !!pin.completed;
      const line = document.createElementNS(SVG_NS, "line");
      line.setAttribute("x1", pin.x);
      line.setAttribute("y1", pin.y - 4);
      line.setAttribute("x2", cx - 8);
      line.setAttribute("y2", cy);
      line.setAttribute(
        "stroke",
        done ? "rgba(150,150,150,0.35)" : "rgba(245,166,35,0.35)",
      );
      line.setAttribute("stroke-dasharray", "2,2");
      const c = document.createElementNS(SVG_NS, "circle");
      c.setAttribute("cx", cx);
      c.setAttribute("cy", cy);
      c.setAttribute("r", 8);
      c.setAttribute("fill", done ? "rgb(150,150,150)" : "rgb(245,166,35)");
      c.setAttribute("stroke", done ? "rgb(90,90,90)" : "rgb(138,90,0)");
      c.setAttribute("stroke-width", "0.8");
      const t = document.createElementNS(SVG_NS, "text");
      t.setAttribute("x", cx);
      t.setAttribute("y", cy + 2.6);
      t.setAttribute("text-anchor", "middle");
      t.setAttribute("font-size", "8");
      t.setAttribute("font-family", "system-ui,sans-serif");
      t.setAttribute("fill", "white");
      t.textContent = String(idx + 1);
      g.append(line, c, t);
      g.addEventListener("click", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        showAnnot(pin, ev.clientX, ev.clientY);
      });
      page.appendChild(g);
      lastApplied += 1;
    });
  };
  document.addEventListener(
    "click",
    (ev) => {
      if (!ev.altKey) return;
      const pages = Array.from(findPages());
      let el =
        ev.target && ev.target.closest
          ? ev.target.closest("g.typst-page, .typst-page")
          : null;
      if (!(el instanceof SVGGraphicsElement)) {
        // Alt-mode disables pointer events inside the svg, so the click
        // target is the svg root; find the page geometrically. The page
        // group's bbox only covers rendered content, so test the click in
        // page-local coordinates against the declared page size instead.
        el =
          pages.find((page) => {
            const pageCtm = page.getScreenCTM && page.getScreenCTM();
            if (!pageCtm) return false;
            const p = new DOMPoint(ev.clientX, ev.clientY).matrixTransform(
              pageCtm.inverse(),
            );
            const w = parseFloat((page.dataset || {}).pageWidth || "0");
            const h = parseFloat((page.dataset || {}).pageHeight || "0");
            return w > 0 && h > 0 && p.x >= 0 && p.x <= w && p.y >= 0 && p.y <= h;
          }) || null;
      }
      if (!(el instanceof SVGGraphicsElement)) return;
      const pageNo = pages.indexOf(el) + 1;
      const ctm = el.getScreenCTM();
      if (!pageNo || !ctm) return;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      const pt = new DOMPoint(ev.clientX, ev.clientY).matrixTransform(ctm.inverse());
      composeAnnot(pageNo, pt.x, pt.y, ev.clientX, ev.clientY);
    },
    true,
  );
  const render = (data) => {
    lastData = data;
    clear();
    lastApplied = 0;
    const pages = findPages();
    lastPageCount = pages.length;
    if (data.cursor) {
      try {
        drawCursorBlock(pages, data.cursor);
      } catch (e) {
        console.warn("tinymist overlay:", e);
      }
    }
    try {
      drawAnnotations(pages, data.annotations || []);
    } catch (e) {
      console.warn("tinymist overlay:", e);
    }
    if (data.ok) {
      lastScrollSig = null;
      return;
    }
    let firstMark = null;
    const panel = document.createElement("div");
    panel.id = PANEL_ID;
    panel.style.cssText =
      "position:fixed;left:0;right:0;bottom:0;z-index:2147483647;" +
      "background:rgba(46,16,16,0.94);color:#ffb4b4;" +
      "font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;" +
      "padding:8px 14px;white-space:pre-wrap;max-height:40vh;overflow-y:auto;" +
      "border-top:2px solid #e5534b;box-sizing:border-box";
    panel.textContent = (data.messages || []).join("\n");
    document.body.appendChild(panel);
    for (const loc of data.locations || []) {
      const page = pages[loc.page - 1];
      if (!page) continue;
      try {
        if (page instanceof SVGGraphicsElement) {
          const y0 = loc.y - 12;
          const y1 = loc.yEnd != null ? loc.yEnd - 12 : loc.y + 4;
          const h = Math.max(y1 - y0, 8);
          const rect = document.createElementNS(SVG_NS, "rect");
          rect.setAttribute("class", OVERLAY_CLASS);
          rect.setAttribute("x", 0);
          rect.setAttribute("y", y0);
          rect.setAttribute("width", loc.pageWidth);
          rect.setAttribute("height", h);
          rect.setAttribute("fill", "rgba(229,83,75,0.25)");
          rect.setAttribute("stroke", "rgba(229,83,75,0.9)");
          rect.setAttribute("stroke-width", "1");
          rect.setAttribute("pointer-events", "none");
          page.appendChild(rect);
          lastApplied += 1;
          if (!firstMark) firstMark = rect;
        } else {
          const r = page.getBoundingClientRect();
          const span = loc.yEnd != null ? (loc.yEnd - loc.y) / loc.pageHeight : 0.02;
          const h = Math.max(r.height * Math.max(span, 0.02), 8);
          const div = document.createElement("div");
          div.className = OVERLAY_CLASS;
          div.style.cssText =
            "position:absolute;pointer-events:none;z-index:2147483646;" +
            "background:rgba(229,83,75,0.25);border:1px solid rgba(229,83,75,0.9);" +
            "box-sizing:border-box";
          div.style.left = r.left + window.scrollX + "px";
          div.style.top =
            r.top + window.scrollY + ((loc.y - 12) / loc.pageHeight) * r.height + "px";
          div.style.width = r.width + "px";
          div.style.height = h + "px";
          document.body.appendChild(div);
          lastApplied += 1;
          if (!firstMark) firstMark = div;
        }
      } catch (e) {
        console.warn("tinymist overlay:", e);
      }
    }
    // Bring the highlight into view once per distinct error, so the user
    // sees where the failure is even when it is off-screen or behind the
    // message panel.
    const sig = JSON.stringify([data.messages, data.locations]);
    if (firstMark && sig !== lastScrollSig) {
      lastScrollSig = sig;
      try {
        firstMark.scrollIntoView({ block: "center", behavior: "smooth" });
      } catch (e) {
        console.warn("tinymist overlay:", e);
      }
    }
  };
  // The renderer may redraw the document at any time (incremental renders,
  // scrolls, follow-cursor jumps), which either removes our marks or paints
  // content after them — and in SVG, later siblings paint on top. Keep the
  // marks last in paint order and re-apply them when wiped.
  let reapplyTimer = null;
  const ensure = () => {
    if (!lastData) return;
    const els = document.querySelectorAll("." + OVERLAY_CLASS);
    const ours = (el) => el.classList && el.classList.contains(OVERLAY_CLASS);
    els.forEach((el) => {
      // Move below-content elements back to the top of the paint order, but
      // never leapfrog our own elements (that would churn forever).
      let sib = el.nextSibling;
      while (sib && ours(sib)) sib = sib.nextSibling;
      if (el.parentNode && sib) el.parentNode.appendChild(el);
    });
    const want =
      (lastData.ok ? 0 : (lastData.locations || []).length) +
      (lastData.cursor ? 1 : 0) +
      (lastData.annotations || []).length;
    const wiped =
      els.length < lastApplied || (!lastData.ok && !document.getElementById(PANEL_ID));
    const morePages = lastApplied < want && findPages().length !== lastPageCount;
    if (!wiped && !morePages) return;
    if (reapplyTimer) clearTimeout(reapplyTimer);
    reapplyTimer = setTimeout(() => render(lastData), 150);
  };
  new MutationObserver(ensure).observe(document.documentElement, {
    childList: true,
    subtree: true,
  });
  setInterval(ensure, 1000);
  const connect = () => {
    const es = new EventSource("/dev/diagnostics");
    es.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data);
        updateSmartInvert(data);
        render(data);
      } catch (e) {
        console.warn("tinymist overlay:", e);
      }
    };
    es.onerror = () => {
      es.close();
      setTimeout(connect, 2000);
    };
  };
  connect();
})();
"#;
