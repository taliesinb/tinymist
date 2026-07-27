//! Injects compile diagnostics into the preview frontend.
//!
//! When `preview.errorOverlay` is enabled, the preview http server exposes a
//! `/dev/diagnostics` SSE endpoint and the served frontend html gets a small
//! script injected that highlights error locations on the last successful
//! render and shows the error messages in a floating panel.

use reflexo_typst::TypstDocument;
use serde::Serialize;
use tinymist_project::LspCompiledArtifact;
use tinymist_query::jump_from_cursor;
use typst::diag::Severity;
use typst::World;

/// The sender half of a per-preview diagnostics channel.
pub type DiagTx = tokio::sync::watch::Sender<String>;
/// The receiver half of a per-preview diagnostics channel.
pub type DiagRx = tokio::sync::watch::Receiver<String>;

/// The maximum number of message lines shown in the floating panel.
const MAX_MESSAGE_LINES: usize = 10;
/// The maximum number of resolved error locations.
const MAX_LOCATIONS: usize = 4;
/// How many lines to walk backwards when the exact error position does not
/// resolve on the last successful render.
const MAX_ANCHOR_WALK_BACK: usize = 100;

/// An error location resolved onto the last successful render.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlayLocation {
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

/// The diagnostics payload sent over the SSE channel.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OverlayPayload {
    /// Whether the last compilation succeeded.
    ok: bool,
    /// The first [`MAX_MESSAGE_LINES`] lines of the error output.
    messages: Vec<String>,
    /// The resolved error locations.
    locations: Vec<OverlayLocation>,
}

/// The initial payload before any compilation is observed.
pub fn initial_diagnostics_payload() -> String {
    serde_json::to_string(&OverlayPayload {
        ok: true,
        messages: vec![],
        locations: vec![],
    })
    .unwrap()
}

/// Renders the diagnostics of a compiled artifact as an overlay payload.
pub fn diagnostics_payload(art: &LspCompiledArtifact) -> String {
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
            // The error may sit in freshly edited text whose spans don't exist
            // in the last good document; in that case, walk backwards line by
            // line to the nearest position that does resolve.
            if locations.len() < MAX_LOCATIONS {
                log::info!(
                    "errorOverlay: success_doc={} src={}",
                    success_doc.is_some(),
                    src.is_some()
                );
                if let Some((doc, (source, cursor, line, _))) =
                    success_doc.as_ref().zip(src.as_ref())
                {
                    // `jump_from_cursor` only resolves on text leaves, so probe
                    // a few in-text positions per candidate line.
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
                    let pos = jump_from_cursor(doc, source, *cursor)
                        .into_iter()
                        .next()
                        .or_else(|| {
                            (line.saturating_sub(MAX_ANCHOR_WALK_BACK)..=*line)
                                .rev()
                                .find_map(try_line)
                        });
                    log::info!(
                        "errorOverlay: resolve line {line}: success_doc={} pos={pos:?}",
                        success_doc.is_some()
                    );
                    let page_size = |page: usize| {
                        let TypstDocument::Paged(paged) = doc else {
                            return None;
                        };
                        let size = paged.pages().get(page.checked_sub(1)?)?.frame.size();
                        Some((size.x.to_pt(), size.y.to_pt()))
                    };
                    if let Some(pos) = pos {
                        let page = pos.page.get();
                        if let Some((page_width, page_height)) = page_size(page) {
                            locations.push(OverlayLocation {
                                page,
                                x: pos.point.x.to_pt(),
                                y: pos.point.y.to_pt(),
                                page_width,
                                page_height,
                            });
                        }
                    }
                }
            }
        }
        if truncated > 0 {
            messages.push(format!("… and {truncated} more error(s)"));
        }
    }

    serde_json::to_string(&OverlayPayload {
        ok,
        messages,
        locations,
    })
    .unwrap()
}

/// The script injected into the frontend html when the error overlay is
/// enabled.
pub const ERROR_OVERLAY_JS: &str = r#"
(() => {
  const PANEL_ID = "tinymist-error-panel";
  const OVERLAY_CLASS = "tinymist-error-overlay";
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
  const render = (data) => {
    lastData = data;
    clear();
    lastApplied = 0;
    lastPageCount = findPages().length;
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
    const pages = findPages();
    for (const loc of data.locations || []) {
      const page = pages[loc.page - 1];
      if (!page) continue;
      try {
        if (page instanceof SVGGraphicsElement) {
          const bbox = page.getBBox();
          const h = Math.max(bbox.height * 0.025, 8);
          const rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
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
        console.warn("tinymist error overlay:", e);
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
        console.warn("tinymist error overlay:", e);
      }
    }
  };
  // The renderer may draw (or redraw) the document after the diagnostics
  // arrive, dropping our highlights; re-apply them when the DOM settles.
  let reapplyTimer = null;
  new MutationObserver(() => {
    if (!lastData || lastData.ok) return;
    const want = (lastData.locations || []).length;
    const have = document.querySelectorAll("." + OVERLAY_CLASS).length;
    const wiped = have < lastApplied;
    const morePages = lastApplied < want && findPages().length !== lastPageCount;
    if (!wiped && !morePages) return;
    if (reapplyTimer) clearTimeout(reapplyTimer);
    reapplyTimer = setTimeout(() => render(lastData), 150);
  }).observe(document.documentElement, { childList: true, subtree: true });

  const connect = () => {
    const es = new EventSource("/dev/diagnostics");
    es.onmessage = (ev) => {
      try {
        render(JSON.parse(ev.data));
      } catch (e) {
        console.warn("tinymist error overlay:", e);
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
