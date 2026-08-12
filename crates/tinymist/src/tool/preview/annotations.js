// Annotation UI for the tinymist preview: anchor bubbles, the chip
// conveyor, and the annotation window. Loaded by error_overlay.js only on
// /?annotate pages; plain previews carry none of this.
(() => {
  const api = window.__tinymist;
  if (!api) return;
  const { findPages, docDark, lastData } = api;
  const SVG_NS = "http://www.w3.org/2000/svg";
  const ANNOTATE = location.pathname.replace(/\/+$/, "") === "/annotate";

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
    // The renderer pans the view on drag; in annotation mode a drag means
    // "select a span", so its mouse gestures are suppressed at the capture
    // phase (our own handlers listen on document, which still sees them).
    for (const type of ["dragstart", "selectstart"]) {
      window.addEventListener(
        type,
        (ev) => {
          if (
            ev.target &&
            ev.target.closest &&
            ev.target.closest("#tinymist-annot-box, #tinymist-annot-stacks")
          ) {
            return;
          }
          ev.stopPropagation();
          if (type === "dragstart" || type === "selectstart") ev.preventDefault();
        },
        true,
      );
    }
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
  const ANNOT_CSS_ID = "tinymist-annot-css";
  let annotEscHandler = null;
  // Whether the open window is a new-comment compose (no uuid yet): closing
  // it with unsaved text prompts, since there is nowhere to persist it.
  let composeActive = false;
  // Persisted drafts for existing annotations, keyed by (uuid, creation
  // time), so a reply typed but not sent survives closing and reopening.
  const draftKey = (pin) => `tinymist-annot-draft:${pin.uuid}:${pin.time}`;
  const loadDraft = (key) => {
    try {
      return (key && localStorage.getItem(key)) || "";
    } catch (e) {
      return "";
    }
  };
  const saveDraft = (key, value) => {
    if (!key) return;
    try {
      if (value.trim()) localStorage.setItem(key, value);
      else localStorage.removeItem(key);
    } catch (e) {}
  };
  // The uuid and content-signature of the annotation shown in the window,
  // for rebuilding it in place when SSE updates arrive.
  let openUuid = null;
  let openSig = null;

  // Status palette (luminance-matched): created = green, ongoing = purple,
  // resolved = blue-gray. Windows use a very dark tint of the status color
  // as background, with bg-tinted text.
  const STATUS_COLOR = { created: "#2f9e44", ongoing: "#a476f5", resolved: "#6c7d91" };
  const STATUS_BG = { created: "#0d2616", ongoing: "#1e1330", resolved: "#171e28" };
  const STATUS_TEXT = { created: "#b8d4be", ongoing: "#cabfe2", resolved: "#b9c4d2" };
  const STATUS_AUTHOR = { created: "#ddeee1", ongoing: "#e6dff5", resolved: "#dde5ee" };
  // Unselected icon letters: dimmed further into their status color, so the
  // bright white of the selected icon stands out.
  const STATUS_LETTER = { created: "#a5d4b1", ongoing: "#c6b3ee", resolved: "#bcc9d8" };
  const statusColor = (status) => STATUS_COLOR[status] || STATUS_COLOR.created;
  // Lightens a hex color toward white; the selected icon wears a lighter
  // version of its status color.
  const lighten = (hex, amount) => {
    const n = parseInt(hex.slice(1), 16);
    const mix = (c) => Math.round(c + (255 - c) * amount);
    return (
      "#" +
      [(n >> 16) & 255, (n >> 8) & 255, n & 255]
        .map((c) => mix(c).toString(16).padStart(2, "0"))
        .join("")
    );
  };
  const iconColor = (status, selected) =>
    selected ? lighten(statusColor(status), 0.28) : statusColor(status);

  const post = (path, payload) =>
    fetch(path, { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .then((r) => {
        if (!r.ok) console.warn("tinymist annotation:", r.error);
      })
      .catch((e) => console.warn("tinymist annotation:", e));
  const postJson = (path, payload) =>
    fetch(path, { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));

  // "5s", "5m", "11h", "3d" — largest unit only
  const timeAgo = (iso) => {
    const then = Date.parse(iso);
    if (isNaN(then)) return iso;
    const s = Math.max(Math.floor((Date.now() - then) / 1000), 0);
    if (s < 60) return s + "s";
    const m = Math.floor(s / 60);
    if (m < 60) return m + "m";
    const h = Math.floor(m / 60);
    if (h < 24) return h + "h";
    return Math.floor(h / 24) + "d";
  };

  // Display letters (bijective base 26), mirroring the server's assignment
  // so the compose window can predict the next letter; the server's choice
  // wins and the SSE refresh corrects any race.
  const letterIndex = (str) => {
    if (!str) return 0;
    let n = 0;
    for (const c of str.toLowerCase()) {
      const d = c.charCodeAt(0) - 96;
      if (d < 1 || d > 26) return 0;
      n = n * 26 + d;
    }
    return n;
  };
  const indexLetter = (n) => {
    let out = "";
    while (n > 0) {
      n -= 1;
      out = String.fromCharCode(97 + (n % 26)) + out;
      n = Math.floor(n / 26);
    }
    return out;
  };
  const nextLetter = () => {
    const list = (lastData() && lastData().annotations) || [];
    const max = Math.max(0, ...list.map((pin) => letterIndex(pin.letter)));
    return indexLetter(max + 1);
  };

  // The color of the page as displayed (accounting for smart inversion),
  // used to halo the caret and its letter so they separate from the text
  // they sit on.
  const pageIsDark = () =>
    docDark() === true || !!document.getElementById("tinymist-smart-invert");
  const pageHalo = () => (pageIsDark() ? "rgba(0,0,0,0.9)" : "rgba(255,255,255,0.9)");
  const positionBubble = (el, ctm, x, y, block, gap) => {
    const p = new DOMPoint(x, y).matrixTransform(ctm);
    if (block) {
      el.style.left = p.x - el.__w - (gap != null ? gap : 3) + "px";
      el.style.top = p.y - el.__h / 2 + "px";
    } else {
      el.style.left = p.x - el.__w / 2 + "px";
      el.style.top = p.y - 2 + "px";
    }
  };
  const ensureAnnotCss = () => {
    if (document.getElementById(ANNOT_CSS_ID)) return;
    const style = document.createElement("style");
    style.id = ANNOT_CSS_ID;
    style.textContent =
      `[data-tinymist-cursor="block"] svg.typst-doc,\n` +
      `[data-tinymist-cursor="block"] svg.typst-doc * { cursor: pointer !important; }\n` +
      `#${ANNOT_BOX_ID} .ta-acts { display: none; gap: 5px; }\n` +
      `#${ANNOT_BOX_ID} .ta-title:hover .ta-acts { display: inline-flex; }\n` +
      `#${ANNOT_BOX_ID} .ta-acts [data-act] { cursor: pointer; text-decoration: underline; }\n` +
      `#${ANNOT_BOX_ID} textarea::placeholder { color: rgba(255,255,255,0.35); }`;
    document.head.appendChild(style);
  };

  const closeAnnotBox = (force) => {
    const box = document.getElementById(ANNOT_BOX_ID);
    if (box && composeActive && !force) {
      const ta = box.querySelector("textarea");
      if (ta && ta.value.trim() && !confirm("Discard new comment?")) return false;
    }
    if (box) box.remove();
    if (annotEscHandler) {
      document.removeEventListener("keydown", annotEscHandler, true);
      annotEscHandler = null;
    }
    composeActive = false;
    ghostPin = null;
    openUuid = null;
    openSig = null;
    if (typeof updateStacks === "function") updateStacks();
    return true;
  };

  // The annotation window: a docked bottom-left rounded card whose
  // background is a dark tint of the status color, topped by a full-width
  // title strip in the full-strength color. The strip holds the letter and
  // (on hover) underlined slash-separated actions prefixing the status text.
  const annotShell = (status, letter, stateText, acts) => {
    if (!closeAnnotBox()) return null;
    ensureAnnotCss();
    const box = document.createElement("div");
    box.id = ANNOT_BOX_ID;
    box.style.cssText =
      "position:fixed;left:12px;bottom:12px;width:280px;border-radius:8px;" +
      "overflow:hidden;z-index:2147483647;box-shadow:0 6px 24px rgba(0,0,0,0.5);" +
      "font:13px/1.45 system-ui,sans-serif;background:" +
      (STATUS_BG[status] || STATUS_BG.created);
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

    const title = document.createElement("div");
    title.className = "ta-title";
    title.style.cssText =
      "display:flex;align-items:center;gap:10px;padding:4px 10px;min-height:20px;" +
      "background:" + statusColor(status);
    const letterEl = document.createElement("span");
    letterEl.style.cssText =
      "font:bold 12px ui-monospace,monospace;color:white;text-transform:uppercase";
    letterEl.textContent = letter;
    const right = document.createElement("span");
    right.style.cssText =
      "margin-left:auto;display:inline-flex;align-items:baseline;gap:5px;font-size:11px";
    const actsEl = document.createElement("span");
    actsEl.className = "ta-acts";
    actsEl.style.color = "rgba(255,255,255,0.92)";
    acts.forEach(([label, fn], idx) => {
      if (idx > 0) {
        const sep = document.createElement("span");
        sep.textContent = "/";
        sep.style.color = "rgba(255,255,255,0.55)";
        actsEl.appendChild(sep);
      }
      const act = document.createElement("span");
      act.dataset.act = label;
      act.textContent = label;
      act.onclick = fn;
      actsEl.appendChild(act);
    });
    const state = document.createElement("span");
    state.style.color = "rgba(255,255,255,0.78)";
    state.textContent = stateText;
    right.append(actsEl, state);
    title.append(letterEl, right);
    const content = document.createElement("div");
    content.style.cssText = "padding:10px 12px 12px";
    box.append(title, content);
    document.body.appendChild(box);
    return { box, content };
  };

  const msgRow = (status, author, time, text, first) => {
    const wrap = document.createElement("div");
    wrap.style.marginTop = first ? "0" : "10px";
    const head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:baseline";
    const who = document.createElement("b");
    who.style.cssText =
      "font-weight:650;font-size:12px;color:" +
      (STATUS_AUTHOR[status] || STATUS_AUTHOR.created);
    who.textContent = author || "unknown";
    const when = document.createElement("span");
    when.style.cssText = "margin-left:auto;font-size:11px;color:rgba(255,255,255,0.4)";
    when.textContent = time ? timeAgo(time) : "";
    when.title = time || "";
    head.append(who, when);
    const body = document.createElement("div");
    body.style.cssText =
      "margin-top:1px;white-space:pre-wrap;font-size:12.5px;color:" +
      (STATUS_TEXT[status] || STATUS_TEXT.created);
    body.textContent = text;
    wrap.append(head, body);
    return wrap;
  };

  const replyField = (status, placeholder, hint, rows, first, onSubmit) => {
    const wrap = document.createElement("div");
    wrap.style.cssText = "position:relative;margin-top:" + (first ? "0" : "14px");
    const ta = document.createElement("textarea");
    ta.rows = rows;
    ta.placeholder = placeholder;
    ta.style.cssText =
      "width:100%;box-sizing:border-box;background:rgba(255,255,255,0.07);" +
      "border:none;outline:none;resize:none;border-radius:5px;display:block;" +
      "overflow:hidden;" +
      "padding:6px 66px 6px 8px;font:12.5px/1.4 system-ui,sans-serif;color:" +
      (STATUS_TEXT[status] || STATUS_TEXT.created);
    // Grow with the content instead of scrolling; the window is anchored at
    // the bottom, so extra height extends it upward.
    const autosize = () => {
      ta.style.height = "auto";
      ta.style.height = ta.scrollHeight + "px";
    };
    requestAnimationFrame(autosize);
    const hintEl = document.createElement("span");
    hintEl.style.cssText =
      "position:absolute;right:8px;top:7px;font-size:11px;" +
      "color:rgba(255,255,255,0.28);pointer-events:none";
    hintEl.textContent = hint;
    ta.addEventListener("input", () => {
      hintEl.style.display = ta.value ? "none" : "";
      autosize();
      if (ta.__persistKey !== undefined) saveDraft(ta.__persistKey, ta.value);
    });
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && e.shiftKey) {
        e.preventDefault();
        onSubmit(ta);
      } else if (e.key === "Escape") {
        closeAnnotBox();
      }
    });
    wrap.append(ta, hintEl);
    return { wrap, ta };
  };

  const pinSig = (pin) =>
    JSON.stringify([pin.status, pin.type, pin.letter, pin.author, pin.time,
                    pin.content, pin.discussion]);

  const currentDraft = () => {
    const box = document.getElementById(ANNOT_BOX_ID);
    const ta = box && box.querySelector("textarea");
    return ta ? { draft: ta.value, focused: document.activeElement === ta } : null;
  };

  const changeStatus = (pin, status) => {
    post("/dev/annotate/status", { uuid: pin.uuid, status });
    showAnnot({ ...pin, status }, currentDraft());
  };

  const showAnnot = (pin, restore) => {
    const status = pin.status || "created";
    const acts = [];
    if (status === "resolved") {
      acts.push(["reopen", () => changeStatus(pin, "created")]);
    } else {
      acts.push(["resolve", () => changeStatus(pin, "resolved")]);
    }
    acts.push([
      "delete",
      () => {
        post("/dev/annotate/delete", { uuid: pin.uuid });
        saveDraft(draftKey(pin), "");
        closeAnnotBox(true);
      },
    ]);
    const shell = annotShell(
      status,
      pin.letter || "?",
      `${status} ${pin.type || "comment"}`,
      acts,
    );
    if (!shell) return;
    const { content } = shell;
    openUuid = pin.uuid;
    openSig = pinSig(pin);
    content.append(msgRow(status, pin.author, pin.time, pin.content, true));
    for (const reply of pin.discussion || []) {
      content.append(msgRow(status, reply.author, reply.time, reply.content, false));
    }
    const { wrap, ta } = replyField(
      status,
      status === "resolved" ? "Type reply to re-open" : "Type reply",
      "⇧⏎ send",
      1,
      false,
      (field) => {
        const text = field.value.trim();
        if (!text) return;
        post("/dev/annotate/reply", { uuid: pin.uuid, text });
        // Replying to a resolved annotation re-opens it.
        if (status === "resolved") post("/dev/annotate/status", { uuid: pin.uuid, status: "ongoing" });
        field.value = "";
        field.dispatchEvent(new Event("input"));
        saveDraft(draftKey(pin), "");
        // The window stays open; the SSE refresh appends the reply.
      },
    );
    content.append(wrap);
    // Unsent text survives closing and reopening the window.
    ta.__persistKey = draftKey(pin);
    const persisted = loadDraft(ta.__persistKey);
    if (restore || persisted) {
      ta.value = (restore && restore.draft) || persisted || "";
      ta.dispatchEvent(new Event("input"));
      if (!restore || restore.focused) {
        ta.focus();
        // Continue where the draft left off.
        ta.setSelectionRange(ta.value.length, ta.value.length);
      }
    } else {
      ta.focus();
    }
    updateStacks();
  };

  // Rebuilds the open window in place when its annotation changes (replies,
  // status flips, agent edits); closes it if the annotation was deleted.
  const refreshOpenAnnot = (data) => {
    if (!openUuid) return;
    if (!document.getElementById(ANNOT_BOX_ID)) {
      openUuid = null;
      return;
    }
    const pin = (data.annotations || []).find((p) => p.uuid === openUuid);
    if (!pin) {
      closeAnnotBox(true);
      return;
    }
    if (pinSig(pin) === openSig) return;
    showAnnot(pin, currentDraft());
  };

  const composeAnnot = (pageNo, px, py) => {
    postJson("/dev/annotate/probe", { page: pageNo, x: px, y: py }).then((probe) => {
      if (!probe.ok) return; // margin, past the end, or non-text: nothing to anchor
      // Open first: opening replaces any previous window, which also clears
      // the previous probe caret.
      const letter = nextLetter();
      openCompose(pageNo, px, py, letter);
      setGhost({
        letter,
        scope: probe.scope,
        rects: probe.rects || [],
        page: probe.page,
        x: probe.x,
        y: probe.y,
        pageWidth: pageWidthOf(probe.page),
      });
    });
  };
  const openComposeSpan = (range, block) => {
    const letter = nextLetter();
    const words = (wordsFor(block) || []).filter((w) => w.s >= range.s && w.e <= range.e);
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const uuid = Array.from(rand, (x) =>
          x.toString(16).padStart(2, "0").toUpperCase(),
        ).join("");
        post("/dev/annotate", { uuid, s: range.s, e: range.e, text });
      }
      closeAnnotBox(true);
    };
    const shell = annotShell("created", letter, "new span", [
      ["save", () => submit(document.querySelector(`#${ANNOT_BOX_ID} textarea`))],
      ["cancel", () => closeAnnotBox()],
    ]);
    if (!shell) return;
    composeActive = true;
    const { wrap, ta } = replyField("created", "Type comment", "⇧⏎ save", 3, true, submit);
    shell.content.append(wrap);
    ta.focus();
    setGhost({
      letter,
      scope: "span",
      rects: rectsFromWords(words),
      page: (words[0] && words[0].page) || 1,
      pageWidth: pageWidthOf((words[0] && words[0].page) || 1),
    });
  };
  const openComposeBlock = (margin) => {
    const letter = nextLetter();
    const scope = margin.kind; // "item" or "para"
    const rects =
      scope === "item" ? margin.block.rects.slice(0, 1) : margin.block.rects;
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const uuid = Array.from(rand, (x) =>
          x.toString(16).padStart(2, "0").toUpperCase(),
        ).join("");
        // Anchor at the end of the region's first word, so the label sits
        // inside the block it names.
        post("/dev/annotate", { uuid, s: margin.block.s, scope, text });
      }
      closeAnnotBox(true);
    };
    const shell = annotShell("created", letter, `new ${scope}`, [
      ["save", () => submit(document.querySelector(`#${ANNOT_BOX_ID} textarea`))],
      ["cancel", () => closeAnnotBox()],
    ]);
    if (!shell) return;
    composeActive = true;
    const { wrap, ta } = replyField("created", "Type comment", "⇧⏎ save", 3, true, submit);
    shell.content.append(wrap);
    ta.focus();
    setGhost({
      letter,
      scope,
      rects,
      page: rects[0] ? rects[0].page : 1,
      pageWidth: pageWidthOf(rects[0] ? rects[0].page : 1),
      railX: margin.block.railX,
      gutterX: margin.block.gutterX,
      // An item's marker hangs off its bullet, not off its text.
      x: scope === "item" ? gutterOf(margin.block) : undefined,
    });
  };
  const openComposePoint = (gap) => {
    const letter = nextLetter();
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const uuid = Array.from(rand, (x) =>
          x.toString(16).padStart(2, "0").toUpperCase(),
        ).join("");
        post("/dev/annotate", { uuid, s: gap.s, scope: "point", text });
      }
      closeAnnotBox(true);
    };
    const shell = annotShell("created", letter, "new point", [
      ["save", () => submit(document.querySelector(`#${ANNOT_BOX_ID} textarea`))],
      ["cancel", () => closeAnnotBox()],
    ]);
    if (!shell) return;
    composeActive = true;
    const { wrap, ta } = replyField("created", "Type comment", "⇧⏎ save", 3, true, submit);
    shell.content.append(wrap);
    ta.focus();
    const pageNo = (blockPageAt(gap) || 1);
    const doc = docPointOf(pageNo, gap.x, gap.y);
    setGhost({
      letter,
      scope: "point",
      rects: [],
      page: pageNo,
      x: doc ? doc.x : 0,
      y: doc ? doc.y : 0,
      pageWidth: pageWidthOf(pageNo),
    });
  };
  const openCompose = (pageNo, px, py, letter) => {
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        // Stamp a short random uuid (16 bits of entropy); the server falls
        // back to its own if this one is taken.
        const rand = crypto.getRandomValues(new Uint8Array(2));
        const uuid = Array.from(rand, (b) =>
          b.toString(16).padStart(2, "0").toUpperCase(),
        ).join("");
        post("/dev/annotate", { uuid, page: pageNo, x: px, y: py, text });
      }
      closeAnnotBox(true);
    };
    const shell = annotShell("created", letter || nextLetter(), "new comment", [
      ["save", () => submit(document.querySelector(`#${ANNOT_BOX_ID} textarea`))],
      ["cancel", () => closeAnnotBox()],
    ]);
    if (!shell) return;
    composeActive = true;
    const { wrap, ta } = replyField("created", "Type comment", "⇧⏎ save", 3, true, submit);
    shell.content.append(wrap);
    ta.focus();
  };
  // --- local hit-testing: the layout map ---
  // The server sends the document's structure once per compile (headings,
  // list items, paragraphs — small); the words of a region are fetched the
  // first time the cursor enters it. Hover and drag then run entirely in
  // the client, with no round trip per mouse move.
  let layout = null;
  const wordCache = new Map();
  const loadLayout = () => {
    postJson("/dev/annotate/layout", {}).then((res) => {
      if (res && res.ok) {
        layout = res.blocks || [];
        wordCache.clear();
      }
    });
  };
  const wordsFor = (block) => {
    const key = block.s + ":" + block.e;
    if (wordCache.has(key)) return wordCache.get(key);
    wordCache.set(key, null); // in flight
    postJson("/dev/annotate/words", { s: block.s, e: block.e }).then((res) => {
      wordCache.set(key, (res && res.ok && res.words) || []);
      // A drag that started before the words arrived still wants its
      // preview.
      if (drag && drag.block && drag.block.s === block.s && drag.redraw) {
        drag.redraw();
      }
    });
    return null;
  };
  const toScreen = (rect) => {
    const page = Array.from(findPages())[rect.page - 1];
    const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
    if (!ctm) return null;
    const a = new DOMPoint(rect.x0, rect.y0).matrixTransform(ctm);
    const b = new DOMPoint(rect.x1, rect.y1).matrixTransform(ctm);
    return { x0: a.x, y0: a.y, x1: b.x, y1: b.y };
  };
  const hitRect = (rect, x, y, pad) => {
    const r = toScreen(rect);
    return (
      r && x >= r.x0 - pad && x <= r.x1 + pad && y >= r.y0 - pad && y <= r.y1 + pad
    );
  };
  // The innermost region under the cursor: items beat paragraphs, headings
  // beat the paragraph they sit in.
  const blockAt = (x, y) => {
    if (!layout) return null;
    let best = null;
    for (const b of layout) {
      if (!b.rects.some((r) => hitRect(r, x, y, 2))) continue;
      const size = b.e - b.s;
      if (!best || size < best.e - best.s) best = b;
    }
    return best;
  };
  // Margin zones: a band 15..100px left of a paragraph annotates the whole
  // paragraph; the same band left of an item's first line annotates that
  // item. Items win where the two overlap, and the 15px next to the text
  // belong to neither.
  // A paragraph is claimed by the band its strip occupies, nothing wider:
  // the same band an existing annotation's strip covers, so one paragraph
  // can hold one paragraph annotation and the hit never overlaps the text.
  const paraZoneAt = (x, y) => {
    if (!layout) return null;
    let best = null;
    for (const b of layout) {
      if (b.kind !== "para") continue;
      const boxes = b.rects.map(toScreen).filter(Boolean);
      if (!boxes.length) continue;
      const band = bandOf(boxes, b.rects[0].page, b.gutterX, b.railX);
      if (x < band.x0 || x > band.x1 || y < band.y0 - 1 || y > band.y1 + 1) continue;
      if (!best || b.e - b.s < best.block.e - best.block.s) {
        best = { block: b, kind: "para" };
      }
    }
    return best;
  };
  const marginZoneAt = (x, y) => itemZoneAt(x, y) || paraZoneAt(x, y);
  // A word claims its box minus 4px at each end: those edges belong to the
  // inter-word point, so a point is easy to hit.
  const WORD_INSET = 4;
  const wordAt = (block, x, y) => {
    const words = block && wordsFor(block);
    if (!words) return null;
    return (
      words.find((w) => {
        const r = toScreen(w);
        return (
          r &&
          y >= r.y0 - 1 &&
          y <= r.y1 + 1 &&
          x >= r.x0 + WORD_INSET &&
          x <= r.x1 - WORD_INSET
        );
      }) || null
    );
  };

  // A transient, letter-less preview of what a click or release would make.
  const HOVER_CLASS = "tinymist-hover-preview";
  const clearHover = () =>
    document.querySelectorAll("." + HOVER_CLASS).forEach((el) => el.remove());
  const drawPreviewRects = (rects, vertical, strong) => {
    clearHover();
    const host = document.getElementById(STACK_ID);
    if (!host || !rects.length) return;
    const color = statusColor("created");
    if (vertical) {
      // One strip spanning the whole region, not a dashed line of them.
      const boxes = rects.map(toScreen).filter(Boolean);
      if (!boxes.length) return;
      const top = Math.min(...boxes.map((b) => b.y0));
      const bot = Math.max(...boxes.map((b) => b.y1));
      const left = Math.min(...boxes.map((b) => b.x0));
      const el = document.createElement("div");
      el.className = HOVER_CLASS;
      el.style.cssText =
        `z-index:0;position:absolute;pointer-events:none;border-radius:1.5px;` +
        `background:${color};opacity:${strong ? 0.85 : 0.5};width:3px;` +
        `left:${left - 6}px;top:${top}px;height:${Math.max(bot - top, 4)}px`;
      host.appendChild(el);
      return;
    }
    for (const rect of rects) {
      const r = toScreen(rect);
      if (!r) continue;
      const el = document.createElement("div");
      el.className = HOVER_CLASS;
      el.style.cssText = vertical
        ? `z-index:0;position:absolute;pointer-events:none;border-radius:1.5px;` +
          `background:${color};opacity:${strong ? 0.85 : 0.5};width:3px;` +
          `left:${r.x0 - 6}px;top:${r.y0}px;height:${Math.max(r.y1 - r.y0, 4)}px`
        : `z-index:0;position:absolute;pointer-events:none;border-radius:1.5px;` +
          `background:${color};opacity:${strong ? 0.85 : 0.45};height:2px;` +
          `left:${r.x0}px;top:${r.y1 + 1}px;width:${Math.max(r.x1 - r.x0, 2)}px`;
      host.appendChild(el);
    }
  };
  // Merges word boxes into per-line rects.
  const rectsFromWords = (words) => {
    const rows = [];
    for (const w of words) {
      const last = rows[rows.length - 1];
      if (last && last.page === w.page && Math.abs(last.y1 - w.y1) < 1.5) {
        last.x0 = Math.min(last.x0, w.x0);
        last.y0 = Math.min(last.y0, w.y0);
        last.x1 = Math.max(last.x1, w.x1);
        last.y1 = Math.max(last.y1, w.y1);
      } else {
        rows.push({ page: w.page, x0: w.x0, y0: w.y0, x1: w.x1, y1: w.y1 });
      }
    }
    return rows;
  };

  // The preview of a paragraph annotation is the very strip the click would
  // create — same edge, same extent — so what you see is what you get, and
  // the strip already there covers the same ground you would be clicking.
  const drawParaPreview = (block) => {
    clearHover();
    const host = document.getElementById(STACK_ID);
    const rects = block && block.rects;
    if (!host || !rects || !rects.length) return;
    const boxes = rects.map(toScreen).filter(Boolean);
    if (!boxes.length) return;
    const band = bandOf(boxes, rects[0].page, block.gutterX, block.railX);
    const el = document.createElement("div");
    el.className = HOVER_CLASS;
    el.style.cssText =
      `z-index:0;position:absolute;pointer-events:none;border-radius:1.5px;` +
      `background:${statusColor("created")};opacity:0.5;width:${STRIP_W}px;` +
      `left:${band.left}px;` +
      `top:${band.y0}px;height:${Math.max(band.y1 - band.y0, 4)}px`;
    host.appendChild(el);
  };
  // An item is marked by a pointer chip beside its bullet, never a strip, so
  // its preview is that chip, letterless and pale.
  const drawItemPreview = (block) => {
    clearHover();
    const host = document.getElementById(STACK_ID);
    const rect = block && block.rects && block.rects[0];
    if (!host || !rect) return;
    const page = Array.from(findPages())[rect.page - 1];
    const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
    if (!ctm) return;
    const el = document.createElement("div");
    el.className = HOVER_CLASS;
    el.style.cssText =
      "z-index:0;position:absolute;pointer-events:none;line-height:0;opacity:0.55";
    renderBubble(el, { uuid: "hover", letter: "", status: "created" }, "right");
    // The gutter is the bullet's own edge, so the chip clears it exactly as
    // the real one does instead of landing on top of it.
    positionBubble(el, ctm, gutterOf(block), (rect.y0 + rect.y1) / 2, true, CHIP_GAP);
    host.appendChild(el);
  };
  const gutterOf = (block) =>
    block.gutterX != null ? block.gutterX : block.rects[0].x0;
  // The horizontal span of an item's mark, in screen px: from the left edge of
  // its chip, across the bullet or number, up to where the item's text starts.
  // The bullet is not part of the item's boxes, which is why the server sends
  // the gutter separately — clicking a bullet is clicking its item.
  const itemBandX = (pageNo, gutterX, textX0) => {
    const gx = docToScreenX(pageNo, gutterX);
    if (gx == null) return null;
    const tx = docToScreenX(pageNo, textX0);
    const x0 = gx - CHIP_GAP - CHIP_W - 4;
    // Without a text edge to stop at, allow the width of a wide number.
    const x1 = tx != null && tx > gx ? tx - 1 : gx + 20;
    return { x0, x1 };
  };
  // An item is claimed by the box its chip occupies — the same box the chip
  // of an existing annotation fills, so one item holds one item annotation.
  const itemZoneAt = (x, y) => {
    if (!layout) return null;
    let best = null;
    for (const b of layout) {
      if (b.kind !== "item") continue;
      const r = toScreen(b.rects[0]);
      if (!r) continue;
      const band = itemBandX(b.rects[0].page, gutterOf(b), b.rects[0].x0);
      if (!band) continue;
      if (x < band.x0 || x > band.x1 || y < r.y0 - 2 || y > r.y1 + 2) continue;
      if (!best || b.e - b.s < best.block.e - best.block.s) {
        best = { block: b, kind: "item" };
      }
    }
    return best;
  };
  // Between words: a small chevron where a point annotation would go.
  const drawPointPreview = (gap) => {
    clearHover();
    const host = document.getElementById(STACK_ID);
    if (!host || !gap) return;
    const el = document.createElement("div");
    el.className = HOVER_CLASS;
    const color = statusColor("created");
    const w = 12;
    const h = 7;
    el.style.cssText =
      `z-index:0;position:absolute;pointer-events:none;line-height:0;opacity:0.8;` +
      `left:${gap.x - w / 2}px;top:${gap.y + 2}px`;
    el.innerHTML =
      `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
      `<path d="M 1 ${h - 1} L ${w / 2} 1 L ${w - 1} ${h - 1}" fill="none"` +
      ` stroke="${color}" stroke-width="2" stroke-linecap="round"` +
      ` stroke-linejoin="round"></path></svg>`;
    host.appendChild(el);
  };
  // The insertion point between two words on the hovered line: its screen
  // position, and the source offset a point anchor would use (the end of
  // the word to the left).
  const blockPageAt = (gap) =>
    (layout || []).flatMap((b) => b.rects).find((rect) => {
      const r = toScreen(rect);
      return r && gap.y >= r.y0 - 4 && gap.y <= r.y1 + 4 && gap.x >= r.x0 - 60;
    })?.page;
  const gapAt = (block, x, y) => {
    const words = block && wordsFor(block);
    if (!words) return null;
    const line = words
      .map((w) => ({ w, r: toScreen(w) }))
      .filter(({ r }) => r && y >= r.y0 - 2 && y <= r.y1 + 2);
    if (!line.length) return null;
    line.sort((a, b) => a.r.x0 - b.r.x0);
    // A point attaches to the word on its left, so one must exist.
    let prev = null;
    let next = null;
    for (const item of line) {
      if (item.r.x1 - WORD_INSET <= x) prev = item;
      else if (!next) next = item;
    }
    if (!prev) return null;
    return {
      // Midway between the words it sits between, when there is one to the
      // right; otherwise just past the word on the left.
      x: next ? (prev.r.x1 + next.r.x0) / 2 : prev.r.x1,
      y: prev.r.y1,
      s: prev.w.e,
      prev: prev.w,
      next: next && next.w,
    };
  };

  // The annotation being composed is rendered as a real one — same
  // decoration, same letter — so the edit window always has a visible host.
  const GHOST_UUID = "tinymist-ghost";
  let ghostPin = null;
  const pageWidthOf = (pageNo) => {
    const page = Array.from(findPages())[pageNo - 1];
    return parseFloat((page && page.dataset && page.dataset.pageWidth) || "0") || 595;
  };
  const setGhost = (pin) => {
    if (!pin) {
      ghostPin = null;
      updateStacks();
      return;
    }
    const ghost = { uuid: GHOST_UUID, status: "created", ...pin };
    if (!ghost.pageWidth) ghost.pageWidth = pageWidthOf(ghost.page || 1);
    // Give it the same anchor point a real pin of this scope would get, so
    // its marks land where the saved annotation's will. An x supplied by the
    // caller (an item's gutter) wins: it is measured, not derived.
    const first = (ghost.rects || [])[0];
    const last = (ghost.rects || [])[ghost.rects.length - 1];
    if (first && last) {
      const region =
        ghost.scope === "block" || ghost.scope === "item" || ghost.scope === "para";
      if (ghost.page === undefined) ghost.page = region ? first.page : last.page;
      if (ghost.x === undefined) {
        ghost.x = region ? first.x0 : (last.x0 + last.x1) / 2;
      }
      if (ghost.y === undefined) {
        ghost.y = region
          ? ghost.scope === "item"
            ? (first.y0 + first.y1) / 2
            : (first.y0 + last.y1) / 2
          : last.y1;
      }
    }
    ghostPin = ghost;
    updateStacks();
  };
  const docPointOf = (pageNo, screenX, screenY) => {
    const page = Array.from(findPages())[pageNo - 1];
    const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
    if (!ctm) return null;
    return new DOMPoint(screenX, screenY).matrixTransform(ctm.inverse());
  };

  let drag = null;
  // A real mouse emits click after mouseup; a drag has already acted on it.
  let swallowNextClick = false;
  const setCursorHint = (kind) => {
    const root = document.documentElement;
    if (root.dataset.tinymistCursor !== kind) root.dataset.tinymistCursor = kind;
  };
  const previewAt = (ev) => {
    ensureAnnotCss();
    const margin = marginZoneAt(ev.clientX, ev.clientY);
    setCursorHint(margin ? "block" : "text");
    if (margin) {
      lastGap = null;
      if (margin.kind === "item") drawItemPreview(margin.block);
      else drawParaPreview(margin.block);
      return;
    }
    const block = blockAt(ev.clientX, ev.clientY);
    if (!block) return clearHover();
    const word = wordAt(block, ev.clientX, ev.clientY);
    if (word) {
      // A word: a click annotates the word itself.
      drawPreviewRects([word], false, false);
      lastGap = null;
      return;
    }
    // Between words: a click makes a point, anchored to the word on the
    // left. Failing that (empty space in the region), the region itself.
    const gap = gapAt(block, ev.clientX, ev.clientY);
    lastGap = gap;
    // Empty space inside a block annotates nothing: paragraphs are claimed
    // from their rail, so there is no preview to show here.
    if (gap) drawPointPreview(gap);
    else clearHover();
  };
  let lastGap = null;
  const onMouseDown = (ev) => {
    if (!ANNOTATE || ev.button !== 0 || onOverlay(ev)) return;
    if (openUuid !== null || composeActive) return; // this click only dismisses
    const block = blockAt(ev.clientX, ev.clientY);
    const word = wordAt(block, ev.clientX, ev.clientY);
    const gap = word ? null : gapAt(block, ev.clientX, ev.clientY);
    if (!word && !gap) return;
    wordsFor(block); // warm the cache for the drag that may follow
    drag = {
      block,
      from: { x: ev.clientX, y: ev.clientY },
      first: word,
      gap,
      moved: false,
    };
  };
  const onMouseMove = (ev) => {
    if (!ANNOTATE) return;
    if (!drag) {
      if (openUuid !== null || composeActive || onOverlay(ev)) return clearHover();
      previewAt(ev);
      return;
    }
    const dx = ev.clientX - drag.from.x;
    const dy = ev.clientY - drag.from.y;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    drag.moved = true;
    if (!drag.first && drag.gap) {
      // Started from a point: dragging right begins at the word after it,
      // dragging left ends at the word before it.
      drag.first = ev.clientX >= drag.from.x ? drag.gap.next : drag.gap.prev;
      if (!drag.first) return;
    }
    const word = wordAt(drag.block, ev.clientX, ev.clientY) || drag.last;
    if (!word) return;
    drag.last = word;
    const words = wordsFor(drag.block) || [];
    const lo = Math.min(drag.first.s, word.s);
    const hi = Math.max(drag.first.e, word.e);
    drag.range = { s: lo, e: hi };
    drag.redraw = () => {
      const ws = wordsFor(drag.block) || [];
      drawPreviewRects(
        rectsFromWords(ws.filter((w) => w.s >= lo && w.e <= hi)),
        false,
        true,
      );
    };
    drag.redraw();
  };
  const onMouseUp = (ev) => {
    if (!ANNOTATE || !drag) return;
    const d = drag;
    drag = null;
    clearHover();
    // A drag either way makes a span; only a range that never grew beyond
    // the word it started on is a plain click.
    const grew =
      d.range && (d.range.s < d.first.s || d.range.e > d.first.e);
    if (!d.moved || !grew) return;
    ev.preventDefault();
    ev.stopImmediatePropagation();
    swallowNextClick = true;
    openComposeSpan(d.range, d.block);
  };
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && drag) {
      drag = null;
      clearHover();
    }
  });
  const onOverlay = (ev) =>
    ev.target &&
    ev.target.closest &&
    ev.target.closest(
      ".tinymist-error-overlay, #tinymist-annot-box, #tinymist-annot-stacks",
    );
  const MOUSE_HANDLERS = {
    mousedown: onMouseDown,
    mousemove: onMouseMove,
    mouseup: onMouseUp,
  };
  for (const [type, handler] of Object.entries(MOUSE_HANDLERS)) {
    window.addEventListener(
      type,
      (ev) => {
        handler(ev);
        // In annotation mode the renderer must not also pan or select.
        if (ANNOTATE && !onOverlay(ev)) ev.stopPropagation();
      },
      true,
    );
  }

  document.addEventListener(
    "click",
    (ev) => {
      if (swallowNextClick) {
        swallowNextClick = false;
        ev.preventDefault();
        ev.stopImmediatePropagation();
        return;
      }
      if (!ev.altKey && !ANNOTATE) return;
      if (
        ev.target &&
        ev.target.closest &&
        ev.target.closest(
          ".tinymist-error-overlay, #tinymist-annot-box, #tinymist-annot-stacks",
        )
      ) {
        return;
      }
      // The left margin creates the structural annotation it previews.
      if (openUuid === null && !composeActive) {
        const margin = marginZoneAt(ev.clientX, ev.clientY);
        if (margin) {
          ev.preventDefault();
          ev.stopImmediatePropagation();
          openComposeBlock(margin);
          return;
        }
      }
      // Between words: create a point there, anchored to the word on the
      // left, rather than annotating a word the cursor is not on.
      if (openUuid === null && !composeActive) {
        const block = blockAt(ev.clientX, ev.clientY);
        const gap =
          block && !wordAt(block, ev.clientX, ev.clientY)
            ? gapAt(block, ev.clientX, ev.clientY)
            : null;
        if (gap) {
          ev.preventDefault();
          ev.stopImmediatePropagation();
          openComposePoint(gap);
          return;
        }
      }
      // A click while something is selected only dismisses it; creating a
      // new annotation takes a second click, from a clean slate.
      if (openUuid !== null || composeActive) {
        if (closeAnnotBox()) {
          ev.preventDefault();
          ev.stopImmediatePropagation();
        }
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
  // Off-screen annotations stack at the right edge — pins whose anchors
  // have scrolled past the top stick in a column at the top-right, ones
  // below the viewport at the bottom-right — so every annotation in the
  // document stays reachable.
  const STACK_ID = "tinymist-annot-stacks";
  // Off-screen annotations ride a conveyor belt around the viewport edge,
  // rotating counter-clockwise as you scroll down: an icon below the
  // viewport slides RIGHT along the bottom edge as its anchor approaches,
  // lives on the right side while visible (the inline pin), then slides
  // LEFT along the top edge once its anchor has scrolled past. Both rows
  // are right-anchored: the nearest off-screen annotation sits at the
  // corner, farther ones extend leftward.
  const SLOT = 27; // px per icon along a row
  const stackSquare = (pin) => {
    const el = document.createElement("button");
    el.dataset.uuid = pin.uuid;
    el.style.cssText =
      "z-index:1;pointer-events:auto;position:absolute;display:block;min-width:22px;height:20px;" +
      "padding:0 4px;border-radius:4px;cursor:pointer;color:white;border:none;" +
      "font:bold 11px ui-monospace,monospace;text-transform:uppercase;" +
      "transition:left 0.25s ease,top 0.25s ease";
    el.onclick = (ev) => {
      ev.stopPropagation();
      if (openUuid === el.__pin.uuid) {
        closeAnnotBox();
      } else {
        if (el.dataset.state !== "inline") scrollToPin(el.__pin);
        showAnnot(el.__pin);
      }
      updateStacks();
    };
    return el;
  };
  // Animates scrollTop over ~250ms with ease-in-out (setting the property
  // directly, since programmatic scrollTo is muted in annotate mode). The
  // chip conveyor tracks along via the scroll events each frame fires.
  const animateScroll = (sc, target, ms = 250) => {
    const start = sc.scrollTop;
    const delta = target - start;
    if (!delta) return;
    const t0 = performance.now();
    const ease = (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);
    const step = (now) => {
      const t = Math.min((now - t0) / ms, 1);
      sc.scrollTop = start + delta * ease(t);
      if (t < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  };
  const scrollToPin = (pin) => {
    const page = Array.from(findPages())[pin.page - 1];
    if (!(page instanceof SVGGraphicsElement)) return;
    const ctm = page.getScreenCTM();
    if (!ctm) return;
    const p = new DOMPoint(pin.x, pin.y).matrixTransform(ctm);
    let sc = page.parentElement;
    while (sc && !(sc.scrollHeight > sc.clientHeight + 10)) sc = sc.parentElement;
    sc = sc || document.scrollingElement;
    animateScroll(sc, sc.scrollTop + p.y - window.innerHeight / 2);
  };
  // Scopes whose strip already shows the extent get a lighter marker: the
  // bare letter, haloed against the page, attached to the strip's end and
  // centred on it (as if an arrow pointed at the strip).
  const STRIPPED_SCOPES = new Set(["word", "sentence", "span", "block", "para"]);
  // Every mark hangs off geometry the server measured: a region's gutter (the
  // x of its bullet or number, which is not part of its own boxes) and the
  // page's rail (its leftmost ink). The client only adds its own chip size.
  const STRIP_W = 3;
  const CHIP_W = 22;
  const CHIP_GAP = 7;
  // The paragraph rail clears a full item chip, so the two never overlap.
  const RAIL_CLEAR = CHIP_W + CHIP_GAP + 8;
  const docToScreenX = (pageNo, x) => {
    const page = Array.from(findPages())[pageNo - 1];
    const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
    if (!ctm) return null;
    return new DOMPoint(x, 0).matrixTransform(ctm).x;
  };
  // The rail's hit band: the strip padded to something a mouse can land on.
  // The band is shared by the strip that exists and the click that would
  // create one, so a paragraph that already has an annotation cannot be
  // given a second — the existing strip owns the ground.
  // The region's own gutter when it has one (a paragraph of bullets reaches
  // out to their markers), else the page rail, else — for a server too old to
  // measure either — the text edge.
  const bandOf = (boxes, pageNo, gutterX, railX) => {
    const doc = gutterX != null ? gutterX : railX;
    const anchor = doc != null ? docToScreenX(pageNo, doc) : null;
    const left =
      anchor != null ? anchor - RAIL_CLEAR : Math.min(...boxes.map((b) => b.x0)) - 6;
    return {
      left,
      // Wide enough on the left to cover the letter that hangs off the strip,
      // so the whole mark — glyph included — is one target.
      x0: left - 30,
      x1: left + STRIP_W + 5,
      y0: Math.min(...boxes.map((b) => b.y0)),
      y1: Math.max(...boxes.map((b) => b.y1)),
    };
  };
  const renderLetterGlyph = (el, pin) => {
    const letter = (pin.letter || "?").toUpperCase();
    const selected = pin.uuid === openUuid;
    const color = iconColor(pin.status, selected);
    const fill = selected ? "#ffffff" : color;
    const halo = pageIsDark() ? "#0e0e0e" : "#ffffff";
    const w = Math.max(10, 6 * letter.length + 4);
    const h = 12;
    const sig = ["glyph", letter, fill, halo].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    el.__w = w;
    el.__h = h;
    el.style.filter = "";
    el.innerHTML =
      `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
      `<text x="${w - 2}" y="${h / 2 + 3}" text-anchor="end" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace" fill="${fill}"` +
      ` style="paint-order:stroke;stroke:${halo};stroke-width:3px;stroke-linejoin:round">` +
      `${letter}</text></svg>`;
  };
  // Whether the page ground is displayed dark (native dark doc, or light
  // doc under smart inversion — checked via the live style element).
  const displayedPageDark = pageIsDark;
  const renderBubble = (el, pin, dir) => {
    const letter = (pin.letter || "?").toUpperCase();
    const selected = pin.uuid === openUuid;
    const w = Math.max(15, 5 + 6 * letter.length);
    const tri = 6;
    const h = 11;
    const r = 3.5;
    const pad = 2;
    const W = w + pad * 2;
    const H = tri + h + pad * 2;
    const color = iconColor(pin.status, selected);
    const border = displayedPageDark() ? "#0e0e0e" : "#ffffff";
    const cx = pad + w / 2;
    const bot = pad + tri + h;
    const d =
      `M ${cx} ${pad}` +
      ` L ${pad + w} ${pad + tri}` +
      ` L ${pad + w} ${bot - r}` +
      ` Q ${pad + w} ${bot} ${pad + w - r} ${bot}` +
      ` L ${pad + r} ${bot}` +
      ` Q ${pad} ${bot} ${pad} ${bot - r}` +
      ` L ${pad} ${pad + tri}` +
      ` Z`;
    const sig = [letter, color, border, selected, dir || "up"].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    el.__w = W;
    // Inline markers show selection through colour alone; the glow is
    // reserved for the edge-row chips, where it is the only cue.
    el.style.filter = "";
    // Block anchors point right, at the left edge of the block they mark;
    // text anchors point up, at the baseline of the word they follow. Both
    // are the same shape, drawn in the direction they point.
    const fill = selected
      ? "#ffffff"
      : STATUS_LETTER[pin.status] || STATUS_LETTER.created;
    let svgW = W;
    let svgH = H;
    let path = d;
    let tx = cx;
    let ty = pad + tri + h / 2 + 1;
    if (dir === "right") {
      // Body of depth h and height w, apex on the right at mid-height.
      svgW = pad * 2 + h + tri;
      svgH = pad * 2 + w;
      const left = pad;
      const right = pad + h;
      const top = pad;
      const bot = pad + w;
      const cy = (top + bot) / 2;
      path =
        `M ${right + tri} ${cy}` +
        ` L ${right} ${top}` +
        ` L ${left + r} ${top}` +
        ` Q ${left} ${top} ${left} ${top + r}` +
        ` L ${left} ${bot - r}` +
        ` Q ${left} ${bot} ${left + r} ${bot}` +
        ` L ${right} ${bot}` +
        ` Z`;
      tx = (left + right) / 2 + 1.5;
      ty = cy + 3.5;
    }
    el.__w = svgW;
    el.__h = svgH;
    el.innerHTML =
      `<svg width="${svgW}" height="${svgH}" viewBox="0 0 ${svgW} ${svgH}">` +
      `<path d="${path}" fill="${color}" stroke="${border}" stroke-width="2"` +
      ` stroke-linejoin="round"></path>` +
      `<text x="${tx}" y="${ty}" text-anchor="middle" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace" fill="${fill}">${letter}</text>` +
      `</svg>`;
  };
  // Scope decorations: a strip along the referent, drawn in the fixed
  // overlay layer so it tracks scroll with everything else.
  //   word / sentence / span : strip under each rect (bottom edge)
  //   block                  : strip along the left edge of the rect
  //   para                   : gutter strip, bridged across pages and
  //                            clipped to the viewport, drawn through the
  //                            letter chip
  //   point                  : none
  const STRIP_CLASS = "tinymist-annot-strip";
  const stripFor = (host, key, color) => {
    let el = host.querySelector(`[data-strip="${CSS.escape(key)}"]`);
    if (!el) {
      el = document.createElement("div");
      el.dataset.strip = key;
      el.className = STRIP_CLASS;
      el.style.cssText =
        "position:absolute;pointer-events:none;border-radius:1.5px;z-index:0";
      // Strips go under the icons: prepend so they paint first.
      host.insertBefore(el, host.firstChild);
    }
    el.style.background = color;
    el.dataset.seen = "1";
    return el;
  };
  // The clickable band around a region strip: the strip, the room its letter
  // occupies to the left, and a few pixels of slack to the right.
  const stripHit = (host, pin, left, top, height) => {
    const hit = stripFor(host, pin.uuid + ":phit", "transparent");
    hit.style.left = left - 30 + "px";
    hit.style.top = top + "px";
    hit.style.width = 30 + STRIP_W + 5 + "px";
    hit.style.height = Math.max(height, 4) + "px";
    hit.style.pointerEvents = "auto";
    hit.style.cursor = "pointer";
    hit.__pin = pin;
    hit.onclick = (ev) => {
      ev.stopPropagation();
      if (openUuid === hit.__pin.uuid) closeAnnotBox();
      else showAnnot(hit.__pin);
      updateStacks();
    };
  };
  const drawDecorations = (host, list, pages) => {
    for (const el of host.querySelectorAll("." + STRIP_CLASS)) el.dataset.seen = "";
    for (const pin of list) {
      const scope = pin.scope || "point";
      if (scope === "point" || !pin.rects || !pin.rects.length) continue;
      const color = iconColor(pin.status, pin.uuid === openUuid);
      const boxes = pin.rects
        .map((r) => {
          const page = pages[r.page - 1];
          const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
          if (!ctm) return null;
          const a = new DOMPoint(r.x0, r.y0).matrixTransform(ctm);
          const b = new DOMPoint(r.x1, r.y1).matrixTransform(ctm);
          return { x0: a.x, y0: a.y, x1: b.x, y1: b.y };
        })
        .filter(Boolean);
      if (!boxes.length) continue;
      if (scope === "para") {
        // The inline strip down the paragraph's left edge, carrying the
        // letter, exactly as a block is marked — and clickable, so the
        // paragraph reads as taken.
        const band = bandOf(boxes, pin.rects[0].page, pin.gutterX, pin.railX);
        const inline = stripFor(host, pin.uuid + ":pleft", color);
        inline.style.width = STRIP_W + "px";
        inline.style.left = band.left + "px";
        inline.style.top = band.y0 + "px";
        inline.style.height = Math.max(band.y1 - band.y0, 4) + "px";
        stripHit(host, pin, band.left, band.y0, band.y1 - band.y0);
        // One bridged strip in the gutter, clipped to the viewport.
        const top = Math.max(Math.min(...boxes.map((b) => b.y0)), 4);
        const bot = Math.min(Math.max(...boxes.map((b) => b.y1)), window.innerHeight - 4);
        if (bot <= top) continue;
        const laneX = laneLeftPx(pin, pages);
        const el = stripFor(host, pin.uuid + ":para", color);
        el.style.width = "3px";
        el.style.left = laneX + 9 + "px";
        el.style.top = top + "px";
        el.style.height = bot - top + "px";
        continue;
      }
      if (scope === "item") {
        // No strip — the chip alone says which item — but the chip, the bullet
        // and the gap before the text open it.
        const band = itemBandX(pin.rects[0].page, pin.gutterX, pin.rects[0].x0);
        if (band) {
          const hit = stripFor(host, pin.uuid + ":ihit", "transparent");
          hit.style.left = band.x0 + "px";
          hit.style.top = boxes[0].y0 + "px";
          hit.style.width = Math.max(band.x1 - band.x0, 4) + "px";
          hit.style.height = Math.max(boxes[0].y1 - boxes[0].y0, 4) + "px";
          hit.style.pointerEvents = "auto";
          hit.style.cursor = "pointer";
          hit.__pin = pin;
          hit.onclick = (ev) => {
            ev.stopPropagation();
            if (openUuid === hit.__pin.uuid) closeAnnotBox();
            else showAnnot(hit.__pin);
            updateStacks();
          };
        }
        continue;
      }
      if (scope === "block") {
        const top = Math.min(...boxes.map((b) => b.y0));
        const bot = Math.max(...boxes.map((b) => b.y1));
        const left = Math.min(...boxes.map((b) => b.x0)) - 6;
        const el = stripFor(host, pin.uuid + ":block", color);
        el.style.width = STRIP_W + "px";
        el.style.left = left + "px";
        el.style.top = top + "px";
        el.style.height = Math.max(bot - top, 4) + "px";
        stripHit(host, pin, left, top, Math.max(bot - top, 4));
        continue;
      }
      // word / sentence / span: the highlighted text is itself clickable,
      // so an annotation can be opened by its content, not just its letter.
      // (Block and paragraph rects stay inert: they cover whole regions, and
      // clicking inside one should still start a new annotation.)
      boxes.forEach((b, idx) => {
        const hit = stripFor(host, pin.uuid + ":h" + idx, "transparent");
        hit.style.left = b.x0 + "px";
        hit.style.top = b.y0 + "px";
        hit.style.width = Math.max(b.x1 - b.x0, 2) + "px";
        hit.style.height = Math.max(b.y1 - b.y0, 2) + "px";
        hit.style.pointerEvents = "auto";
        hit.style.cursor = "pointer";
        hit.__pin = pin;
        hit.onclick = (ev) => {
          ev.stopPropagation();
          if (openUuid === hit.__pin.uuid) closeAnnotBox();
          else showAnnot(hit.__pin);
          updateStacks();
        };
      });
      boxes.forEach((b, idx) => {
        const el = stripFor(host, pin.uuid + ":u" + idx, color);
        el.style.height = "2px";
        el.style.left = b.x0 + "px";
        el.style.top = b.y1 + 1 + "px";
        el.style.width = Math.max(b.x1 - b.x0, 2) + "px";
      });
    }
    for (const el of host.querySelectorAll("." + STRIP_CLASS)) {
      if (!el.dataset.seen) el.remove();
    }
  };
  // The x of the chip lane (the page's right margin), used to line the
  // paragraph strip up with its letter chip.
  const laneLeftPx = (pin, pages) => {
    const page = pages[pin.page - 1];
    const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
    if (!ctm) return window.innerWidth - 64;
    return new DOMPoint(pin.pageWidth - 16, 0).matrixTransform(ctm).x - 13;
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
    const real = (lastData() && lastData().annotations) || [];
    const list = ghostPin ? real.concat([ghostPin]) : real;
    const pages = Array.from(findPages());
    drawDecorations(host, list, pages);
    const inline = [];
    const above = [];
    const below = [];
    for (const pin of list) {
      const page = pages[pin.page - 1];
      if (!(page instanceof SVGGraphicsElement)) continue;
      const ctm = page.getScreenCTM();
      if (!ctm) continue;
      // The chip's actual screen position (its center; the chip spans
      // m.y-10 .. m.y+10). Classify by its edges: a chip turns onto a row
      // when its edge comes within one row-gap (5px) of the corner chip,
      // the same spacing the rows use internally. Corner chips span
      // y 10..30 and (H-30)..(H-10).
      const m = new DOMPoint(pin.pageWidth - 16, pin.y - 4).matrixTransform(ctm);
      if (m.y - 10 < 35) above.push([m.y, pin]);
      else if (m.y + 10 > window.innerHeight - 35) below.push([m.y, pin]);
      else {
        const a = new DOMPoint(pin.x, pin.y).matrixTransform(ctm);
        inline.push([m, pin, a]);
      }
    }
    // Nearest off-screen annotation at the right corner, farther ones
    // extending leftward.
    above.sort((a, b) => b[0] - a[0]);
    below.sort((a, b) => a[0] - b[0]);
    const placed = new Map();
    // The rows continue leftward from the inline-chip lane at the page's
    // right margin. The nearest off-screen chip waits at the corner slot —
    // in the lane column itself, at row height — so the handoff between
    // vertical lane tracking and the horizontal row involves no sideways
    // jump; the turn happens one chip early.
    let laneLeft = window.innerWidth - 64;
    // Derive the lane x exactly as the inline chips do (the pin's precise
    // page width; the data-page-width attribute is rounded and drifts a
    // couple of pixels).
    const ref = list[0];
    const refPage = ref && pages[ref.page - 1];
    if (refPage instanceof SVGGraphicsElement) {
      const ctm = refPage.getScreenCTM();
      if (ctm) {
        laneLeft = new DOMPoint(ref.pageWidth - 16, 0).matrixTransform(ctm).x - 13;
      }
    }
    above.forEach(([, pin], idx) => {
      placed.set(pin.uuid, [pin, laneLeft - SLOT * idx, 10, "top"]);
    });
    below.forEach(([, pin], idx) => {
      placed.set(pin.uuid, [
        pin,
        laneLeft - SLOT * idx,
        window.innerHeight - 30,
        "bottom",
      ]);
    });
    // Chips whose anchors share a line would stack on top of each other in
    // the lane; fan them out so the line reads left-to-right in document
    // order, with the last chip of the line keeping the lane position.
    const bubbles = new Map();
    const rows = new Map();
    for (const item of inline) {
      const key = Math.round(item[0].y / 18);
      if (!rows.has(key)) rows.set(key, []);
      rows.get(key).push(item);
    }
    for (const group of rows.values()) {
      group.sort((a, b) => a[2].x - b[2].x);
      group.forEach(([m, pin, a], idx) => {
        const run = group.length - 1 - idx;
        placed.set(pin.uuid, [pin, m.x - 13 - run * SLOT, m.y - 10, "inline"]);
        bubbles.set(pin.uuid, [pin, a]);
      });
    }
    for (const el of [...host.children]) {
      if (el.dataset.strip !== undefined) {
        // Scope decorations are managed by drawDecorations.
      } else if (el.classList.contains(HOVER_CLASS)) {
        // Hover previews live and die with the pointer, not with the pins.
      } else if (el.dataset.buuid !== undefined) {
        if (!bubbles.has(el.dataset.buuid)) el.remove();
      } else if (!placed.has(el.dataset.uuid)) {
        el.remove();
      }
    }
    // Anchor bubbles: pointer chips riding directly on the anchored text,
    // apex on the baseline, same size as the margin chips.
    for (const [uuid, [pin, a]] of bubbles) {
      let el = host.querySelector(`[data-buuid="${CSS.escape(uuid)}"]`);
      if (!el) {
        el = document.createElement("div");
        el.dataset.buuid = uuid;
        el.style.cssText =
          "z-index:1;position:absolute;pointer-events:auto;cursor:pointer;line-height:0";
        el.onclick = (ev) => {
          ev.stopPropagation();
          if (openUuid === el.__pin.uuid) closeAnnotBox();
          else showAnnot(el.__pin);
          updateStacks();
        };
        host.appendChild(el);
      }
      el.__pin = pin;
      if (STRIPPED_SCOPES.has(pin.scope)) {
        renderLetterGlyph(el, pin);
        const boxes = (pin.rects || [])
          .map((rr) => {
            const page = pages[rr.page - 1];
            const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
            if (!ctm) return null;
            const p0 = new DOMPoint(rr.x0, rr.y0).matrixTransform(ctm);
            const p1 = new DOMPoint(rr.x1, rr.y1).matrixTransform(ctm);
            return { x0: p0.x, y0: p0.y, x1: p1.x, y1: p1.y };
          })
          .filter(Boolean);
        if (boxes.length) {
          if (pin.scope === "block" || pin.scope === "para") {
            // The strip runs down the rect's left edge; sit left of it.
            const top = Math.min(...boxes.map((b) => b.y0));
            const bot = Math.max(...boxes.map((b) => b.y1));
            const left =
              pin.scope === "para"
                ? bandOf(boxes, pin.rects[0].page, pin.gutterX, pin.railX).left + STRIP_W
                : Math.min(...boxes.map((b) => b.x0));
            el.style.left = left - 10 - el.__w + "px";
            el.style.top = (top + bot) / 2 - el.__h / 2 + "px";
          } else {
            // Underline: tuck the letter below the strip, right-aligned
            // with its end, so it sits in the descender gap and cannot
            // collide with a neighbouring underline on the same line.
            const last = boxes[boxes.length - 1];
            // The glyph's own right edge (2px inside its box) lands on the
            // strip's end, so letter and underline finish flush.
            el.style.left = last.x1 - el.__w + 2 + "px";
            el.style.top = last.y1 + 3 + "px";
          }
        }
        continue;
      }
      const block = pin.scope === "item";
      renderBubble(el, pin, block ? "right" : "up");
      if (block) {
        // Item markers clear the bullet/number they point at; block
        // markers sit just left of their rect.
        const gap = pin.scope === "item" ? 7 : 3;
        el.style.left = a.x - el.__w - gap + "px";
        el.style.top = a.y - el.__h / 2 + "px";
      } else {
        el.style.left = a.x - el.__w / 2 + "px";
        el.style.top = a.y - 2 + "px";
      }
    }
    for (const [uuid, [pin, x, y, state]] of placed) {
      let el = host.querySelector(`[data-uuid="${CSS.escape(uuid)}"]`);
      let fresh = false;
      if (!el) {
        el = stackSquare(pin);
        fresh = true;
        host.appendChild(el);
      }
      el.__pin = pin;
      el.textContent = (pin.letter || "?").toUpperCase();
      el.title = (pin.author ? pin.author + ": " : "") + (pin.content || "");
      const selected = pin.uuid === openUuid;
      el.style.background = iconColor(pin.status, selected);
      el.style.color = selected
        ? "#ffffff"
        : STATUS_LETTER[pin.status] || STATUS_LETTER.created;
      el.style.boxShadow = selected
        ? `0 0 6px 2px ${statusColor(pin.status)}, 0 0 12px 3px ${statusColor(pin.status)}66`
        : "";
      // Inline chips track the scroll position directly; state changes
      // (inline <-> edge rows, row reshuffles) animate.
      const tracking = state === "inline" && el.dataset.state === "inline";
      el.style.transition =
        fresh || tracking ? "none" : "left 0.25s ease,top 0.25s ease";
      el.dataset.state = state;
      el.style.left = x + "px";
      el.style.top = y + "px";
    }
  };
  document.addEventListener("scroll", updateStacks, { capture: true, passive: true });
  window.addEventListener("resize", updateStacks);
  // Up/down arrows walk the annotations in document order — selecting,
  // scrolling to, and opening each — whenever no reply is being typed
  // (no window open, or its field still empty).
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      const box = document.getElementById(ANNOT_BOX_ID);
      const ta = box && box.querySelector("textarea");
      if (ta && ta.value) return;
      const list = ((lastData() && lastData().annotations) || []).slice();
      if (!list.length) return;
      list.sort((a, b) => a.page - b.page || a.y - b.y);
      let idx = list.findIndex((p) => p.uuid === openUuid);
      if (e.key === "ArrowDown") idx = idx < 0 ? 0 : Math.min(idx + 1, list.length - 1);
      else idx = idx < 0 ? list.length - 1 : Math.max(idx - 1, 0);
      e.preventDefault();
      e.stopPropagation();
      const pin = list[idx];
      scrollToPin(pin);
      showAnnot(pin);
      updateStacks();
    },
    true,
  );

  // Driven by the SSE payloads relayed from error_overlay.js.
  loadLayout();
  window.__tinymistAnnot = (data) => {
    loadLayout();
    updateStacks();
    refreshOpenAnnot(data);
  };
  setInterval(updateStacks, 1000);
})();
