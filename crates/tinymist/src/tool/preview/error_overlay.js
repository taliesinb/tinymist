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
        "svg.typst-doc * { pointer-events: none !important; }\n" +
        "svg.typst-doc .tinymist-error-overlay, " +
        "svg.typst-doc .tinymist-error-overlay * " +
        "{ pointer-events: auto !important; cursor: pointer !important; }";
      document.head.appendChild(style);
    } else if (!on && style) {
      style.remove();
    }
  };
  if (ANNOTATE) {
    setAltMode(true);
    // The renderer scrolls the view on editor events (follow-cursor,
    // jumps); mute all programmatic scrolling in annotation mode.
    const noop = () => {};
    Element.prototype.scrollIntoView = noop;
    Element.prototype.scrollTo = noop;
    window.scrollTo = noop;
  } else {
    window.addEventListener("keydown", (e) => {
      if (e.key === "Alt") setAltMode(true);
    });
    window.addEventListener("keyup", (e) => {
      if (e.key === "Alt") setAltMode(false);
    });
    window.addEventListener("blur", () => setAltMode(false));
  }
  const ANNOT_BOX_ID = "tinymist-annot-box";
  let annotEscHandler = null;
  const closeAnnotBox = () => {
    const box = document.getElementById(ANNOT_BOX_ID);
    if (box) box.remove();
    if (typeof clearProbeCaret === "function") clearProbeCaret();
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
    // Docked: always the same predictable spot, bottom-left corner.
    void clientX;
    void clientY;
    box.style.left = "12px";
    box.style.bottom = "12px";
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
  // "5s ago", "1m30s ago", "32m ago", "2h30m ago", "1d ago"
  const timeAgo = (iso) => {
    const then = Date.parse(iso);
    if (isNaN(then)) return iso;
    const s = Math.max(Math.floor((Date.now() - then) / 1000), 0);
    if (s < 60) return s + "s ago";
    const m = Math.floor(s / 60);
    if (m < 10) return m + "m" + (s % 60 ? (s % 60) + "s" : "") + " ago";
    if (m < 60) return m + "m ago";
    const h = Math.floor(m / 60);
    if (h < 24) return h + "h" + (m % 60 ? (m % 60) + "m" : "") + " ago";
    return Math.floor(h / 24) + "d ago";
  };
  const postJson = (path, payload) =>
    fetch(path, { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));
  // A preview caret marking where a new annotation's anchor would land,
  // shown while the compose box is open.
  const PROBE_CLASS = "tinymist-probe-caret";
  const clearProbeCaret = () =>
    document.querySelectorAll("." + PROBE_CLASS).forEach((el) => el.remove());
  const drawProbeCaret = (pageNo, x, y) => {
    clearProbeCaret();
    const page = Array.from(findPages())[pageNo - 1];
    if (!(page instanceof SVGGraphicsElement)) return;
    const caret = document.createElementNS(SVG_NS, "path");
    caret.setAttribute("class", PROBE_CLASS);
    caret.setAttribute(
      "d",
      `M ${x - 2.2} ${y - 8.5} h 4.4 M ${x} ${y - 8.5} v 10 M ${x - 2.2} ${y + 1.5} h 4.4`,
    );
    caret.setAttribute("stroke", "rgb(245,166,35)");
    caret.setAttribute("stroke-width", "1.4");
    caret.setAttribute("stroke-linecap", "round");
    caret.setAttribute("fill", "none");
    page.appendChild(caret);
  };
  const linkButton = (label, color) => {
    const btn = document.createElement("button");
    btn.textContent = label;
    btn.style.cssText =
      "background:none;border:none;padding:0;cursor:pointer;font:12px system-ui,sans-serif;" +
      "color:" + color;
    return btn;
  };
  const showAnnot = (pin) => {
    const box = annotBox(null, null);
    const meta = document.createElement("div");
    meta.style.cssText = "color:#999;font-size:11px;margin-bottom:6px";
    meta.textContent = [pin.type, pin.author, pin.time && timeAgo(pin.time), pin.status]
      .filter(Boolean)
      .join(" · ");
    meta.title = pin.time || "";
    const text = document.createElement("div");
    text.style.cssText = "white-space:pre-wrap;margin-bottom:6px";
    text.textContent = pin.content;
    box.append(meta, text);
    for (const reply of pin.discussion || []) {
      const rMeta = document.createElement("div");
      rMeta.style.cssText =
        "color:#999;font-size:11px;margin:6px 0 2px;padding-top:5px;" +
        "border-top:1px solid rgba(255,255,255,0.12)";
      rMeta.textContent = [reply.author, reply.time && timeAgo(reply.time)]
        .filter(Boolean)
        .join(" · ");
      rMeta.title = reply.time || "";
      const rText = document.createElement("div");
      rText.style.cssText = "white-space:pre-wrap;font-size:12px";
      rText.textContent = reply.content;
      box.append(rMeta, rText);
    }
    const ta = document.createElement("textarea");
    ta.rows = 1;
    ta.placeholder = "Reply…  (⇧⏎ send)";
    ta.style.cssText =
      "display:block;width:100%;box-sizing:border-box;background:rgba(255,255,255,0.06);" +
      "color:#eee;border:none;outline:none;resize:none;border-radius:4px;" +
      "padding:5px 6px;font:12px/1.4 system-ui,sans-serif;margin:8px 0 6px";
    const doReply = () => {
      const text = ta.value.trim();
      if (text) post("/dev/annotate/reply", { uuid: pin.uuid, text });
      closeAnnotBox();
    };
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        doReply();
      }
    });
    const strip = document.createElement("div");
    strip.style.cssText = "display:flex;gap:10px";
    const reply = linkButton("reply", "rgb(123,216,143)");
    reply.onclick = doReply;
    const resolved = pin.status === "resolved";
    const toggle = linkButton(
      resolved ? "reopen" : "resolve",
      resolved ? "rgb(245,166,35)" : "rgb(150,180,220)",
    );
    toggle.onclick = () => {
      post("/dev/annotate/status", {
        uuid: pin.uuid,
        status: resolved ? "created" : "resolved",
      });
      closeAnnotBox();
    };
    const del = linkButton("delete", "rgb(220,130,130)");
    del.onclick = () => {
      post("/dev/annotate/delete", { uuid: pin.uuid });
      closeAnnotBox();
    };
    const close = linkButton("close", "rgb(150,150,150)");
    close.onclick = closeAnnotBox;
    strip.append(reply, toggle, del, close);
    box.append(ta, strip);
    ta.focus();
  };
  const composeAnnot = (pageNo, px, py) => {
    postJson("/dev/annotate/probe", { page: pageNo, x: px, y: py }).then((probe) => {
      if (!probe.ok) return; // margin, past the end, or non-text: nothing to anchor
      // Open first: opening replaces any previous box, which also clears
      // the previous probe caret.
      openCompose(pageNo, px, py);
      drawProbeCaret(probe.page, probe.x, probe.y);
    });
  };
  const openCompose = (pageNo, px, py) => {
    const box = annotBox(null, null);
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
        // Stamp a short random label (16 bits of entropy); the server falls
        // back to its own if this one is taken.
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const uuid = Array.from(rand, (b) =>
          b.toString(16).padStart(2, "0").toUpperCase(),
        ).join("");
        post("/dev/annotate", { uuid, page: pageNo, x: px, y: py, text });
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
      // created = amber, ongoing = faded amber, resolved = gray
      const done = pin.status === "resolved";
      const fill = done ? "rgb(150,150,150)" : "rgb(245,166,35)";
      const edge = done ? "rgb(90,90,90)" : "rgb(138,90,0)";
      const letter = pin.letter || "?";
      const w = Math.max(16, 9 + 5 * letter.length);
      // A zero-space reticle at the exact anchor position: a small I-beam
      // caret in the status color.
      const caret = document.createElementNS(SVG_NS, "path");
      caret.setAttribute(
        "d",
        `M ${pin.x - 2.2} ${pin.y - 8.5} h 4.4 ` +
          `M ${pin.x} ${pin.y - 8.5} v 10 ` +
          `M ${pin.x - 2.2} ${pin.y + 1.5} h 4.4`,
      );
      caret.setAttribute("stroke", done ? "rgb(150,150,150)" : "rgb(245,166,35)");
      caret.setAttribute("stroke-width", "1.1");
      caret.setAttribute("stroke-linecap", "round");
      caret.setAttribute("fill", "none");
      const line = document.createElementNS(SVG_NS, "line");
      line.setAttribute("x1", pin.x);
      line.setAttribute("y1", pin.y - 4);
      line.setAttribute("x2", cx - w / 2);
      line.setAttribute("y2", cy);
      line.setAttribute(
        "stroke",
        done ? "rgba(150,150,150,0.35)" : "rgba(245,166,35,0.35)",
      );
      line.setAttribute("stroke-dasharray", "2,2");
      const c = document.createElementNS(SVG_NS, "rect");
      c.setAttribute("x", cx - w / 2);
      c.setAttribute("y", cy - 8);
      c.setAttribute("width", w);
      c.setAttribute("height", 16);
      c.setAttribute("rx", 3);
      c.setAttribute("fill", fill);
      c.setAttribute("stroke", edge);
      if (pin.status === "ongoing") g.setAttribute("opacity", "0.55");
      c.setAttribute("stroke-width", "0.8");
      const t = document.createElementNS(SVG_NS, "text");
      t.setAttribute("x", cx);
      t.setAttribute("y", cy + 3);
      t.setAttribute("text-anchor", "middle");
      t.setAttribute("font-size", "9");
      t.setAttribute("font-family", "ui-monospace,monospace");
      t.setAttribute("fill", "white");
      t.textContent = letter;
      g.append(caret, line, c, t);
      g.addEventListener("click", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        showAnnot(pin);
      });
      page.appendChild(g);
      lastApplied += 1;
    });
  };
  document.addEventListener(
    "click",
    (ev) => {
      if (!ev.altKey && !ANNOTATE) return;
      if (
        ev.target &&
        ev.target.closest &&
        ev.target.closest(
          "." + OVERLAY_CLASS + ", #tinymist-annot-box, #tinymist-annot-stacks",
        )
      ) {
        return;
      }
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
      composeAnnot(pageNo, pt.x, pt.y);
    },
    true,
  );
  let assetVersion = null;
  // Off-screen annotations stack at the right edge — pins whose anchors
  // have scrolled past the top stick in a column at the top-right, ones
  // below the viewport at the bottom-right — so every annotation in the
  // document stays reachable.
  const STACK_ID = "tinymist-annot-stacks";
  const stackSquare = (pin) => {
    const done = pin.status === "resolved";
    const el = document.createElement("button");
    el.textContent = pin.letter || "?";
    el.title = (pin.author ? pin.author + ": " : "") + (pin.content || "");
    el.style.cssText =
      "pointer-events:auto;display:block;min-width:22px;height:20px;padding:0 4px;" +
      "border-radius:4px;cursor:pointer;font:11px ui-monospace,monospace;color:white;" +
      "border:1px solid " + (done ? "rgb(90,90,90)" : "rgb(138,90,0)") + ";" +
      "background:" + (done ? "rgb(150,150,150)" : "rgb(245,166,35)") + ";" +
      (pin.status === "ongoing" ? "opacity:0.55;" : "");
    el.onclick = (ev) => {
      ev.stopPropagation();
      scrollToPin(pin);
      showAnnot(pin);
    };
    return el;
  };
  const scrollToPin = (pin) => {
    const page = Array.from(findPages())[pin.page - 1];
    if (!(page instanceof SVGGraphicsElement)) return;
    const ctm = page.getScreenCTM();
    if (!ctm) return;
    const p = new DOMPoint(pin.x, pin.y).matrixTransform(ctm);
    // Find the scroll container (programmatic scrollTo is muted in annotate
    // mode, so adjust scrollTop directly).
    let sc = page.parentElement;
    while (sc && !(sc.scrollHeight > sc.clientHeight + 10)) sc = sc.parentElement;
    sc = sc || document.scrollingElement;
    sc.scrollTop += p.y - window.innerHeight / 2;
  };
  const updateStacks = () => {
    let host = document.getElementById(STACK_ID);
    if (!host) {
      host = document.createElement("div");
      host.id = STACK_ID;
      host.style.cssText =
        "position:fixed;inset:0;pointer-events:none;z-index:2147483645";
      document.body.appendChild(host);
    }
    host.replaceChildren();
    const list = (lastData && lastData.annotations) || [];
    if (!list.length) return;
    const pages = Array.from(findPages());
    const above = [];
    const below = [];
    for (const pin of list) {
      const page = pages[pin.page - 1];
      if (!(page instanceof SVGGraphicsElement)) continue;
      const ctm = page.getScreenCTM();
      if (!ctm) continue;
      const p = new DOMPoint(pin.x, pin.y).matrixTransform(ctm);
      if (p.y < 32) above.push([p.y, pin]);
      else if (p.y > window.innerHeight - 32) below.push([p.y, pin]);
    }
    const mkColumn = (items, fromTop) => {
      if (!items.length) return;
      const col = document.createElement("div");
      col.style.cssText =
        "position:absolute;right:10px;display:flex;flex-direction:column;gap:4px;" +
        (fromTop ? "top:10px" : "bottom:10px");
      items.sort((a, b) => a[0] - b[0]);
      for (const [, pin] of items) col.appendChild(stackSquare(pin));
      host.appendChild(col);
    };
    mkColumn(above, true);
    mkColumn(below, false);
  };
  document.addEventListener("scroll", updateStacks, { capture: true, passive: true });
  window.addEventListener("resize", updateStacks);
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
  setInterval(() => {
    ensure();
    updateStacks();
  }, 1000);
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
        updateStacks();
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
