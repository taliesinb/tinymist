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
        }
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
    let pos = jump_from_cursor(doc, source, cursor)
        .into_iter()
        .next()
        .or_else(|| {
            (line.saturating_sub(MAX_ANCHOR_WALK_BACK)..=line)
                .rev()
                .find_map(try_line)
        })?;
    location_of(doc, pos)
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
          const h = 16;
          const rect = document.createElementNS(SVG_NS, "rect");
          rect.setAttribute("class", OVERLAY_CLASS);
          rect.setAttribute("x", 0);
          rect.setAttribute("y", loc.y - 12);
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
          const h = Math.max(r.height * 0.025, 8);
          const div = document.createElement("div");
          div.className = OVERLAY_CLASS;
          div.style.cssText =
            "position:absolute;pointer-events:none;z-index:2147483646;" +
            "background:rgba(229,83,75,0.25);border:1px solid rgba(229,83,75,0.9);" +
            "box-sizing:border-box";
          div.style.left = r.left + window.scrollX + "px";
          div.style.top =
            r.top + window.scrollY + (loc.y / loc.pageHeight) * r.height - h / 2 + "px";
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
      (lastData.cursor ? 1 : 0);
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
        render(JSON.parse(ev.data));
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
