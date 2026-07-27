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
use typst::syntax::{LinkedNode, Source};
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
    /// The editor's cursor position.
    pub cursor: Option<CursorOverlay>,
}

/// The cursor indicator payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorOverlay {
    /// The exact cursor location.
    point: OverlayLocation,
    /// The vertical extent of the enclosing top-level block (e.g. the
    /// paragraph containing the cursor).
    block: Option<BlockExtent>,
}

/// The vertical extent of a block on a page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockExtent {
    /// The 1-based page number.
    page: usize,
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

/// Resolves the editor's cursor position onto a rendered document, together
/// with the extent of its enclosing top-level block.
pub fn cursor_overlay(
    world: &LspWorld,
    doc: &TypstDocument,
    path: &Path,
    pos: LspPosition,
) -> Option<CursorOverlay> {
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
    let point = resolve_location(doc, &source, cursor, pos.line as usize)?;

    // Find the enclosing top-level block: the ancestor whose parent is the
    // root markup node.
    let block = (|| {
        let root = LinkedNode::new(source.root());
        let leaf = root.leaf_at_compat(cursor)?;
        let mut node = leaf;
        while let Some(parent) = node.parent() {
            if parent.parent().is_none() {
                break;
            }
            node = parent.clone();
        }
        let range = node.range();
        let text = source.text().get(range.clone())?;

        // Probe in-text positions near both ends of the block.
        let probe = |cur: usize| jump_from_cursor(doc, &source, cur).into_iter().next();
        let start = text
            .char_indices()
            .filter(|(_, ch)| ch.is_alphanumeric())
            .take(24)
            .find_map(|(off, ch)| probe(range.start + off + ch.len_utf8()))?;
        let end = text
            .char_indices()
            .rev()
            .filter(|(_, ch)| ch.is_alphanumeric())
            .take(24)
            .find_map(|(off, ch)| probe(range.start + off + ch.len_utf8()))?;

        let start = location_of(doc, start)?;
        let end = location_of(doc, end)?;
        // If the block spans pages, clip it to the cursor's page.
        let (y0, y1) = match (start.page == point.page, end.page == point.page) {
            (true, true) => (start.y, end.y),
            (true, false) => (start.y, start.page_height),
            (false, true) => (0.0, end.y),
            (false, false) => (0.0, point.page_height),
        };
        Some(BlockExtent {
            page: point.page,
            y0: (y0 - 4.0).max(0.0),
            // The resolved positions are element origins; pad one text line.
            y1: (y1 + 14.0).min(point.page_height),
            page_width: point.page_width,
            page_height: point.page_height,
        })
    })();

    Some(CursorOverlay { point, block })
}

/// Renders the diagnostics of a compiled artifact as an overlay payload. The
/// `cursor` field is left empty; it is merged separately.
pub fn diagnostics_payload(art: &LspCompiledArtifact) -> OverlayPayload {
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
            // Resolve the source location of the diagnostic, for both the
            // message and the overlay position.
            let src = diag.span.id().and_then(|id| {
                let source = world.source(id).ok()?;
                let range = typst_shim::syntax::source_range(&source, diag.span)?;
                let line = source.lines().byte_to_line(range.start)?;
                let column = source.lines().byte_to_column(range.start)?;
                Some((source, range.start, line, column))
            });

            if lines >= MAX_MESSAGE_LINES {
                truncated += 1;
                continue;
            }
            let at = diag
                .span
                .id()
                .zip(src.as_ref())
                .map(|(id, (_, _, line, column))| {
                    format!(
                        " ({}:{}:{})",
                        id.vpath().get_with_slash(),
                        line + 1,
                        column + 1
                    )
                })
                .unwrap_or_default();
            messages.push(format!("error: {}{at}", diag.message));
            lines += 1;
            for hint in diag.hints.iter() {
                if lines >= MAX_MESSAGE_LINES {
                    break;
                }
                messages.push(format!("  hint: {}", hint.v));
                lines += 1;
            }

            // Resolve the error position against the last successful render.
            if locations.len() < MAX_LOCATIONS {
                if let Some((doc, (source, cursor, line, _))) =
                    success_doc.as_ref().zip(src.as_ref())
                {
                    if let Some(loc) = resolve_location(doc, source, *cursor, *line) {
                        locations.push(loc);
                    }
                }
            }
        }
        if truncated > 0 {
            messages.push(format!("… and {truncated} more error(s)"));
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
  const CURSOR_ID = "tinymist-cursor-mark";
  const BLOCK_ID = "tinymist-cursor-block";
  let lastData = null;
  let lastApplied = 0;
  let lastPageCount = 0;
  let lastScrollSig = null;
  // The cursor marker and block highlight are persistent singletons that are
  // moved (with a CSS transition) rather than recreated, so they glide to the
  // new position as the editor cursor moves.
  const updateCursor = (pages, cur) => {
    const gEl = (id) => document.getElementById(id);
    if (!cur) {
      if (gEl(CURSOR_ID)) gEl(CURSOR_ID).remove();
      if (gEl(BLOCK_ID)) gEl(BLOCK_ID).remove();
      return;
    }
    const loc = cur.point;
    const page = pages[loc.page - 1];
    if (!page || !(page instanceof SVGGraphicsElement)) return;
    const bbox = page.getBBox();
    const k = bbox.height / loc.pageHeight; // svg units per pt

    // Block highlight: a soft tint plus a left accent bar.
    if (cur.block) {
      const b = cur.block;
      const bpage = pages[b.page - 1] || page;
      let blockG = gEl(BLOCK_ID);
      if (blockG && blockG.parentNode !== bpage) {
        blockG.remove();
        blockG = null;
      }
      if (!blockG) {
        blockG = document.createElementNS(SVG_NS, "g");
        blockG.setAttribute("id", BLOCK_ID);
        blockG.setAttribute("pointer-events", "none");
        const tint = document.createElementNS(SVG_NS, "rect");
        tint.setAttribute("fill", "rgba(64,156,255,0.06)");
        const bar = document.createElementNS(SVG_NS, "rect");
        bar.setAttribute("fill", "rgba(64,156,255,0.55)");
        for (const el of [tint, bar]) {
          el.style.transition = "y 0.2s ease, height 0.2s ease";
          blockG.appendChild(el);
        }
        bpage.appendChild(blockG);
      }
      const bb = bpage.getBBox();
      const bk = bb.height / b.pageHeight;
      const [tint, bar] = blockG.children;
      const y = bb.y + b.y0 * bk;
      const h = Math.max((b.y1 - b.y0) * bk, 8);
      tint.setAttribute("x", bb.x);
      tint.setAttribute("width", bb.width);
      tint.setAttribute("y", y);
      tint.setAttribute("height", h);
      bar.setAttribute("x", bb.x);
      bar.setAttribute("width", Math.max(3 * bk, 3));
      bar.setAttribute("y", y);
      bar.setAttribute("height", h);
    } else if (gEl(BLOCK_ID)) {
      gEl(BLOCK_ID).remove();
    }

    // Crosshair marker: a ring with ticks, moved via an animated transform.
    let mark = gEl(CURSOR_ID);
    if (mark && mark.parentNode !== page) {
      mark.remove();
      mark = null;
    }
    if (!mark) {
      mark = document.createElementNS(SVG_NS, "g");
      mark.setAttribute("id", CURSOR_ID);
      mark.setAttribute("pointer-events", "none");
      const shapes = [
        ["circle", { r: 6, fill: "none", stroke: "rgba(64,156,255,0.9)", "stroke-width": 1.5 }],
        ["circle", { r: 1.6, fill: "rgba(64,156,255,0.9)" }],
        ["line", { x1: -12, x2: -7, y1: 0, y2: 0 }],
        ["line", { x1: 7, x2: 12, y1: 0, y2: 0 }],
        ["line", { y1: -12, y2: -7, x1: 0, x2: 0 }],
        ["line", { y1: 7, y2: 12, x1: 0, x2: 0 }],
      ];
      for (const [tag, attrs] of shapes) {
        const el = document.createElementNS(SVG_NS, tag);
        for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value);
        if (tag === "line") {
          el.setAttribute("stroke", "rgba(64,156,255,0.9)");
          el.setAttribute("stroke-width", 1.5);
        }
        mark.appendChild(el);
      }
      mark.style.transition = "transform 0.25s ease";
      page.appendChild(mark);
    }
    const x = bbox.x + (loc.x / loc.pageWidth) * bbox.width;
    const y = bbox.y + (loc.y / loc.pageHeight) * bbox.height + 6 * k;
    mark.style.transform = `translate(${x}px, ${y}px) scale(${Math.max(k, 0.5)})`;
  };
  const render = (data) => {
    lastData = data;
    clear();
    lastApplied = 0;
    const pages = findPages();
    lastPageCount = pages.length;
    try {
      updateCursor(pages, data.cursor);
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
          const bbox = page.getBBox();
          const h = Math.max(bbox.height * 0.025, 8);
          const rect = document.createElementNS(SVG_NS, "rect");
          rect.setAttribute("class", OVERLAY_CLASS);
          rect.setAttribute("x", bbox.x);
          rect.setAttribute("y", bbox.y + (loc.y / loc.pageHeight) * bbox.height - h / 2);
          rect.setAttribute("width", bbox.width);
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
    const els = document.querySelectorAll(
      "." + OVERLAY_CLASS + ", #" + CURSOR_ID + ", #" + BLOCK_ID
    );
    const ours = (el) =>
      el.id === CURSOR_ID ||
      el.id === BLOCK_ID ||
      (el.classList && el.classList.contains(OVERLAY_CLASS));
    els.forEach((el) => {
      // Move below-content elements back to the top of the paint order, but
      // never leapfrog our own elements (that would churn forever).
      let sib = el.nextSibling;
      while (sib && ours(sib)) sib = sib.nextSibling;
      if (el.parentNode && sib) el.parentNode.appendChild(el);
    });
    const errMarks = document.querySelectorAll("." + OVERLAY_CLASS).length;
    const wiped =
      errMarks < lastApplied ||
      (!lastData.ok && !document.getElementById(PANEL_ID)) ||
      (lastData.cursor && !document.getElementById(CURSOR_ID));
    const morePages =
      lastApplied < (lastData.locations || []).length &&
      findPages().length !== lastPageCount;
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
