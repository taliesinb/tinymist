// Annotation UI for HTML-mode documents.
//
// A fork of the paged annotator (annotations.js), not a generalisation of it.
// The paged one annotates a picture: it asks the server for rectangles, maps
// page coordinates through an SVG transform, and hit-tests boxes. Here the
// document is real HTML, so the browser already knows where every word is —
// the server only says which part of the source each piece came from, and all
// geometry comes from DOM ranges.
//
// The two share the sidecar format, the anchor labels, and the mutation
// endpoints (/dev/annotate*), which are about annotations rather than about
// how a document is drawn. Nothing else.
(() => {
  const SRC_ATTR = "data-typst-src";
  const TEXT_ATTR = "data-typst-text";
  const DOC_ID = "tinymist-doc";
  const MARKS_ID = "tinymist-marks";
  const BOX_ID = "tinymist-annot-box";
  const STATUS_ID = "tinymist-status";
  const ANNOTATE = location.pathname.replace(/\/+$/, "") === "/annotate";

  // ---------------------------------------------------------------- palette
  // Status colours, luminance-matched, as in the paged mode: created = green,
  // ongoing = purple, resolved = blue-gray.
  const STATUS_COLOR = { created: "#2f9e44", ongoing: "#a476f5", resolved: "#6c7d91" };
  const STATUS_BG = { created: "#0d2616", ongoing: "#1e1330", resolved: "#171e28" };
  const STATUS_TEXT = { created: "#b8d4be", ongoing: "#cabfe2", resolved: "#b9c4d2" };
  const STATUS_AUTHOR = { created: "#ddeee1", ongoing: "#e6dff5", resolved: "#dde5ee" };
  const STATUS_LETTER = { created: "#a5d4b1", ongoing: "#c6b3ee", resolved: "#bcc9d8" };
  const statusColor = (status) => STATUS_COLOR[status] || STATUS_COLOR.created;
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
  const pageIsDark = () =>
    matchMedia && matchMedia("(prefers-color-scheme: dark)").matches;
  const haloColor = () => (pageIsDark() ? "#0e0e0e" : "#ffffff");

  // ------------------------------------------------------------------- http
  const post = (path, payload) =>
    fetch(path, { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .then((r) => {
        if (!r.ok) console.warn("tinymist annotation:", r.error);
        return r;
      })
      .catch((e) => {
        console.warn("tinymist annotation:", e);
        return { ok: false, error: String(e) };
      });
  const getJson = (path) =>
    fetch(path)
      .then((r) => r.json())
      .catch((e) => ({ ok: false, error: String(e) }));

  const freshUuid = () => {
    const rand = crypto.getRandomValues(new Uint8Array(2));
    return Array.from(rand, (b) => b.toString(16).padStart(2, "0").toUpperCase()).join("");
  };

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

  // Display letters (bijective base 26), mirroring the server's assignment so
  // a compose window can name itself before the annotation exists.
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
  const nextLetter = () =>
    indexLetter(Math.max(0, ...pins.map((p) => letterIndex(p.letter))) + 1);

  // ------------------------------------------------------- the source index
  // Every text run the server emitted is wrapped in a span carrying the source
  // range it came from, so a source offset and a place in the document are the
  // same thing said two ways. Everything below — where a pin's mark goes, what
  // a click would annotate — is a lookup in this index.
  let pins = [];
  let runs = []; // { el, node, s, e, text, exact }
  let blocks = []; // { el, s, e, kind }
  const BLOCK_TAGS = {
    LI: "item",
    P: "para",
    H1: "block",
    H2: "block",
    H3: "block",
    H4: "block",
    H5: "block",
    H6: "block",
    BLOCKQUOTE: "block",
    PRE: "block",
    FIGURE: "block",
    TABLE: "block",
  };

  const parseRange = (el, attr) => {
    const raw = el.getAttribute(attr);
    if (!raw) return null;
    const [s, e] = raw.split(":").map((n) => parseInt(n, 10));
    return Number.isFinite(s) && Number.isFinite(e) ? { s, e } : null;
  };

  const indexDocument = () => {
    runs = [];
    blocks = [];
    const doc = document.getElementById(DOC_ID);
    if (!doc) return;
    // Runs of text carry their own range; whether it sits on a wrapper the
    // server added or on the element that already held the text is not this
    // side's business.
    for (const el of doc.querySelectorAll("[" + TEXT_ATTR + "]")) {
      const range = parseRange(el, TEXT_ATTR);
      const only = el.childNodes.length === 1 ? el.firstChild : null;
      if (range && only && only.nodeType === Node.TEXT_NODE) {
        const text = only.nodeValue || "";
        runs.push({
          el,
          node: only,
          s: range.s,
          e: range.e,
          text,
          // A run whose source is exactly as long as its text maps character
          // for character; anything else (escapes, smart quotes, a run the
          // compiler assembled) is mapped by clamping, which still lands in
          // the right run.
          exact: range.e - range.s === text.length,
        });
      }
    }
    for (const el of doc.querySelectorAll("[" + SRC_ATTR + "]")) {
      const range = parseRange(el, SRC_ATTR);
      const kind = range && BLOCK_TAGS[el.tagName];
      if (kind) blocks.push({ el, s: range.s, e: range.e, kind });
    }
    runs.sort((a, b) => a.s - b.s || a.e - b.e);
    // An element's own range says where it began, not how far it reaches: a
    // label written into a paragraph ends the paragraph's span there. What it
    // covers is what its text covers, so that is measured from the runs
    // inside it.
    for (const block of blocks) {
      const inside = runs.filter((run) => block.el.contains(run.node));
      block.lo = Math.min(block.s, ...inside.map((r) => r.s));
      block.hi = Math.max(block.e, ...inside.map((r) => r.e));
    }
    blocks.sort((a, b) => a.lo - b.lo || b.hi - a.hi);
  };

  // The run an offset falls in: the innermost, shortest run that covers it.
  const runAt = (offset) => {
    let best = null;
    for (const run of runs) {
      if (offset < run.s || offset > run.e) continue;
      if (!best || run.e - run.s < best.e - best.s) best = run;
    }
    return best;
  };
  const charIn = (run, offset) =>
    Math.max(0, Math.min(run.exact ? offset - run.s : run.text.length, run.text.length));

  // A DOM position for a source offset, and a DOM range for a source range.
  const pointAt = (offset) => {
    const run = runAt(offset);
    if (!run) return null;
    return { node: run.node, index: charIn(run, offset), run };
  };
  const rangeFor = (from, to) => {
    const a = pointAt(from);
    const b = pointAt(to);
    if (!a || !b) return null;
    const range = document.createRange();
    try {
      range.setStart(a.node, a.index);
      range.setEnd(b.node, b.index);
    } catch (err) {
      return null;
    }
    return range.collapsed && from !== to ? null : range;
  };
  const rectsOf = (range) =>
    range
      ? Array.from(range.getClientRects()).filter((r) => r.width > 0 || r.height > 0)
      : [];

  // The word ending at an offset: what a word anchor names, since the label is
  // written just past the word it belongs to.
  const wordEndingAt = (offset) => {
    const run = runAt(offset);
    if (!run) return null;
    const at = charIn(run, offset);
    let end = at;
    while (end > 0 && /\s/.test(run.text[end - 1])) end -= 1;
    let start = end;
    while (start > 0 && !/\s/.test(run.text[start - 1])) start -= 1;
    if (start === end) return null;
    return { run, start, end, s: run.s + start, e: run.s + end };
  };
  // The word around a character index, for a click.
  const wordAround = (run, at) => {
    const text = run.text;
    if (!text.trim()) return null;
    let start = Math.min(at, text.length - 1);
    if (/\s/.test(text[start])) return null;
    let end = start;
    while (start > 0 && !/\s/.test(text[start - 1])) start -= 1;
    while (end < text.length && !/\s/.test(text[end])) end += 1;
    return { run, start, end, s: run.s + start, e: run.s + end };
  };

  // Where the pointer is, in document terms. Browsers disagree on the name of
  // this, and on nothing else about it.
  const caretAt = (x, y) => {
    let node = null;
    let offset = 0;
    if (document.caretRangeFromPoint) {
      const range = document.caretRangeFromPoint(x, y);
      if (range) {
        node = range.startContainer;
        offset = range.startOffset;
      }
    } else if (document.caretPositionFromPoint) {
      const pos = document.caretPositionFromPoint(x, y);
      if (pos) {
        node = pos.offsetNode;
        offset = pos.offset;
      }
    }
    if (!node || node.nodeType !== Node.TEXT_NODE) return null;
    const run = runs.find((r) => r.node === node);
    return run ? { run, at: offset } : null;
  };

  // The innermost block that covers an offset — an item beats the paragraph
  // it sits in, a heading beats the section around it.
  const blockCovering = (offset) => {
    let best = null;
    for (const b of blocks) {
      if (offset < b.lo || offset > b.hi) continue;
      if (!best || b.hi - b.lo < best.hi - best.lo) best = b;
    }
    return best;
  };

  // ------------------------------------------------------------- the marks
  const marksHost = () => {
    let host = document.getElementById(MARKS_ID);
    if (!host) {
      host = document.createElement("div");
      host.id = MARKS_ID;
      document.body.appendChild(host);
    }
    return host;
  };
  const mark = (host, key, cls) => {
    let el = host.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (!el) {
      el = document.createElement("div");
      el.dataset.key = key;
      el.className = cls;
      host.appendChild(el);
    }
    el.dataset.seen = "1";
    return el;
  };
  const place = (el, x, y, w, h) => {
    el.style.left = x + "px";
    el.style.top = y + "px";
    if (w != null) el.style.width = Math.max(w, 1) + "px";
    if (h != null) el.style.height = Math.max(h, 1) + "px";
  };
  const openFor = (el, pin) => {
    el.__pin = pin;
    el.onclick = (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      if (openUuid === el.__pin.uuid) closeBox();
      else showAnnot(el.__pin);
      render();
    };
  };

  // Geometry of a pin: the boxes its mark hangs off, and how it is drawn.
  //   word / sentence / span : an underline beneath the text, letter at its end
  //   block / para           : a strip down the left of the region, letter at its head
  //   item                   : a chip pointing right at the item's marker
  //   point                  : a chevron between two words
  const STRIP_W = 3;
  const STRIP_GAP = 10;
  // How far a region's strip sits left of the text column, past anything the
  // document itself hangs in that margin (a bullet, a number).
  const RAIL_GAP = 26;

  const geometryOf = (pin) => {
    const scope = pin.scope || "point";
    if (scope === "span") {
      const range = rangeFor(pin.start, pin.end);
      return { scope, boxes: rectsOf(range) };
    }
    if (scope === "word" || scope === "sentence") {
      const word = wordEndingAt(pin.start);
      if (!word) return null;
      const range = rangeFor(word.s, word.e);
      return { scope, boxes: rectsOf(range) };
    }
    if (scope === "point") {
      const point = pointAt(pin.start);
      if (!point) return null;
      const range = document.createRange();
      range.setStart(point.node, point.index);
      range.setEnd(point.node, point.index);
      const box = range.getBoundingClientRect();
      return { scope, boxes: [box], caret: box };
    }
    // A region: the box of the element the anchor landed in.
    const block = blockCovering(pin.start);
    if (!block) return null;
    return { scope, boxes: [block.el.getBoundingClientRect()], block };
  };

  // A region's strip sits in the margin, left of the text and left of anything
  // the document hangs there, so item chips and paragraph strips never collide.
  const stripLeftOf = (block, boxes) => {
    const own = Math.min(...boxes.map((b) => b.left));
    if (!block || block.kind !== "para") return own - STRIP_GAP;
    return own - RAIL_GAP;
  };

  const drawPin = (host, pin, geom, dim) => {
    if (!geom || !geom.boxes.length) return;
    const color = iconColor(pin.status, pin.uuid === openUuid);
    const key = pin.uuid;
    const opacity = dim ? 0.5 : 1;
    const { scope, boxes } = geom;
    if (scope === "block" || scope === "para" || scope === "item") {
      const top = Math.min(...boxes.map((b) => b.top));
      const bot = Math.max(...boxes.map((b) => b.bottom));
      if (scope === "item") {
        // An item wears a chip beside its marker, never a strip: the marker is
        // already a vertical cue and a second one reads as clutter.
        const el = mark(host, key + ":chip", "tm-chip");
        el.style.opacity = opacity;
        drawBubble(el, pin, "right");
        place(el, boxes[0].left - el.__w - 8, (top + bot) / 2 - el.__h / 2);
        if (!dim) openFor(el, pin);
        return;
      }
      const left = stripLeftOf(geom.block, boxes);
      const strip = mark(host, key + ":strip", "tm-mark");
      strip.style.background = color;
      strip.style.opacity = opacity;
      place(strip, left, top, STRIP_W, Math.max(bot - top, 4));
      const glyph = mark(host, key + ":letter", "tm-glyph");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      // Right-aligned against the strip, so the letter reads as its label.
      place(glyph, left - 3 - glyph.__w, (top + bot) / 2 - glyph.__h / 2);
      if (!dim) {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, left - 30, top, 30 + STRIP_W + 5, Math.max(bot - top, 4));
        openFor(hit, pin);
      }
      return;
    }
    if (scope === "point") {
      const el = mark(host, key + ":point", "tm-glyph");
      el.style.opacity = opacity;
      drawChevron(el, color);
      place(el, boxes[0].left - 6, boxes[0].bottom + 1);
      if (!dim) {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, boxes[0].left - 8, boxes[0].top, 16, boxes[0].height);
        openFor(hit, pin);
      }
      return;
    }
    // Text scopes: an underline under every line the text covers, and the
    // letter tucked below the end of the last one.
    boxes.forEach((b, idx) => {
      const el = mark(host, key + ":u" + idx, "tm-mark");
      el.style.background = color;
      el.style.opacity = opacity;
      place(el, b.left, b.bottom + 1, b.width, 2);
      if (dim) return;
      const hit = mark(host, key + ":h" + idx, "tm-hit");
      place(hit, b.left, b.top, b.width, b.height);
      openFor(hit, pin);
    });
    const last = boxes[boxes.length - 1];
    const glyph = mark(host, key + ":letter", "tm-glyph");
    glyph.style.opacity = opacity;
    drawLetter(glyph, pin);
    place(glyph, last.right - glyph.__w + 2, last.bottom + 3);
  };

  const drawLetter = (el, pin) => {
    const letter = (pin.letter || "").toUpperCase();
    const selected = pin.uuid === openUuid;
    const fill = selected ? "#ffffff" : iconColor(pin.status, selected);
    const halo = haloColor();
    const w = Math.max(10, 6 * Math.max(letter.length, 1) + 4);
    const h = 12;
    el.__w = w;
    el.__h = h;
    const sig = ["glyph", letter, fill, halo].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    el.innerHTML =
      `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
      `<text x="${w - 2}" y="${h / 2 + 3}" text-anchor="end" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace" fill="${fill}"` +
      ` style="paint-order:stroke;stroke:${halo};stroke-width:3px;stroke-linejoin:round">` +
      `${letter}</text></svg>`;
  };

  // The pointer chip: a rounded body with an apex, drawn in the direction it
  // points. Same shape as the paged mode's, so the two modes read alike.
  const drawBubble = (el, pin, dir) => {
    const letter = (pin.letter || "").toUpperCase();
    const selected = pin.uuid === openUuid;
    const w = Math.max(15, 5 + 6 * Math.max(letter.length, 1));
    const tri = 6;
    const h = 11;
    const r = 3.5;
    const pad = 2;
    const color = iconColor(pin.status, selected);
    const border = haloColor();
    const fill = selected ? "#ffffff" : STATUS_LETTER[pin.status] || STATUS_LETTER.created;
    let svgW;
    let svgH;
    let path;
    let tx;
    let ty;
    if (dir === "right") {
      svgW = pad * 2 + h + tri;
      svgH = pad * 2 + w;
      const left = pad;
      const right = pad + h;
      const top = pad;
      const bot = pad + w;
      const cy = (top + bot) / 2;
      path =
        `M ${right + tri} ${cy} L ${right} ${top} L ${left + r} ${top}` +
        ` Q ${left} ${top} ${left} ${top + r} L ${left} ${bot - r}` +
        ` Q ${left} ${bot} ${left + r} ${bot} L ${right} ${bot} Z`;
      tx = (left + right) / 2 + 1.5;
      ty = cy + 3.5;
    } else {
      svgW = w + pad * 2;
      svgH = tri + h + pad * 2;
      const cx = pad + w / 2;
      const bot = pad + tri + h;
      path =
        `M ${cx} ${pad} L ${pad + w} ${pad + tri} L ${pad + w} ${bot - r}` +
        ` Q ${pad + w} ${bot} ${pad + w - r} ${bot} L ${pad + r} ${bot}` +
        ` Q ${pad} ${bot} ${pad} ${bot - r} L ${pad} ${pad + tri} Z`;
      tx = cx;
      ty = pad + tri + h / 2 + 1;
    }
    el.__w = svgW;
    el.__h = svgH;
    const sig = ["bubble", letter, color, border, fill, dir].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    el.innerHTML =
      `<svg width="${svgW}" height="${svgH}" viewBox="0 0 ${svgW} ${svgH}">` +
      `<path d="${path}" fill="${color}" stroke="${border}" stroke-width="2"` +
      ` stroke-linejoin="round"></path>` +
      `<text x="${tx}" y="${ty}" text-anchor="middle" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace" fill="${fill}">${letter}</text>` +
      `</svg>`;
  };

  const drawChevron = (el, color) => {
    const w = 12;
    const h = 7;
    el.__w = w;
    el.__h = h;
    if (el.dataset.sig === "chevron|" + color) return;
    el.dataset.sig = "chevron|" + color;
    el.innerHTML =
      `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
      `<path d="M 1 ${h - 1} L ${w / 2} 1 L ${w - 1} ${h - 1}" fill="none"` +
      ` stroke="${color}" stroke-width="2" stroke-linecap="round"` +
      ` stroke-linejoin="round"></path></svg>`;
  };

  // Off-screen annotations ride the edges of the viewport: one above the top
  // edge for everything scrolled past, one below for everything still to come,
  // so nothing in the document is out of reach.
  const SLOT = 27;
  const drawEdgeChips = (host, placements) => {
    for (const [pin, x, y, state] of placements) {
      const el = mark(host, pin.uuid + ":edge", "tm-edge");
      if (el.tagName !== "DIV") continue;
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
      el.style.left = x + "px";
      el.style.top = y + "px";
      el.dataset.state = state;
      el.__pin = pin;
      el.onclick = (ev) => {
        ev.stopPropagation();
        scrollToPin(el.__pin);
        showAnnot(el.__pin);
        render();
      };
    }
  };

  const scrollToPin = (pin) => {
    const geom = geometryOf(pin);
    if (!geom || !geom.boxes.length) return;
    const top = Math.min(...geom.boxes.map((b) => b.top));
    window.scrollTo({
      top: window.scrollY + top - window.innerHeight / 2,
      behavior: "smooth",
    });
  };

  // The whole overlay, redrawn from the pins and the document's current
  // geometry. Cheap enough to run on every scroll: it is a few dozen boxes.
  let ghost = null;
  const render = () => {
    const host = marksHost();
    for (const el of host.children) el.dataset.seen = "";
    const list = ghost ? pins.concat([ghost]) : pins;
    const edge = [];
    for (const pin of list) {
      const geom = geometryOf(pin);
      if (!geom || !geom.boxes.length) continue;
      const top = Math.min(...geom.boxes.map((b) => b.top));
      const bot = Math.max(...geom.boxes.map((b) => b.bottom));
      if (bot < 34 || top > window.innerHeight - 34) {
        edge.push([pin, top]);
        continue;
      }
      drawPin(host, pin, geom, pin === ghost);
    }
    const above = edge.filter(([, top]) => top < 0).sort((a, b) => b[1] - a[1]);
    const below = edge.filter(([, top]) => top >= 0).sort((a, b) => a[1] - b[1]);
    const lane = Math.max(window.innerWidth - 64, 8);
    drawEdgeChips(
      host,
      above
        .map(([pin], idx) => [pin, lane - SLOT * idx, 10, "top"])
        .concat(below.map(([pin], idx) => [pin, lane - SLOT * idx, window.innerHeight - 30, "bottom"])),
    );
    for (const el of [...host.children]) {
      if (!el.dataset.seen && !el.classList.contains(HOVER_CLASS)) el.remove();
    }
  };

  // ------------------------------------------------------- hover previews
  // What a click would make, drawn as the thing it would make: the same strip,
  // the same chip, the same underline, pale and letterless.
  const HOVER_CLASS = "tm-hover";
  const clearHover = () => {
    for (const el of document.querySelectorAll("." + HOVER_CLASS)) el.remove();
  };
  const hoverMark = (host, cls) => {
    const el = document.createElement("div");
    el.className = cls + " " + HOVER_CLASS;
    el.dataset.seen = "1";
    host.appendChild(el);
    return el;
  };
  const previewUnderline = (boxes, strong) => {
    clearHover();
    const host = marksHost();
    for (const b of boxes) {
      const el = hoverMark(host, "tm-mark");
      el.style.background = statusColor("created");
      el.style.opacity = strong ? 0.85 : 0.45;
      place(el, b.left, b.bottom + 1, b.width, 2);
    }
  };
  const previewRegion = (block) => {
    clearHover();
    const host = marksHost();
    const box = block.el.getBoundingClientRect();
    if (block.kind === "item") {
      const el = hoverMark(host, "tm-chip");
      el.style.opacity = 0.55;
      el.style.pointerEvents = "none";
      drawBubble(el, { uuid: "hover", letter: "", status: "created" }, "right");
      place(el, box.left - el.__w - 8, box.top + box.height / 2 - el.__h / 2);
      return;
    }
    const left = stripLeftOf(block, [box]);
    const el = hoverMark(host, "tm-mark");
    el.style.background = statusColor("created");
    el.style.opacity = 0.5;
    place(el, left, box.top, STRIP_W, Math.max(box.height, 4));
  };
  const previewPoint = (box) => {
    clearHover();
    const host = marksHost();
    const el = hoverMark(host, "tm-glyph");
    el.style.opacity = 0.8;
    drawChevron(el, statusColor("created"));
    place(el, box.left - 6, box.bottom + 1);
  };

  // The margin band that creates a region annotation: exactly the ground its
  // strip (or its chip) would cover, so what is previewed is what is made, and
  // a region that already has one cannot be given a second.
  const regionZoneAt = (x, y) => {
    let best = null;
    for (const block of blocks) {
      const box = block.el.getBoundingClientRect();
      if (y < box.top - 2 || y > box.bottom + 2) continue;
      let x0;
      let x1;
      if (block.kind === "item") {
        x0 = box.left - 40;
        x1 = box.left - 2;
      } else {
        const left = stripLeftOf(block, [box]);
        x0 = left - 30;
        x1 = left + STRIP_W + 5;
      }
      if (x < x0 || x > x1) continue;
      if (!best || block.hi - block.lo < best.hi - best.lo) best = block;
    }
    return best;
  };

  // ------------------------------------------------- the annotation window
  let openUuid = null;
  let openSig = null;
  let composeActive = false;
  let escHandler = null;

  const draftKey = (pin) => `tinymist-html-draft:${pin.uuid}:${pin.time}`;
  const loadDraft = (key) => {
    try {
      return (key && localStorage.getItem(key)) || "";
    } catch (err) {
      return "";
    }
  };
  const saveDraft = (key, value) => {
    if (!key) return;
    try {
      if (value.trim()) localStorage.setItem(key, value);
      else localStorage.removeItem(key);
    } catch (err) {}
  };

  const closeBox = (force) => {
    const box = document.getElementById(BOX_ID);
    if (box && composeActive && !force) {
      const ta = box.querySelector("textarea");
      if (ta && ta.value.trim() && !confirm("Discard new comment?")) return false;
    }
    if (box) box.remove();
    if (escHandler) {
      document.removeEventListener("keydown", escHandler, true);
      escHandler = null;
    }
    composeActive = false;
    ghost = null;
    openUuid = null;
    openSig = null;
    render();
    return true;
  };

  const shell = (status, letter, stateText, acts) => {
    if (!closeBox()) return null;
    const box = document.createElement("div");
    box.id = BOX_ID;
    box.style.background = STATUS_BG[status] || STATUS_BG.created;
    for (const type of ["keydown", "keyup", "keypress"]) {
      box.addEventListener(type, (e) => e.stopPropagation());
    }
    escHandler = (e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        closeBox();
      }
    };
    document.addEventListener("keydown", escHandler, true);

    const title = document.createElement("div");
    title.className = "ta-title";
    title.style.background = statusColor(status);
    const letterEl = document.createElement("span");
    letterEl.style.cssText =
      "font:bold 12px ui-monospace,monospace;color:white;text-transform:uppercase";
    letterEl.textContent = letter;
    const right = document.createElement("span");
    right.style.cssText =
      "margin-left:auto;display:inline-flex;align-items:baseline;gap:5px;font-size:11px";
    const actsEl = document.createElement("span");
    actsEl.className = "ta-acts";
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
    content.className = "ta-body";
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
    ta.style.color = STATUS_TEXT[status] || STATUS_TEXT.created;
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
        closeBox();
      }
    });
    wrap.append(ta, hintEl);
    return { wrap, ta };
  };

  const pinSig = (pin) =>
    JSON.stringify([pin.status, pin.type, pin.letter, pin.author, pin.time, pin.content,
                    pin.discussion]);
  const currentDraft = () => {
    const box = document.getElementById(BOX_ID);
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
    if (status === "resolved") acts.push(["reopen", () => changeStatus(pin, "created")]);
    else acts.push(["resolve", () => changeStatus(pin, "resolved")]);
    acts.push([
      "delete",
      () => {
        post("/dev/annotate/delete", { uuid: pin.uuid }).then(refresh);
        saveDraft(draftKey(pin), "");
        closeBox(true);
      },
    ]);
    const parts = shell(status, pin.letter || "?", `${status} ${pin.type || "comment"}`, acts);
    if (!parts) return;
    openUuid = pin.uuid;
    openSig = pinSig(pin);
    parts.content.append(msgRow(status, pin.author, pin.time, pin.content, true));
    for (const reply of pin.discussion || []) {
      parts.content.append(msgRow(status, reply.author, reply.time, reply.content, false));
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
        post("/dev/annotate/reply", { uuid: pin.uuid, text }).then(refresh);
        if (status === "resolved") {
          post("/dev/annotate/status", { uuid: pin.uuid, status: "ongoing" });
        }
        field.value = "";
        field.dispatchEvent(new Event("input"));
        saveDraft(draftKey(pin), "");
      },
    );
    parts.content.append(wrap);
    ta.__persistKey = draftKey(pin);
    const persisted = loadDraft(ta.__persistKey);
    if (restore || persisted) {
      ta.value = (restore && restore.draft) || persisted || "";
      ta.dispatchEvent(new Event("input"));
      if (!restore || restore.focused) {
        ta.focus();
        ta.setSelectionRange(ta.value.length, ta.value.length);
      }
    } else {
      ta.focus();
    }
    render();
  };

  // Rebuilds the open window in place when its annotation changes underneath
  // it (a reply, a status flip, an agent's edit); closes it if it was deleted.
  const refreshOpen = () => {
    if (!openUuid) return;
    if (!document.getElementById(BOX_ID)) {
      openUuid = null;
      return;
    }
    const pin = pins.find((p) => p.uuid === openUuid);
    if (!pin) return void closeBox(true);
    if (pinSig(pin) !== openSig) showAnnot(pin, currentDraft());
  };

  // --------------------------------------------------------------- compose
  // A composed annotation is shown as a real one — same strip, same chip, same
  // letter — so the window always has a visible host to belong to.
  const compose = (kind, payload, ghostPin) => {
    const letter = nextLetter();
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        post("/dev/annotate", { uuid: freshUuid(), text, ...payload }).then(refresh);
      }
      closeBox(true);
    };
    const parts = shell("created", letter, `new ${kind}`, [
      ["save", () => submit(document.querySelector(`#${BOX_ID} textarea`))],
      ["cancel", () => closeBox()],
    ]);
    if (!parts) return;
    composeActive = true;
    const { wrap, ta } = replyField("created", "Type comment", "⇧⏎ save", 3, true, submit);
    parts.content.append(wrap);
    ta.focus();
    ghost = { uuid: "tinymist-ghost", status: "created", letter, ...ghostPin };
    render();
  };

  // ----------------------------------------------------------- interaction
  let drag = null;
  let swallowClick = false;
  const onOverlay = (ev) =>
    ev.target &&
    ev.target.closest &&
    ev.target.closest(`#${BOX_ID}, #${MARKS_ID}, #${STATUS_ID}`);
  const setCursorHint = (kind) => {
    const root = document.documentElement;
    if (root.dataset.tmCursor !== kind) root.dataset.tmCursor = kind;
  };

  const previewAt = (ev) => {
    const region = regionZoneAt(ev.clientX, ev.clientY);
    setCursorHint(region ? "block" : "text");
    if (region) return previewRegion(region);
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return clearHover();
    const word = wordAround(caret.run, caret.at);
    if (word) {
      previewUnderline(rectsOf(rangeFor(word.s, word.e)), false);
      return;
    }
    // Between words: a click makes a point, anchored to the word on the left.
    const gap = gapAt(caret);
    if (!gap) return clearHover();
    previewPoint(gap.box);
  };

  // The insertion point the pointer sits at: where a chevron would go, and the
  // source offset a point anchor would use (the end of the word to its left).
  const gapAt = (caret) => {
    const text = caret.run.text;
    let at = Math.min(caret.at, text.length);
    while (at > 0 && /\s/.test(text[at - 1])) at -= 1;
    if (at === 0) return null;
    const range = document.createRange();
    range.setStart(caret.run.node, at);
    range.setEnd(caret.run.node, at);
    return { s: caret.run.s + at, box: range.getBoundingClientRect() };
  };

  const onMouseDown = (ev) => {
    if (!ANNOTATE || ev.button !== 0 || onOverlay(ev)) return;
    if (openUuid !== null || composeActive) return; // this click only dismisses
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    drag = { from: { x: ev.clientX, y: ev.clientY }, start: caret, moved: false };
  };
  const onMouseMove = (ev) => {
    if (!ANNOTATE) return;
    if (!drag) {
      if (openUuid !== null || composeActive || onOverlay(ev)) return clearHover();
      previewAt(ev);
      return;
    }
    if (!drag.moved && Math.hypot(ev.clientX - drag.from.x, ev.clientY - drag.from.y) < 4) {
      return;
    }
    drag.moved = true;
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    const from = wordAround(drag.start.run, drag.start.at) || {
      s: drag.start.run.s + drag.start.at,
      e: drag.start.run.s + drag.start.at,
    };
    const to = wordAround(caret.run, caret.at) || {
      s: caret.run.s + caret.at,
      e: caret.run.s + caret.at,
    };
    drag.range = { s: Math.min(from.s, to.s), e: Math.max(from.e, to.e) };
    previewUnderline(rectsOf(rangeFor(drag.range.s, drag.range.e)), true);
  };
  const onMouseUp = (ev) => {
    if (!ANNOTATE || !drag) return;
    const d = drag;
    drag = null;
    clearHover();
    if (!d.moved || !d.range || d.range.e <= d.range.s) return;
    const word = wordAround(d.start.run, d.start.at);
    // A drag that never left the word it started on is a plain click.
    if (word && d.range.s === word.s && d.range.e === word.e) return;
    ev.preventDefault();
    ev.stopImmediatePropagation();
    swallowClick = true;
    compose("span", { s: d.range.s, e: d.range.e }, {
      scope: "span",
      start: d.range.s,
      end: d.range.e,
    });
  };

  const onClick = (ev) => {
    if (swallowClick) {
      swallowClick = false;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      return;
    }
    if (!ANNOTATE || onOverlay(ev)) return;
    // A click while something is open only dismisses it; making a new
    // annotation takes a second click, from a clean slate.
    if (openUuid !== null || composeActive) {
      if (closeBox()) {
        ev.preventDefault();
        ev.stopImmediatePropagation();
      }
      return;
    }
    clearHover();
    const region = regionZoneAt(ev.clientX, ev.clientY);
    if (region) {
      ev.preventDefault();
      ev.stopImmediatePropagation();
      // The anchor goes at the head of the region; the server moves it past a
      // list marker so the label binds to the text rather than to the bullet.
      compose(region.kind, { s: region.s, scope: region.kind }, {
        scope: region.kind,
        start: region.s,
      });
      return;
    }
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    ev.preventDefault();
    ev.stopImmediatePropagation();
    const word = wordAround(caret.run, caret.at);
    if (word) {
      compose("comment", { s: word.e, scope: "word" }, { scope: "word", start: word.e });
      return;
    }
    const gap = gapAt(caret);
    if (gap) compose("point", { s: gap.s, scope: "point" }, { scope: "point", start: gap.s });
  };

  // Up/down arrows walk the annotations in document order — selecting,
  // scrolling to, and opening each — whenever no reply is being typed.
  const onArrow = (e) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const box = document.getElementById(BOX_ID);
    const ta = box && box.querySelector("textarea");
    if (ta && ta.value) return;
    if (!pins.length) return;
    const list = pins.slice().sort((a, b) => a.start - b.start);
    let idx = list.findIndex((p) => p.uuid === openUuid);
    if (e.key === "ArrowDown") idx = idx < 0 ? 0 : Math.min(idx + 1, list.length - 1);
    else idx = idx < 0 ? list.length - 1 : Math.max(idx - 1, 0);
    e.preventDefault();
    e.stopPropagation();
    scrollToPin(list[idx]);
    showAnnot(list[idx]);
  };

  // ---------------------------------------------------------------- wiring
  // A compile error is something you want to paste somewhere — into an issue,
  // into a message, into a search. Selecting it by dragging fights the
  // annotation gestures, so the whole panel copies itself on a click.
  const showStatus = (lines) => {
    const el = document.getElementById(STATUS_ID);
    if (!el) return;
    if (!lines || !lines.length) {
      el.hidden = true;
      el.textContent = "";
      return;
    }
    const text = lines.join("\n");
    el.hidden = false;
    el.textContent = text;
    el.title = "Click to copy";
    el.onclick = (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      const note = (what) => {
        for (const old of el.querySelectorAll(".tm-status-copied")) old.remove();
        const tag = document.createElement("div");
        tag.className = "tm-status-copied";
        tag.textContent = what;
        el.appendChild(tag);
        setTimeout(() => tag.remove(), 1200);
      };
      // The clipboard API needs a secure context and the page's permission;
      // where it is refused, a selection in a throwaway field still copies.
      const legacy = () => {
        const area = document.createElement("textarea");
        area.value = text;
        area.style.cssText = "position:fixed;opacity:0";
        document.body.appendChild(area);
        area.select();
        let ok = false;
        try {
          ok = document.execCommand("copy");
        } catch (err) {}
        area.remove();
        note(ok ? "copied" : "press ⌘C to copy");
        if (!ok) {
          // Left selected, so the keystroke has something to act on.
          const range = document.createRange();
          range.selectNodeContents(el);
          const sel = getSelection();
          sel.removeAllRanges();
          sel.addRange(range);
        }
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(() => note("copied"), legacy);
        return;
      }
      legacy();
    };
  };

  // What the last compile said, so a failed fetch does not paint over the
  // reason for it: "no document" is the consequence, the compile error is the
  // story.
  let compileErrors = null;

  const loadDocument = () =>
    getJson("/dev/html/doc").then((res) => {
      if (!res || !res.ok) {
        if (!compileErrors) {
          showStatus([res && res.error ? res.error : "cannot load the document"]);
        }
        return;
      }
      const doc = document.getElementById(DOC_ID);
      if (!doc) return;
      // Only replace the document when it actually changed: rewriting it drops
      // the selection, and every compile would otherwise flicker the page.
      if (doc.dataset.sig !== res.body) {
        doc.dataset.sig = res.body;
        doc.innerHTML = res.body;
        indexDocument();
      }
      if (res.title) document.title = res.title;
    });

  const loadPins = () =>
    getJson("/dev/html/pins").then((res) => {
      if (res && res.ok) pins = res.pins || [];
    });

  const refresh = () => Promise.all([loadDocument(), loadPins()]).then(() => {
    render();
    refreshOpen();
  });

  let assetVersion = null;
  const listen = () => {
    const sse = new EventSource("/dev/diagnostics");
    sse.onmessage = (ev) => {
      let data = null;
      try {
        data = JSON.parse(ev.data);
      } catch (err) {
        return;
      }
      if (assetVersion === null) assetVersion = data.assetVersion;
      else if (data.assetVersion !== assetVersion) return void location.reload();
      compileErrors = data.ok ? null : data.messages;
      showStatus(compileErrors);
      refresh();
    };
    sse.onerror = () => {
      // The server went away; the page is a snapshot from here on.
      // Nothing here is a preview of a print job; it is a document being
      // served, and in this mode annotated.
      showStatus([
        ANNOTATE
          ? "Disconnected from the annotation server"
          : "Disconnected from the document server",
      ]);
    };
  };

  if (ANNOTATE) {
    document.documentElement.classList.add("tm-annotate");
    window.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("mousemove", onMouseMove, true);
    window.addEventListener("mouseup", onMouseUp, true);
    document.addEventListener("click", onClick, true);
    document.addEventListener("keydown", onArrow, true);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && drag) {
        drag = null;
        clearHover();
      }
    });
  }
  document.addEventListener("scroll", render, { capture: true, passive: true });
  window.addEventListener("resize", render);
  refresh().then(listen);
  // Fonts and images settle after the first paint and move everything below
  // them; a slow tick keeps the marks on their text without watching for it.
  setInterval(render, 1000);
})();
