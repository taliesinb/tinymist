(() => {
  // Annotation mode (/?annotate): the view is locked to reading and
  // writing annotations — plain click annotates, and the editor-coupled
  // behaviors (cursor bar, click-to-jump, follow-scrolling) are muted.
  const ANNOTATE = new URLSearchParams(location.search).has("annotate");
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
  let assetVersion = null;
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
        ", svg.typst-doc .tinymist-probe-caret" +
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
  const render = (data) => {
    lastData = data;
    clear();
    lastApplied = 0;
    const pages = findPages();
    lastPageCount = pages.length;
    if (data.cursor && !ANNOTATE) {
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
  // Client-side errors are reported to the server (stderr + a well-known
  // temp file), so failures in the overlay scripts are findable after the
  // fact instead of hiding in the browser console.
  const report = (kind, detail) => {
    try {
      const line = JSON.stringify({
        kind,
        detail,
        url: location.pathname + location.search,
        time: new Date().toISOString(),
      });
      fetch("/dev/clientlog", { method: "POST", body: line, keepalive: true }).catch(
        () => {},
      );
    } catch (e) {}
  };
  window.addEventListener("error", (e) => {
    report(
      "error",
      `${e.message} @ ${e.filename}:${e.lineno}:${e.colno}` +
        (e.error && e.error.stack ? "\n" + e.error.stack : ""),
    );
  });
  window.addEventListener("unhandledrejection", (e) => {
    const r = e.reason;
    report("unhandledrejection", (r && (r.stack || r.message)) || String(r));
  });
  const origWarn = console.warn;
  console.warn = (...args) => {
    if (String(args[0]).startsWith("tinymist")) {
      report("warn", args.map((a) => (a && a.stack) || String(a)).join(" "));
    }
    origWarn.apply(console, args);
  };
  window.__tinymistReport = report;

  // Minimal API for the annotation layer, which is loaded only on
  // /?annotate pages; plain previews carry no annotation UI at all.
  window.__tinymist = {
    findPages,
    docDark: () => smartState.docDark,
    lastData: () => lastData,
  };
  if (ANNOTATE) {
    const script = document.createElement("script");
    script.src = "/dev/annotations.js";
    document.body.appendChild(script);
  }
  const connect = () => {
    const es = new EventSource("/dev/diagnostics");
    es.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data);
        // Dev asset auto-reload: the server bumps assetVersion when the
        // overlay script changes on disk.
        if (assetVersion === null) assetVersion = data.assetVersion || 0;
        else if ((data.assetVersion || 0) !== assetVersion) {
          location.reload();
          return;
        }
        updateSmartInvert(data);
        render(data);
        if (window.__tinymistAnnot) window.__tinymistAnnot(data);
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
