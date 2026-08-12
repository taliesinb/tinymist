// Annotation UI for the tinymist preview: anchor bubbles, the chip
// conveyor, and the annotation window. Loaded by error_overlay.js only on
// /?annotate pages; plain previews carry none of this.
(() => {
  const api = window.__tinymist;
  if (!api) return;
  const { findPages, docDark, lastData } = api;
  const SVG_NS = "http://www.w3.org/2000/svg";
  const ANNOTATE = new URLSearchParams(location.search).has("annotate");

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
  const PROBE_CLASS = "tinymist-probe-caret";
  const clearProbeCaret = () => {
    document.querySelectorAll("." + PROBE_CLASS).forEach((el) => el.remove());
    probeState = null;
  };
  // The preview marker for a new annotation: the same pointer bubble the
  // real annotations use, carrying the letter it is about to get.
  let probeState = null;
  const drawProbeBubble = (pageNo, x, y, letter, kind) => {
    clearProbeCaret();
    probeState = { pageNo, x, y, letter, kind };
    const page = Array.from(findPages())[pageNo - 1];
    if (!(page instanceof SVGGraphicsElement)) return;
    const ctm = page.getScreenCTM();
    if (!ctm) return;
    const host = document.getElementById(STACK_ID);
    if (!host) return;
    const el = document.createElement("div");
    el.className = PROBE_CLASS;
    el.style.cssText = "position:absolute;pointer-events:none;line-height:0";
    const block = kind === "block";
    renderBubble(
      el,
      { uuid: "\u0000probe", letter, status: "created" },
      block ? "right" : "up",
    );
    positionBubble(el, ctm, x, y, block);
    host.appendChild(el);
  };
  // Places a bubble at a document point: block markers sit left of the
  // block and vertically centered, text markers above the baseline.
  const positionBubble = (el, ctm, x, y, block) => {
    const p = new DOMPoint(x, y).matrixTransform(ctm);
    if (block) {
      el.style.left = p.x - el.__w - 3 + "px";
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
    openUuid = null;
    openSig = null;
    clearProbeCaret();
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
      drawProbeBubble(probe.page, probe.x, probe.y, letter, probe.kind);
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
  document.addEventListener(
    "click",
    (ev) => {
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
      "pointer-events:auto;position:absolute;display:block;min-width:22px;height:20px;" +
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
    const color = statusColor(pin.status);
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
    // The selected annotation's icon glows in its status color.
    el.style.filter = selected
      ? `drop-shadow(0 0 3px ${color}) drop-shadow(0 0 7px ${color})`
      : "";
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
      tx = (left + right) / 2;
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
  const updateStacks = () => {
    let host = document.getElementById(STACK_ID);
    if (!host) {
      host = document.createElement("div");
      host.id = STACK_ID;
      host.style.cssText =
        "position:fixed;inset:0;pointer-events:none;z-index:2147483645";
      document.body.appendChild(host);
    }
    const list = (lastData() && lastData().annotations) || [];
    const pages = Array.from(findPages());
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
      if (el.classList.contains(PROBE_CLASS)) {
        // The compose preview marker is managed separately.
      } else if (el.dataset.buuid !== undefined) {
        if (!bubbles.has(el.dataset.buuid)) el.remove();
      } else if (!placed.has(el.dataset.uuid)) {
        el.remove();
      }
    }
    // The compose preview marker rides the document like the real ones.
    if (probeState) {
      const el = document.querySelector("." + PROBE_CLASS);
      const page = pages[probeState.pageNo - 1];
      const ctm = page instanceof SVGGraphicsElement && page.getScreenCTM();
      if (el && ctm) {
        positionBubble(el, ctm, probeState.x, probeState.y, probeState.kind === "block");
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
          "position:absolute;pointer-events:auto;cursor:pointer;line-height:0";
        el.onclick = (ev) => {
          ev.stopPropagation();
          if (openUuid === el.__pin.uuid) closeAnnotBox();
          else showAnnot(el.__pin);
          updateStacks();
        };
        host.appendChild(el);
      }
      el.__pin = pin;
      const block = pin.kind === "block";
      renderBubble(el, pin, block ? "right" : "up");
      if (block) {
        el.style.left = a.x - el.__w - 3 + "px";
        el.style.top = a.y - el.__h / 2 + "px";
      } else {
        el.style.left = a.x - el.__w / 2 + "px";
        el.style.top = a.y - 2 + "px";
      }
      void positionBubble;
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
      el.style.background = statusColor(pin.status);
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
  window.__tinymistAnnot = (data) => {
    updateStacks();
    refreshOpenAnnot(data);
  };
  setInterval(updateStacks, 1000);
})();
