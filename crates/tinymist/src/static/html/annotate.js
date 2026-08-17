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
  const DOC_ID = "tinymist-doc";
  const MARKS_ID = "tinymist-marks";
  const BOX_ID = "tinymist-annot-box";
  const STATUS_ID = "tinymist-status";
  const TOGGLE_ID = "tinymist-annotate-toggle";
  // Which face this page wears rides in the URL's first segment: `/a/` is the
  // annotator, `/v/` a document served to be read, `/p/` an editor's preview.
  const ANNOTATE = /^\/a(\/|$)/.test(location.pathname);

  // ---------------------------------------------------------------- palette
  // An annotation is coloured by its letter, not by its state: A is always the
  // same colour, wherever it is and whatever has happened to it, so the mark
  // in the text and the chip in the lane and the window at the corner are
  // obviously one thing.
  //
  // A rainbow at one lightness and one modest chroma — oklch(0.80 0.105 h),
  // ten hues around the wheel — so no letter shouts louder than another and
  // none of them competes with the document's own ink. The order steps around
  // the wheel rather than along it, so consecutive letters look different;
  // after the tenth it starts again.
  const PALETTE = [
    "#faa39c", // 25°
    "#5ed1de", // 205°
    "#d1be6c", // 97°
    "#acb8ff", // 277°
    "#73d4b2", // 169°
    "#f1a2c8", // 349°
    "#efae76", // 61°
    "#7dc7fb", // 241°
    "#a3cd86", // 133°
    "#d5aaee", // 313°
  ];
  // What a state says is only how loudly: resolved is done with, and steps
  // back without changing what it is.
  const RESOLVED_OPACITY = 0.45;
  const letterColor = (letter) =>
    PALETTE[Math.max(0, letterIndex(letter) - 1) % PALETTE.length];
  const pinOpacity = (pin) => (pin.resolved ? RESOLVED_OPACITY : 1);
  // The same colour taken down to something that can sit behind or inside it:
  // the halo around a mark, and the letter written on a chip. Black would do
  // the job and look like a different design; this keeps one hue per
  // annotation, all the way through.
  const darkTint = (color) => `color-mix(in oklab, ${color} 32%, #0b0b0e)`;
  // A mark's halo, on both sides of its stroke, so it holds up over text as
  // readily as over a filled callout.
  const haloRing = (color) =>
    `0 0 0 1.5px ${darkTint(color)}, inset 0 0 0 1.5px ${darkTint(color)}`;
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
  // The colour a pin is drawn in: its letter's, lifted while its window is
  // open so the one being read stands out from the rest.
  // The colour an annotation was made in, which the sidecar keeps: the palette
  // can change, and a letter can be reassigned, without an annotation changing
  // colour under someone who has been looking at it. Only one without a stored
  // colour falls back to the palette.
  const ownColor = (pin) => pin.color || letterColor(pin.letter);
  const pinColor = (pin) =>
    pin.uuid === openUuid ? lighten(ownColor(pin), 0.28) : ownColor(pin);
  // What a new annotation here would be called, and therefore what colour it
  // would be: a preview is the annotation you are about to make.
  const nextColor = () => letterColor(nextLetter());

  // A mark that has not been made yet is drawn in the same colour as one that
  // has — a dim green reads as a faded annotation rather than as a proposal —
  // and says it is provisional by being dotted, and by travelling: the dots
  // crawl along the mark, slowly and forever, the way a marquee does.
  // Dots on a solid ground of the same colour taken down, rather than dots on
  // nothing: the gaps then read as part of the mark instead of as holes onto
  // whatever is behind it.
  const dotted = (color, vertical) =>
    `repeating-linear-gradient(${vertical ? "to bottom" : "to right"}, ` +
    `${color} 0 2px, ${darkTint(color)} 2px 5px)`;
  const crawlLine = (el, color, vertical) => {
    el.style.background = dotted(color, vertical);
    // Backed by a dark tint of its own colour, exactly as a finished mark is:
    // the dots have to hold up over text either way.
    el.style.boxShadow = `0 0 0 1.5px ${darkTint(color)}`;
    el.classList.add(vertical ? "tm-crawl-v" : "tm-crawl-h");
  };
  // Around a box, the four edges are four gradients, so they can travel
  // clockwise together: a dotted border cannot be animated.
  const crawlBox = (el, color) => {
    el.style.backgroundImage = [
      dotted(color, false),
      dotted(color, true),
      dotted(color, false),
      dotted(color, true),
    ].join(", ");
    el.classList.add("tm-crawl-box");
  };
  const pageIsDark = () =>
    matchMedia && matchMedia("(prefers-color-scheme: dark)").matches;
  const haloColor = () => (pageIsDark() ? "#0e0e0e" : "#ffffff");

  // ------------------------------------------------------------------- http
  // Every endpoint sits under the page that uses it: one server can serve a
  // whole directory, and `/a/paper/dev/html/doc` is that document's, while
  // `/a/dev/html/doc` would be nobody's. The page's own URL ends in a slash, so
  // it is the base to resolve them against.
  const BASE = location.pathname.replace(/[^/]*$/, "");
  const url = (path) => BASE + path.replace(/^\/?(dev\/)?/, "dev/");
  // Whether the server is answering. Everything that needs the server goes
  // through here, so one failed request is enough to know, and one that
  // succeeds is enough to know again.
  let online = true;
  let flying = false; // airplane mode: the page pretends the server is gone
  const listeners = [];
  const onConnection = (fn) => listeners.push(fn);
  const setOnline = (state) => {
    if (online === state) return;
    online = state;
    for (const fn of listeners) fn(online);
  };
  const offline = () => ({ ok: false, error: "no connection to the server" });

  const post = (path, payload) => {
    if (flying) return Promise.resolve(offline());
    return fetch(url(path), { method: "POST", body: JSON.stringify(payload) })
      .then((r) => r.json())
      .then((r) => {
        setOnline(true);
        if (!r.ok) console.warn("tinymist annotation:", r.error);
        return r;
      })
      .catch((e) => {
        console.warn("tinymist annotation:", e);
        setOnline(false);
        return { ok: false, error: String(e) };
      });
  };
  const getJson = (path) => {
    if (flying) return Promise.resolve(offline());
    return fetch(url(path))
      .then((r) => r.json())
      .then((r) => {
        setOnline(true);
        return r;
      })
      .catch((e) => {
        setOnline(false);
        return { ok: false, error: String(e) };
      });
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

  // ------------------------------------------------------------- the index
  // Every part of the rendering that can be referred to carries `data-uid`, and
  // the map that came with the document says what each id is. A location names
  // an id; drawing one and making one are both lookups by id. The client never
  // sees a position in the `.typ` file.
  let pins = [];
  let renderId = null; // which rendering the page is showing
  let nodeKinds = {}; // uid -> kind, from the map
  let runs = []; // { el, uid, node, text }
  let atoms = []; // { el, uid, kind }
  let blocks = []; // { el, uid, kind }
  const byUid = new Map();

  const BLOCK_KINDS = ["block", "para", "item", "math_block", "math.block", "svg", "image"];
  const ATOM_KINDS = ["math", "link", "raw", "inline"];
  const BLOCK_SELECTOR =
    "p,li,dt,dd,figure,table,blockquote,pre,div,section,article,aside,h1,h2,h3,h4,h5,h6";

  // What a region is called: the map says, and a click on one asks for an
  // annotation of that kind.
  const blockKind = (el, kind) => (kind === "math_block" ? "math.block" : kind);

  // A drawing carries its id in `id` rather than in `data-uid`: an SVG can only
  // be given the attribute Typst already writes for it.
  const uidOf = (el) => {
    if (!el || !el.getAttribute) return null;
    return el.getAttribute("data-uid") || (el.localName === "svg" ? el.getAttribute("id") : null);
  };

  const indexDocument = () => {
    forget();
    runs = [];
    atoms = [];
    blocks = [];
    byUid.clear();
    const doc = document.getElementById(DOC_ID);
    if (!doc) return;
    for (const el of doc.querySelectorAll("[data-uid]")) {
      const uid = uidOf(el);
      const kind = nodeKinds[uid];
      if (!uid || !kind) continue;
      const only = el.childNodes.length === 1 ? el.firstChild : null;
      const node = only && only.nodeType === Node.TEXT_NODE ? only : null;
      const entry = { el, uid, kind, node, text: node ? node.nodeValue || "" : "" };
      byUid.set(uid, entry);
      if (kind === "text" && node) runs.push(entry);
      else if (ATOM_KINDS.includes(kind)) atoms.push(entry);
      else if (BLOCK_KINDS.includes(kind)) blocks.push(entry);
    }
    // A block whose whole content is another block adds nothing to annotate:
    // the exporter wraps headings and figures in spacing divs, and offering
    // both means two frames around the same thing, one of them the width of
    // the column.
    blocks = blocks.filter((block) => {
      const kids = [...block.el.children];
      if (kids.length !== 1 || block.el.childNodes.length !== 1) return true;
      const inner = byUid.get(uidOf(kids[0]));
      return !(inner && BLOCK_KINDS.includes(inner.kind));
    });
    // Whether another region contains it, which decides how far into the margin
    // its mark may reach: a paragraph on its own has the margin to itself, one
    // inside a callout does not. Judged against the regions that are actually
    // offered, so that a wrapper the exporter left behind does not count.
    for (const block of blocks) {
      block.enclosed = blocks.some(
        (other) => other !== block && other.el.contains(block.el),
      );
    }
    // A drawing carries its id on the `<svg>` itself, which Typst writes as an
    // `id` rather than as an attribute of ours.
    for (const el of doc.querySelectorAll("svg[id]")) {
      const uid = el.getAttribute("id");
      if (!uid || byUid.has(uid) || nodeKinds[uid] !== "svg") continue;
      const entry = { el, uid, kind: "svg", node: null, text: "" };
      byUid.set(uid, entry);
      blocks.push(entry);
    }
  };

  // ------------------------------------------------------- ranges and boxes

  const domRange = (a, b) => {
    if (!a || !b) return null;
    const range = document.createRange();
    try {
      range.setStart(a.node, Math.min(a.at, (a.node.nodeValue || "").length));
      range.setEnd(b.node, Math.min(b.at, (b.node.nodeValue || "").length));
    } catch (err) {
      return null;
    }
    return range;
  };

  // The boxes a range covers, as lines rather than as fragments. A range that
  // crosses markup — an equation, a bold word — comes back in pieces with
  // slivers between them, and an underline drawn piece by piece reads as a
  // dashed line. One box per line says what the annotation covers.
  const mergeLines = (rects) => {
    const lines = [];
    const boxes = Array.from(rects).sort((a, b) => a.top - b.top || a.left - b.left);
    for (const box of boxes) {
      if (box.width < 1 || box.height < 1) continue;
      const line = lines.find((l) => box.top < l.bottom - 2 && box.bottom > l.top + 2);
      if (line) {
        line.left = Math.min(line.left, box.left);
        line.right = Math.max(line.right, box.right);
        line.top = Math.min(line.top, box.top);
        line.bottom = Math.max(line.bottom, box.bottom);
      } else {
        lines.push({ left: box.left, right: box.right, top: box.top, bottom: box.bottom });
      }
    }
    for (const line of lines) {
      line.width = line.right - line.left;
      line.height = line.bottom - line.top;
    }
    return lines;
  };
  const rectsOf = (range) => (range ? mergeLines(Array.from(range.getClientRects())) : []);

  // ------------------------------------------------------------- locations
  // The two conversions between what the page can see and what an annotation
  // says: a location becomes boxes to draw, and a click becomes a location to
  // send.

  const runOf = (uid) => {
    const entry = byUid.get(uid);
    return entry && entry.node ? entry : null;
  };

  const charRects = (uid, from, to) => {
    const run = runOf(uid);
    if (!run) return [];
    return rectsOf(domRange({ node: run.node, at: from }, { node: run.node, at: to }));
  };

  // Where a caret sits: the gap between two characters, as a box.
  const caretBox = (uid, pos) => {
    const run = runOf(uid);
    if (!run) return null;
    const text = run.text;
    const at = Math.max(0, Math.min(pos, text.length));
    // A zero-width range has no box in some browsers, so a character beside it
    // is measured and its edge taken.
    const before = at > 0 ? domRange({ node: run.node, at: at - 1 }, { node: run.node, at }) : null;
    const after =
      at < text.length ? domRange({ node: run.node, at }, { node: run.node, at: at + 1 }) : null;
    const box = (range) => {
      const rects = range ? Array.from(range.getClientRects()) : [];
      return rects.length ? rects[rects.length - 1] : null;
    };
    const edge = (rect, at) => ({
      left: at,
      right: at + 1,
      top: rect.top,
      bottom: rect.bottom,
      width: 1,
      height: rect.height,
    });
    const left = box(before);
    const right = box(after);
    if (left) return edge(left, left.right);
    if (right) return edge(right, right.left);
    return null;
  };

  // A stretch from one position to another, which may cross runs.
  const spanRects = (from, to) => {
    const a = runOf(from.ref);
    const b = runOf(to.ref);
    if (!a || !b) return [];
    const range = domRange({ node: a.node, at: from.pos }, { node: b.node, at: to.pos });
    if (!range) return [];
    if (range.collapsed) return [];
    return rectsOf(range);
  };

  // The sentence containing a position: from the end of the previous sentence
  // to the end of this one. A sentence ends at `.`, `!` or `?` followed by a
  // space, which is wrong for "Dr. Who" and right the rest of the time.
  const SENTENCE_END = /[.!?]["'\u201d\u2019]?(\s|$)/g;
  const sentenceAround = (text, at) => {
    let beg = 0;
    let end = text.length;
    SENTENCE_END.lastIndex = 0;
    let match;
    while ((match = SENTENCE_END.exec(text))) {
      const stop = match.index + match[0].length;
      if (stop <= at) {
        beg = stop;
        continue;
      }
      end = stop;
      break;
    }
    while (beg < text.length && /\s/.test(text[beg])) beg += 1;
    return end > beg ? { beg, end: Math.min(end, text.length) } : null;
  };

  // The sentence around a position, across the runs it is spread over.
  //
  // An anchor written in the middle of a sentence ends the run it is in, so the
  // sentence carries on in the runs after it. The block the run belongs to is
  // the whole of what a sentence can cover.
  const sentenceRects = (run, at) => {
    const host = blockAround(run.el);
    const parts = host ? runs.filter((other) => host.contains(other.el)) : [run];
    let text = "";
    const spans = [];
    for (const other of parts) {
      spans.push({ run: other, at: text.length });
      text += other.text;
    }
    const base = spans.find((span) => span.run === run);
    if (!base) return [];
    const range = sentenceAround(text, base.at + at);
    if (!range) return [];
    const boxes = [];
    for (const span of spans) {
      const beg = Math.max(range.beg - span.at, 0);
      const end = Math.min(range.end - span.at, span.run.text.length);
      if (end > beg) boxes.push(...charRects(span.run.uid, beg, end));
    }
    return boxes;
  };

  // The block an element is in, if any.
  const blockAround = (el) => {
    let node = el;
    while (node && node.id !== DOC_ID) {
      const entry = byUid.get(uidOf(node));
      if (entry && BLOCK_KINDS.includes(entry.kind)) return node;
      node = node.parentElement;
    }
    return null;
  };

  // The line a position is on, as the browser laid it out: the characters
  // whose boxes share its top edge.
  const lineAround = (run, at) => {
    const text = run.text;
    const box = (index) => {
      const range = domRange({ node: run.node, at: index }, { node: run.node, at: index + 1 });
      const rects = range ? Array.from(range.getClientRects()) : [];
      return rects.length ? rects[0] : null;
    };
    const here = box(Math.max(0, Math.min(at, text.length - 1)));
    if (!here) return null;
    const sameLine = (index) => {
      const other = box(index);
      return other && Math.abs(other.top - here.top) < 2;
    };
    let beg = Math.max(0, Math.min(at, text.length - 1));
    let end = beg;
    while (beg > 0 && sameLine(beg - 1)) beg -= 1;
    while (end < text.length - 1 && sameLine(end)) end += 1;
    return { beg, end: end + 1 };
  };

  const elementOf = (ref) => {
    const entry = byUid.get(ref && ref.ref);
    return entry ? entry.el : null;
  };

  // ---------------------------------------------------------- hit testing

  // Where the pointer is, in the document's terms. Browsers disagree on the
  // name of this and on nothing else about it.
  // What is under the pointer in the document, looking past the marks drawn
  // over it: an annotation's own hit area covers the text it is on, and the
  // ground inside a framed region belongs to what is in it.
  const documentAt = (x, y) => {
    const doc = document.getElementById(DOC_ID);
    if (!doc) return null;
    const stack = document.elementsFromPoint
      ? document.elementsFromPoint(x, y)
      : [document.elementFromPoint(x, y)];
    return stack.find((el) => el && doc.contains(el)) || null;
  };

  const caretAt = (x, y) => {
    // `caretRangeFromPoint` answers with the nearest position, which in the
    // margin is the nearest word, so a preview would appear for text the
    // pointer is nowhere near. What is under the pointer settles it.
    const under = documentAt(x, y);
    const doc = document.getElementById(DOC_ID);
    if (!under || !doc || under === doc || !doc.contains(under)) return null;
    if (atomElementAt(x, y)) return null;
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
    // A position at the end of a wrapped line is also the position at the start
    // of the next one, and that is the one reported. The pointer is on the line
    // it is on, so a position on another line steps back to the end of this
    // one; the word it names is then the last word of this line and not the
    // first word of the next.
    const lineAt = (at) => {
      const range = document.createRange();
      range.setStart(node, at);
      range.setEnd(node, at);
      return range.getBoundingClientRect();
    };
    // A collapsed range at the very start or end of a node can measure as
    // nothing at all, which says nothing about which line it is on.
    const onLine = (box) => box.height === 0 || (y >= box.top - 3 && y <= box.bottom + 3);
    if (!onLine(lineAt(offset))) {
      if (offset > 0 && onLine(lineAt(offset - 1))) offset -= 1;
      else return null;
    }
    // Nearest is still not under: a point in a callout's padding resolves to a
    // character somewhere else. The character's own box has to be near the
    // pointer, with slack sideways so the gaps between words still count.
    const probe = document.createRange();
    probe.setStart(node, Math.max(0, offset - 1));
    probe.setEnd(node, Math.min((node.nodeValue || "").length, offset + 1));
    const near = Array.from(probe.getClientRects()).some(
      (r) => x >= r.left - 10 && x <= r.right + 10 && y >= r.top - 3 && y <= r.bottom + 3,
    );
    if (!near) return null;
    const run = runs.find((r) => r.node === node);
    return run ? { run, at: offset } : null;
  };

  // The word around a character position.
  const wordAround = (run, at) => {
    const text = run.text;
    if (!text.trim()) return null;
    let start = Math.min(at, text.length - 1);
    if (/\s/.test(text[start])) return null;
    let end = start;
    while (start > 0 && !/\s/.test(text[start - 1])) start -= 1;
    while (end < text.length && !/\s/.test(text[end])) end += 1;
    return { run, start, end, text: text.slice(start, end) };
  };

  // The element under the pointer that is annotated whole, if there is one.
  const atomElementAt = (x, y) => {
    let el = documentAt(x, y);
    while (el) {
      const entry = byUid.get(uidOf(el));
      if (entry && ATOM_KINDS.includes(entry.kind)) return entry;
      if (entry && (entry.kind === "svg" || entry.kind === "math.block")) return entry;
      el = el.parentElement;
    }
    return null;
  };

  const atomAt = (x, y) => {
    const entry = atomElementAt(x, y);
    if (!entry) return null;
    // One that already carries an annotation is not offered again: the ground
    // around it belongs to the mark already there.
    const taken = takenBlocks.get(entry.el);
    return taken && taken.size ? null : entry;
  };

  // The gap between two blocks, which is a place in its own right: something
  // belongs here, this paragraph should follow that one. Offered only when the
  // pointer is in the space between them and level with the column, so that
  // the margins stay empty.
  const GAP_REACH = 60;
  // How far a position may be from the pointer and still be the one meant.
  const SNAP_REACH = 400;
  // How far a gap between blocks may be from the pointer and still be the one
  // meant. There is no such limit on a place in a line: inside a run of text
  // every position is one of the spaces in it, and the nearest is the one.
  const SNAP_NEAR = 15;
  // Where the caret for a position at one of a block's edges is drawn: a stub
  // of fixed length lying half a margin beyond the edge and starting a little
  // to the left of the block. It pokes out into the margin the way the caret
  // between two words pokes out above and below the line, so that it is read
  // as a position rather than as a rule under whatever is above it.
  //
  // Half the block's own margin, rather than half the distance to the next
  // block: two blocks with equal margins put their carets in the same place,
  // so the one gap between them shows one caret however it is referred to.
  const EDGE_OUT = 20;
  const edgeCaretBox = (box, side, el) => {
    // The element's own edge rather than the edge of its ink: two blocks with
    // equal margins are then the same distance from the caret between them,
    // whichever of the two it is attached to.
    const own = blockRect(el);
    const edge = side === "bottom" ? own.bottom : own.top;
    const out = edgeMargin(el, side) / 2;
    const middle = side === "bottom" ? edge + out : edge - out;
    // As wide as the two blocks it lies between together, and a little wider
    // than that at each end so that it sticks out into the margin.
    const other = neighbourBlock(el, side);
    const left = Math.min(box.left, other ? other.left : box.left) - EDGE_OUT;
    const right = Math.max(box.right, other ? other.right : box.right) + EDGE_OUT;
    return {
      left,
      right,
      top: middle - EDGE_H / 2,
      bottom: middle + EDGE_H / 2,
      width: right - left,
      height: EDGE_H,
    };
  };

  // The block on the other side of one of a block's edges, if there is one.
  // Blocks are neighbours by how far apart they are vertically and nothing
  // else: a narrow equation and a short heading do not overlap horizontally and
  // are still one after the other.
  const neighbourBlock = (el, side) => {
    const own = blockRect(el);
    let best = null;
    for (const block of blocks) {
      if (block.enclosed || block.el === el) continue;
      const rect = blockRect(block.el);
      const away = side === "bottom" ? rect.top - own.bottom : own.top - rect.bottom;
      if (away < 0) continue;
      if (!best || away < best.away) best = { el: block.el, away };
    }
    return best && blockBox(best.el);
  };

  // The margin an element keeps on one side. The exporter puts a heading's
  // spacing on the wrapper it builds around it rather than on the heading, so
  // the margin is read from the outermost element that holds nothing else.
  const EDGE_LEAST = 8;
  const edgeMargin = (el, side) => measured(el).margin[side];
  const marginOf = (el, side) => {
    let outer = el;
    while (
      outer.parentElement &&
      outer.parentElement.childNodes.length === 1 &&
      outer.parentElement.id !== DOC_ID
    ) {
      outer = outer.parentElement;
    }
    const style = getComputedStyle(outer);
    const margin = parseFloat(side === "bottom" ? style.marginBottom : style.marginTop);
    return Math.max(margin || 0, EDGE_LEAST);
  };

  const verticalGapAt = (x, y, reach = GAP_REACH) => {
    // Anywhere across the column counts, so that the gap under a heading is a
    // gap for its whole width. Outside the column is margin, and empty.
    const doc = document.getElementById(DOC_ID);
    const column = doc && doc.getBoundingClientRect();
    if (!column || x < column.left || x > column.right) return null;
    let above = null;
    let below = null;
    for (const block of blocks) {
      // A block inside another does not make a gap with its neighbours: the
      // space below the last paragraph of a callout is inside the callout.
      if (block.enclosed) continue;
      const box = blockBox(block.el);
      if (box.bottom <= y && (!above || box.bottom > above.box.bottom)) {
        above = { block, box };
      }
      if (box.top >= y && (!below || box.top < below.box.top)) {
        below = { block, box };
      }
    }
    if (!above && !below) return null;
    const gapAbove = above ? y - above.box.bottom : Infinity;
    const gapBelow = below ? below.box.top - y : Infinity;
    if (Math.min(gapAbove, gapBelow) > reach) return null;
    // A gap belongs to the block above it, which makes one gap one place. The
    // first block of the document is the exception: the space above it belongs
    // to nothing else, and is named as that block's top.
    const first = gapAbove <= reach ? { at: above, side: "bottom" } : null;
    const then = gapBelow <= reach ? { at: below, side: "top" } : null;
    for (const choice of [first, then]) {
      if (!choice) continue;
      const taken = takenBlocks.get(choice.at.block.el);
      // An edge that already carries a position is not offered again, but the
      // other side of the same gap still is: it is a place of its own.
      if (taken && taken.has("pos.v")) continue;
      return {
        block: choice.at.block,
        side: choice.side,
        box: edgeCaretBox(choice.at.box, choice.side, choice.at.block.el),
      };
    }
    return null;
  };

  // How much text is kept either side of a position, to recognise the place
  // again if the document has moved on by the time it is submitted.
  const CONTEXT = 8;

  // ---------------------------------------------------- building locations

  const wordLocation = (kind, word) => ({
    type: kind,
    ref: {
      type: "word",
      ref: word.run.uid,
      beg: word.start,
      end: word.end,
      w: word.text,
    },
  });

  const cursorRef = (run, pos) => ({
    type: "text_cursor",
    ref: run.uid,
    pos,
    l: run.text.slice(Math.max(0, pos - CONTEXT), pos),
    r: run.text.slice(pos, pos + CONTEXT),
  });

  const nodeLocation = (kind, entry) => ({
    type: kind,
    ref: { type: "node", ref: entry.uid },
  });
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
  // How a region is marked. A rectangle says where the region ends as well as
  // where it begins, which a strip beside it cannot; the strip is the older,
  // quieter shape and is kept here in case the rectangle proves too loud.
  // Items are not affected either way: they wear a chip beside their marker.
  const REGION_SHAPE = "box"; // "box" | "strip"
  // How far a region's rectangle stands off the text. Less than the strips
  // keep, because two rectangles on consecutive paragraphs have only the
  // paragraph spacing to share between them.
  const BOX_INSET = 5;
  // How an item is marked: a ring around its own marker — a circle around a
  // bullet, a rectangle around an enumerator — or the pointer chip beside it,
  // which is what it was before and is kept in case the ring reads worse.
  const ITEM_SHAPE = "ring"; // "ring" | "chip"
  // How much further out each enclosing region's strip stands. Enough to tell
  // them apart and to point at either, not enough to march off the page: a
  // definition box holding paragraphs is one step out, no more.
  const STRIP_STEP = 8;
  const STRIP_DEPTH_MAX = 3;

  // What an element actually covers with ink, as against the box it was given:
  // a centred equation is a line of glyphs in the middle of a full-width block.
  const inkOf = (el) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    const boxes = mergeLines(Array.from(range.getClientRects()));
    return boxes.length ? boxes : [el.getBoundingClientRect()];
  };

  // What a region annotation is drawn around: the ink of the element, not its
  // border box. A block is as wide as the column whatever is in it, so a
  // figure holding a centred drawing would otherwise be marked — and offered —
  // with a rectangle reaching far past the picture on both sides.
  const blockBox = (el) => viewport(measured(el).ink);

  /// The element's own box, margins excluded, which is what the space between
  /// two blocks is measured from.
  const blockRect = (el) => viewport(measured(el).rect);

  // Measuring an element means laying the page out, and the hover path asks
  // about every block on the page. The answers are kept until something moves
  // them, in page coordinates so that scrolling is not something that moves
  // them.
  const measures = new Map();
  const measured = (el) => {
    let known = measures.get(el);
    if (!known) {
      const boxes = inkOf(el);
      known = {
        ink: page({
          top: Math.min(...boxes.map((b) => b.top)),
          bottom: Math.max(...boxes.map((b) => b.bottom)),
          left: Math.min(...boxes.map((b) => b.left)),
          right: Math.max(...boxes.map((b) => b.right)),
        }),
        rect: page(el.getBoundingClientRect()),
        margin: {
          top: marginOf(el, "top"),
          bottom: marginOf(el, "bottom"),
        },
      };
      measures.set(el, known);
    }
    return known;
  };
  const forget = () => measures.clear();
  const page = (b) => ({
    top: b.top + window.scrollY,
    bottom: b.bottom + window.scrollY,
    left: b.left + window.scrollX,
    right: b.right + window.scrollX,
  });
  const viewport = (b) => {
    const top = b.top - window.scrollY;
    const bottom = b.bottom - window.scrollY;
    const left = b.left - window.scrollX;
    const right = b.right - window.scrollX;
    return { top, bottom, left, right, width: right - left, height: bottom - top };
  };

  // What to draw for a location, and where.
  //
  // The shape a mark takes is a property of the location's kind: text is
  // underlined, a position is a caret, a region is framed. The boxes come from
  // the elements the location names.
  const geometryOf = (pin) => {
    const loc = pin && pin.location;
    if (!loc) return null;
    const kind = loc.type;
    switch (kind) {
      case "word": {
        const ref = loc.ref;
        const boxes = charRects(ref.ref, ref.beg, ref.end);
        return boxes.length ? { scope: kind, boxes } : null;
      }
      case "line":
      case "sentence": {
        // The sidecar names a word; what is drawn is the sentence or line it
        // is in, which only the rendering knows the extent of.
        const ref = loc.ref;
        const run = runOf(ref.ref);
        if (!run) return null;
        if (kind === "sentence") {
          const boxes = sentenceRects(run, ref.end);
          return boxes.length ? { scope: kind, boxes } : null;
        }
        const range = lineAround(run, ref.end);
        if (!range) return null;
        const boxes = charRects(ref.ref, range.beg, range.end);
        return boxes.length ? { scope: kind, boxes } : null;
      }
      case "span.h": {
        const boxes = spanRects(loc.begin, loc.end);
        return boxes.length ? { scope: kind, boxes } : null;
      }
      case "pos.h": {
        const box = caretBox(loc.ref.ref, loc.ref.pos);
        return box ? { scope: "point", boxes: [box], caret: box } : null;
      }
      case "pos.v":
      case "span.v": {
        const el = elementOf(kind === "pos.v" ? loc.ref : loc.begin);
        if (!el) return null;
        // The ink rather than the border box, as a region mark uses: a block is
        // as wide as the column whatever is in it.
        const box = blockBox(el);
        const side = kind === "pos.v" ? loc.ref.side : loc.begin.side;
        const edge = edgeCaretBox(box, side === "top" ? "top" : "bottom", el);
        return { scope: "edge", boxes: [edge], caret: edge, el };
      }
      case "math":
      case "link":
      case "raw":
      case "opaque": {
        const el = elementOf(loc.ref);
        if (!el) return null;
        return { scope: kind, boxes: mergeLines(Array.from(el.getClientRects())), el };
      }
      case "svg": {
        // A drawing is a picture, marked as one: a frame around all of it.
        const el = elementOf(loc.ref);
        if (!el) return null;
        const enclosed = !!(el.parentElement && el.parentElement.closest(BLOCK_SELECTOR));
        return {
          scope: kind,
          boxes: [el.getBoundingClientRect()],
          block: { el, depth: 0 },
          enclosed,
        };
      }
      case "math.block": {
        // The element spans the whole column while the equation is centred in
        // it, so the mark hangs off the equation's own ink.
        const el = elementOf(loc.ref);
        if (!el) return null;
        const enclosed = !!(el.parentElement && el.parentElement.closest(BLOCK_SELECTOR));
        return { scope: kind, boxes: inkOf(el), block: { el, depth: 0 }, enclosed };
      }
      default: {
        // A region: the ink of the element, since its border box is the width
        // of the column whatever is in it.
        const el = elementOf(loc.ref);
        if (!el) return null;
        const entry = byUid.get(loc.ref.ref);
        const scope = entry ? blockKind(el, kind) : kind;
        return { scope, boxes: inkOf(el), block: { el, depth: 0 } };
      }
    }
  };

  // Where an item's bullet is, and the line it is on. A list marker is drawn
  // outside the item's own box, in the padding its list reserves — the
  // stylesheet fixes that at 1.5em so this is a measurement rather than a
  // guess — and it sits on the first line of the item, whatever the item's
  // height.
  const CHIP_GAP = 7;
  const markerLine = (block, box) => {
    const el = block && block.el;
    // The first line of the item, not the item: a bullet sits at the top of a
    // three-line entry, and the chip points at the bullet. A block element's
    // own client rects are just its border box, so the lines have to be
    // measured through a range over its contents.
    let first = box;
    if (el) {
      const range = document.createRange();
      range.selectNodeContents(el);
      const rects = Array.from(range.getClientRects()).filter((r) => r.height > 0);
      if (rects.length) first = rects[0];
    }
    // The marker — "•", "1.", "a." — is drawn outside the item, in the padding
    // its list reserves for it. That padding is measurable, so the chip clears
    // whatever is in it rather than guessing at a bullet's width.
    const list = el && el.closest("ul, ol");
    // The marker is drawn right-aligned in the padding its list reserves, so
    // the padding's left edge clears the widest of them. A bullet is not the
    // widest — an enumerator like "12." is — so a bulleted list gets some of
    // that room back and the chip sits where the eye expects it.
    const numbered = list && list.localName === "ol";
    const markerLeft =
      (list ? list.getBoundingClientRect().left : box.left - 24) + (numbered ? 0 : 10);
    const size = el ? parseFloat(getComputedStyle(el).fontSize) || 16 : 16;
    const height = Math.max(first.height, 8);
    const mid = first.top + height / 2;
    // Where the marker itself is. A bullet is a dot near the text and gets a
    // circle around it; an enumerator is as wide as its digits and gets a
    // rectangle around the room the list set aside for it.
    // The stylesheet draws the bullet itself: a box 0.5em wide whose right edge
    // is 0.5em left of the text, so its centre is 0.75em out and every browser
    // agrees — far enough that the ring around it clears the text. An enumerator is the browser's own, and fills the padding the
    // list reserves, up to a small gap before the text.
    const listLeft = list ? list.getBoundingClientRect().left : box.left - 24;
    const ringH = size * (numbered ? 1.15 : 0.95);
    const ring = numbered
      ? {
          left: listLeft - 2,
          top: mid - ringH / 2,
          width: box.left - listLeft - 2,
          height: ringH,
          round: 3,
        }
      : {
          left: box.left - size * 0.75 - ringH / 2,
          top: mid - ringH / 2,
          width: ringH,
          height: ringH,
          round: ringH / 2,
        };
    return {
      // Where the chip's apex stops: clear of the whole marker area.
      point: markerLeft,
      top: first.top,
      height,
      mid,
      // The whole mark: the chip, the marker it points at, and the gap up to
      // where the item's own text starts.
      left: markerLeft - CHIP_GAP - 22,
      width: box.left - (markerLeft - CHIP_GAP - 22) - 1,
      ring,
    };
  };

  // A region's strip sits in the margin beside it, one step further out for
  // every region it contains.
  const stripLeftOf = (block, boxes) =>
    Math.min(...boxes.map((b) => b.left)) -
    STRIP_GAP -
    STRIP_STEP * Math.min((block && block.depth) || 0, STRIP_DEPTH_MAX);

  const drawPin = (host, pin, geom, state) => {
    const dim = !!state;
    if (!geom || !geom.boxes.length) return;
    const plain = pinColor(pin);
    const key = pin.uuid;
    const opacity = pinOpacity(pin);
    // `dim` means the annotation is still being written: same colour, dotted,
    // and travelling.
    const paint = (el, vertical) => {
      if (dim) {
        crawlLine(el, plain, vertical);
        if (state === "pending") el.classList.add("tm-hurry");
      } else {
        el.style.background = plain;
        el.style.boxShadow = `0 0 0 1.5px ${darkTint(plain)}`;
      }
    };
    const { scope, boxes } = geom;

    // A rectangle around a region, its letter beside it, and a hit box that
    // depends on whether anything else lives inside: a region that holds
    // others is opened by its edge, so that clicking inside it still reaches
    // what it holds. One that holds nothing is opened anywhere within.
    const drawBox = (nested, enclosed) => {
      const top = Math.min(...boxes.map((b) => b.top));
      const bot = Math.max(...boxes.map((b) => b.bottom));
      const left = Math.min(...boxes.map((b) => b.left));
      const right = Math.max(...boxes.map((b) => b.right));
      const x = left - BOX_INSET;
      const y = top - BOX_INSET;
      const w = right - left + BOX_INSET * 2;
      const h = bot - top + BOX_INSET * 2;
      const el = mark(host, key + ":box", "tm-box");
      if (dim) {
        crawlBox(el, plain);
        if (state === "pending") el.classList.add("tm-hurry");
      } else {
        el.style.border = `2px solid ${plain}`;
        el.style.boxShadow = haloRing(plain);
      }
      el.style.opacity = opacity;
      place(el, x, y, w, h);
      // The letter sits inside the frame's bottom-right corner, and *under*
      // it: the halo that keeps the letter legible would otherwise eat a bite
      // out of the frame it belongs to.
      const glyph = mark(host, key + ":letter", "tm-glyph");
      drawLetter(glyph, pin);
      glyph.style.opacity = opacity;
      place(glyph, x + w - glyph.__w - 3, y + h - glyph.__h - 2);
      if (dim) return;
      // The left side reaches out into the margin: there is nothing there to
      // hit by mistake, and a mark that has to be hit within five pixels is a
      // mark that gets missed.
      const reach = enclosed ? 0 : MARGIN_REACH;
      const x0 = x - REACH - reach;
      const width = w + REACH * 2 + reach;
      const edges = nested
        ? [
            [x0, y - REACH, width, REACH * 2],
            [x0, y + h - REACH, width, REACH * 2],
            [x0, y - REACH, REACH * 2 + reach, h + REACH * 2],
            [x + w - REACH, y - REACH, REACH * 2, h + REACH * 2],
          ]
        : [[x0, y - REACH, width, h + REACH * 2]];
      edges.forEach(([hx, hy, hw, hh], idx) => {
        const hit = mark(host, key + ":hit" + idx, "tm-hit");
        place(hit, hx, hy, hw, hh);
        openFor(hit, pin);
      });
      // The letter is part of the mark, tucked in the corner.
      const label = mark(host, key + ":hitL", "tm-hit");
      place(label, x + w - glyph.__w - 5, y + h - glyph.__h - 4, glyph.__w + 5, glyph.__h + 4);
      openFor(label, pin);
    };

    if (scope === "math.block" || scope === "svg") {
      // A block equation and a drawing both stand on their own, with space
      // around them: a strip down one side would say nothing about where they
      // end. Nothing inside is annotatable on its own, so all of it opens it.
      drawBox(false, geom.enclosed);
      return;
    }
    if (scope === "block" || scope === "para" || scope === "item") {
      const top = Math.min(...boxes.map((b) => b.top));
      const bot = Math.max(...boxes.map((b) => b.bottom));
      // What the mark looks like follows what the thing *is*, not what the
      // annotation calls itself. The paged mode files headings and bare
      // paragraphs under `item` too, and a ring drawn beside a heading circles
      // the empty margin where its bullet would have been.
      const listed = scope === "item" && geom.block && geom.block.el.localName === "li";
      if (listed) {
        // An item wears a mark around its marker, never a strip: the marker is
        // already a vertical cue and a second one reads as clutter. It sits on
        // the item's *first* line — an item of three lines has its bullet at
        // the top, not in the middle.
        const line = markerLine(geom.block, boxes[0]);
        if (ITEM_SHAPE === "ring") {
          const ring = line.ring;
          const el = mark(host, key + ":box", "tm-box");
          el.style.borderRadius = ring.round + "px";
          if (dim) {
            crawlBox(el, plain);
            if (state === "pending") el.classList.add("tm-hurry");
          } else {
            el.style.border = `2px solid ${plain}`;
            el.style.boxShadow = haloRing(plain);
          }
          el.style.opacity = opacity;
          place(el, ring.left, ring.top, ring.width, ring.height);
          const glyph = mark(host, key + ":letter", "tm-glyph");
          drawLetter(glyph, pin);
          place(glyph, ring.left - 4 - glyph.__w, ring.top + ring.height / 2 - glyph.__h / 2);
          if (!dim) {
            const hit = mark(host, key + ":hit", "tm-hit");
            place(
              hit,
              ring.left - 5 - glyph.__w - 4,
              ring.top - 5,
              ring.width + glyph.__w + 18,
              ring.height + 10,
            );
            openFor(hit, pin);
          }
          return;
        }
        const el = mark(host, key + ":chip", "tm-chip");
        el.style.opacity = opacity;
        drawBubble(el, pin, "right", dim, state === "pending");
        place(el, line.point - CHIP_GAP - el.__w, line.mid - el.__h / 2);
        if (!dim) {
          const hit = mark(host, key + ":hit", "tm-hit");
          place(hit, line.left, line.top, line.width, line.height);
          openFor(hit, pin);
        }
        return;
      }
      if (REGION_SHAPE === "box") {
        // The ring, not the whole rectangle: a paragraph is full of words that
        // can be annotated by themselves, and clicking one of them must not
        // open the paragraph instead.
        drawBox(true, (geom.block && geom.block.enclosed) || false);
        return;
      }
      const left = stripLeftOf(geom.block, boxes);
      const strip = mark(host, key + ":strip", "tm-mark");
      paint(strip, true);
      strip.style.opacity = opacity;
      place(strip, left, top, STRIP_W, Math.max(bot - top, 4));
      const glyph = mark(host, key + ":letter", "tm-glyph");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      // Right-aligned against the strip, so the letter reads as its label.
      place(glyph, left - 3 - glyph.__w, (top + bot) / 2 - glyph.__h / 2);
      if (!dim) {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, left - 12, top, 12 + STRIP_W + 5, Math.max(bot - top, 4));
        openFor(hit, pin);
      }
      return;
    }
    if (scope === "edge") {
      const el = mark(host, key + ":edge-caret", "tm-mark tm-over");
      el.style.opacity = opacity;
      const box = boxes[0];
      drawEdgeCaret(el, plain, box, dim);
      if (state === "pending") el.classList.add("tm-hurry");
      place(el, box.left, box.top, box.width, EDGE_H);
      const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      place(glyph, box.right + 4, box.top - 6);
      if (!dim) {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, box.left - 20, box.top - 6, box.width + 40, 14);
        openFor(hit, pin);
      }
      return;
    }
    if (scope === "point") {
      const el = mark(host, key + ":point", "tm-mark tm-over");
      el.style.opacity = opacity;
      drawCaret(el, plain, boxes[0], dim);
      if (state === "pending") el.classList.add("tm-hurry");
      const caretX = boxes[0].left - CARET_W / 2;
      const caretY = boxes[0].top - (el.__h - boxes[0].height) / 2;
      place(el, caretX, caretY);
      // A caret says where, the letter says which: without it a point
      // annotation is the one mark on the page that cannot be told from its
      // neighbour, and there is no chip to read it off either.
      const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      place(glyph, caretX + CARET_W + 2, boxes[0].bottom + 1);
      if (!dim) {
        // Down over the caret and its letter, not merely over the line above
        // them: what the eye takes for the annotation is what the pointer has
        // to be able to reach.
        const hit = mark(host, key + ":hit", "tm-hit");
        const right = caretX + el.__w + 1 + glyph.__w;
        place(
          hit,
          caretX - 4,
          boxes[0].top,
          right + 4 - (caretX - 4),
          boxes[0].height + Math.max(el.__h, glyph.__h) + 5,
        );
        openFor(hit, pin);
      }
      return;
    }
    // Text scopes: an underline under every line the text covers, and the
    // letter tucked below the end of the last one.
    boxes.forEach((b, idx) => {
      const el = mark(host, key + ":u" + idx, "tm-mark");
      paint(el, false);
      el.style.opacity = opacity;
      place(el, b.left, b.bottom + 1, b.width, 2);
      if (dim) return;
      // Down to the underline, not just to the text: the mark is part of the
      // annotation, and pointing at it must open that annotation rather than
      // offer a second one over the same words.
      const hit = mark(host, key + ":h" + idx, "tm-hit");
      place(hit, b.left, b.top, b.width, b.height + 5);
      openFor(hit, pin);
    });
    const last = boxes[boxes.length - 1];
    // Over the frames, unlike a frame's own letter: this one labels a few
    // words, and a region drawn around them must not hide it.
    const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
    glyph.style.opacity = opacity;
    drawLetter(glyph, pin);
    place(glyph, last.right - glyph.__w + 2, last.bottom + 3);
  };

  const drawLetter = (el, pin, anchor) => {
    const letter = (pin.letter || "").toUpperCase();
    const selected = pin.uuid === openUuid;
    const fill = selected ? "#ffffff" : pinColor(pin);
    const halo = darkTint(pinColor(pin));
    const w = Math.max(10, 6 * Math.max(letter.length, 1) + 4);
    const h = 12;
    el.__w = w;
    el.__h = h;
    const start = anchor === "start";
    const sig = ["glyph", letter, fill, halo, anchor].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    el.innerHTML =
      `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
      `<text x="${start ? 2 : w - 2}" y="${h / 2 + 3}"` +
      ` text-anchor="${start ? "start" : "end"}" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace" fill="${fill}"` +
      ` style="paint-order:stroke;stroke:${halo};stroke-width:3px;stroke-linejoin:round">` +
      `${letter}</text></svg>`;
  };

  // The pointer chip: a rounded body with an apex, drawn in the direction it
  // points. Same shape as the paged mode's, so the two modes read alike.
  const drawBubble = (el, pin, dir, hollow, hurry) => {
    const letter = (pin.letter || "").toUpperCase();
    const selected = pin.uuid === openUuid;
    const w = Math.max(15, 5 + 6 * Math.max(letter.length, 1));
    const tri = 6;
    const h = 11;
    const r = 3.5;
    const pad = 2;
    const color = pinColor(pin);
    const border = haloColor();
    // The letter inside the chip is its own colour, taken right down: the chip
    // is filled, so the letter has to be darker than it, and black would read
    // as a different design.
    const fill = darkTint(color);
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
    const sig = ["bubble", letter, color, border, fill, dir, hollow, hurry].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    // A chip for an annotation that has not been made yet is an outline, and
    // the outline travels: the same dots as every other transient mark, drawn
    // as a dashed stroke because a path cannot carry a gradient.
    const shape = hollow
      ? `<path d="${path}" fill="none" stroke="${color}" stroke-width="2"` +
        ` stroke-dasharray="2 3" stroke-linejoin="round" class="tm-crawl-stroke${
          hurry ? " tm-hurry" : ""
        }"></path>`
      : `<path d="${path}" fill="${color}" stroke="${border}" stroke-width="2"` +
        ` stroke-linejoin="round"></path>`;
    el.innerHTML =
      `<svg width="${svgW}" height="${svgH}" viewBox="0 0 ${svgW} ${svgH}">` +
      shape +
      `<text x="${tx}" y="${ty}" text-anchor="middle" font-size="9"` +
      ` font-weight="bold" font-family="ui-monospace,monospace"` +
      ` fill="${hollow ? color : fill}">${letter}</text>` +
      `</svg>`;
  };

  // A position between words is a caret: a line standing in the gap, as tall
  // as a couple of lines of text so that it reads as a place rather than as a
  // mark on a word. One that has not been made yet travels, like every other
  // provisional mark.
  const CARET_W = 2;
  const EDGE_H = 2;
  // As long as the vertical caret is tall, and set off from the block's edge so
  // that it is read as lying between two blocks rather than underlining one.
  const EDGE_W = 34;
  const EDGE_GAP = 3;

  // The same caret, lying down: a line along a block's edge for a position
  // between blocks.
  const drawEdgeCaret = (el, color, box, crawling) => {
    el.__w = box.width;
    el.__h = EDGE_H;
    el.innerHTML = "";
    el.style.borderRadius = "1px";
    el.classList.remove("tm-crawl-h");
    if (crawling) {
      crawlLine(el, color, false);
    } else {
      el.style.background = color;
      el.style.boxShadow = `0 0 0 1.5px ${darkTint(color)}`;
    }
  };
  const caretHeight = (box) => Math.max(Math.round((box.height || 16) * 1.8), 20);
  const drawCaret = (el, color, box, crawling) => {
    const h = caretHeight(box);
    el.__w = CARET_W;
    el.__h = h;
    el.innerHTML = "";
    el.style.width = CARET_W + "px";
    el.style.height = h + "px";
    el.style.borderRadius = "1px";
    el.classList.remove("tm-crawl-v");
    if (crawling) {
      crawlLine(el, color, true);
    } else {
      el.style.background = color;
      el.style.boxShadow = `0 0 0 1.5px ${darkTint(color)}`;
    }
  };

  // Every annotation keeps a chip on a conveyor around the edge of the
  // viewport, whether or not the text it names is on screen. While the anchor
  // is visible the chip rides a lane down the right-hand side, level with it;
  // once the anchor scrolls past the top the chip turns the corner and slides
  // left along the top edge, and the same downwards along the bottom. Nothing
  // in a long document is ever more than one click away, and where a chip is
  // says which direction its text lies in.
  const SLOT = 26;
  const CHIP_H = 20;
  const ROW_INSET = 34;
  // The lane runs down the right-hand edge of the window, where a scrollbar or
  // a margin note would be — not against the text, which would put it in a
  // different place on every window. It gives way only if the text reaches
  // that far.
  const LANE_INSET = 34;
  const laneLeft = () => {
    const doc = document.getElementById(DOC_ID);
    const reserved = doc ? parseFloat(getComputedStyle(doc).paddingRight) || 0 : 0;
    const text = doc ? doc.getBoundingClientRect().right - reserved : 0;
    return Math.max(window.innerWidth - LANE_INSET, text + 12);
  };

  const drawEdgeChips = (host, placements) => {
    for (const [pin, x, y, state, fade] of placements) {
      const el = mark(host, pin.uuid + ":edge", "tm-edge");
      const fresh = el.dataset.state === undefined;
      el.textContent = (pin.letter || "?").toUpperCase();
      el.title = (pin.author ? pin.author + ": " : "") + (pin.content || "");
      const selected = pin.uuid === openUuid;
      // An annotation the server does not have yet is drawn hollow, as its
      // mark is: the outline says the same thing the travelling dots do.
      const hollow = !!pin.state;
      el.style.background = hollow ? "var(--tm-bg)" : pinColor(pin);
      el.style.color = hollow ? pinColor(pin) : darkTint(pinColor(pin));
      el.style.border = hollow ? `1.5px solid ${pinColor(pin)}` : "";
      el.style.opacity = pinOpacity(pin) * (fade === undefined ? 1 : fade);
      el.style.boxShadow = selected
        ? `0 0 6px 2px ${ownColor(pin)}, 0 0 12px 3px ${ownColor(pin)}66`
        : "";
      // A chip in the lane follows its text frame by frame and must not lag
      // behind it; the turns onto and off the rows are what the animation is
      // for.
      const tracking = state === "lane" && el.dataset.state === "lane";
      el.style.transition = fresh || tracking ? "none" : "left 0.25s ease, top 0.25s ease";
      el.style.left = x + "px";
      el.style.top = y + "px";
      el.dataset.state = state;
      el.__pin = pin;
      el.onclick = (ev) => {
        ev.stopPropagation();
        if (openUuid === el.__pin.uuid) {
          closeBox();
        } else {
          if (el.dataset.state !== "lane") scrollToPin(el.__pin);
          showAnnot(el.__pin);
        }
        render();
      };
    }
  };

  // Where each chip goes: the lane if its text is on screen, the rows above and
  // below if it is not. Chips whose anchors share a line would stack on top of
  // each other in the lane, so they fan out leftward in document order.
  const placeChips = (marked) => {
    const lane = laneLeft();
    const above = [];
    const below = [];
    const inLane = [];
    for (const { pin, y } of marked) {
      if (y < ROW_INSET) above.push({ pin, y });
      else if (y > window.innerHeight - ROW_INSET) below.push({ pin, y });
      else inLane.push({ pin, y });
    }
    // Nearest to the viewport at the corner, farther ones extending leftward.
    above.sort((a, b) => b.y - a.y);
    below.sort((a, b) => a.y - b.y);
    // A row of chips for what is off screen is a reminder, not a list: the few
    // nearest the corner are what the reader is about to reach, and the rest
    // fade out rather than filling the margin.
    const ROW_SOLID = 6;
    const ROW_LIMIT = 9;
    const fade = (idx) =>
      idx < ROW_SOLID ? 1 : 1 - (idx - ROW_SOLID + 1) / (ROW_LIMIT - ROW_SOLID + 1);
    const placements = above
      .slice(0, ROW_LIMIT)
      .map(({ pin }, idx) => [pin, lane - SLOT * idx, 10, "top", fade(idx)])
      .concat(
        below
          .slice(0, ROW_LIMIT)
          .map(({ pin }, idx) => [
            pin,
            lane - SLOT * idx,
            window.innerHeight - 30,
            "bottom",
            fade(idx),
          ]),
      );
    // Chips in the lane track their text, and two annotations a line apart
    // would otherwise sit on top of each other. They are stacked instead: each
    // one takes the height it wants, or the first free height below the chip
    // before it. Bucketing by line was the obvious alternative and the wrong
    // one — a chip crossing a bucket boundary jumped sideways, and scrolling
    // made it jitter between the two.
    inLane.sort((a, b) => a.y - b.y);
    let free = -Infinity;
    for (const { pin, y } of inLane) {
      const top = Math.max(y - 10, free);
      free = top + CHIP_H + 2;
      placements.push([pin, lane, top, "lane", 1]);
    }
    return placements;
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
  // The annotations this page is holding on its own: one being written, and
  // any that have been sent and not yet come back. They are drawn like the
  // server's, and their state says how: `draft` while it is being written,
  // `pending` while the server has it and the answer has not arrived.
  let local = [];
  // The id the annotation being written goes under, which is never a uuid.
  const DRAFT_ID = "tinymist-draft";
  const dropLocal = (id) => {
    local = local.filter((pin) => pin.uuid !== id);
  };
  // Which regions already carry a mark, by element and kind, so that the
  // margin that offers a new annotation stops offering one over an old.
  const takenBlocks = new Map();
  const render = () => {
    const host = marksHost();
    for (const el of host.children) el.dataset.seen = "";
    const shown = showResolved ? pins : pins.filter((pin) => !pin.resolved);
    const list = local.length ? shown.concat(local) : shown;
    const marked = [];
    takenBlocks.clear();
    for (const pin of list) {
      const geom = geometryOf(pin);
      if (!geom || !geom.boxes.length) continue;
      const on = (geom.block && geom.block.el) || geom.el;
      if (on) {
        let kinds = takenBlocks.get(on);
        if (!kinds) takenBlocks.set(on, (kinds = new Set()));
        kinds.add((geom.block && geom.block.kind) || geom.scope);
      }
      const top = Math.min(...geom.boxes.map((b) => b.top));
      const bot = Math.max(...geom.boxes.map((b) => b.bottom));
      // The mark itself is only drawn where its text is; the chip is drawn
      // wherever the chip belongs.
      if (bot > 0 && top < window.innerHeight) drawPin(host, pin, geom, pin.state);
      marked.push({ pin, y: (top + bot) / 2 });
    }
    drawEdgeChips(host, placeChips(marked));
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
  const previewUnderline = (rects, strong) => {
    clearHover();
    const host = marksHost();
    for (const b of mergeLines(rects)) {
      const el = hoverMark(host, "tm-mark");
      crawlLine(el, nextColor(), false);
      el.style.opacity = strong ? 1 : 0.85;
      place(el, b.left, b.bottom + 1, b.width, 2);
    }
  };
  const previewRegion = (block) => {
    clearHover();
    const host = marksHost();
    const drawing = block.el.localName === "math" || block.el.localName === "svg";
    const box = blockBox(block.el);
    if (drawing) {
      const el = hoverMark(host, "tm-box");
      crawlBox(el, nextColor());
      place(
        el,
        box.left - BOX_INSET,
        box.top - BOX_INSET,
        box.width + BOX_INSET * 2,
        box.height + BOX_INSET * 2,
      );
      return;
    }
    if (block.kind === "item") {
      const line = markerLine(block, box);
      if (ITEM_SHAPE === "ring") {
        const ring = line.ring;
        const el = hoverMark(host, "tm-box");
        el.style.borderRadius = ring.round + "px";
        crawlBox(el, nextColor());
        place(el, ring.left, ring.top, ring.width, ring.height);
        return;
      }
      const el = hoverMark(host, "tm-chip");
      el.style.pointerEvents = "none";
      drawBubble(el, { uuid: "hover", letter: nextLetter() }, "right", true);
      place(el, line.point - CHIP_GAP - el.__w, line.mid - el.__h / 2);
      return;
    }
    if (REGION_SHAPE === "box") {
      const el = hoverMark(host, "tm-box");
      crawlBox(el, nextColor());
      place(
        el,
        box.left - BOX_INSET,
        box.top - BOX_INSET,
        box.width + BOX_INSET * 2,
        box.height + BOX_INSET * 2,
      );
      return;
    }
    const left = stripLeftOf(block, [box]);
    const el = hoverMark(host, "tm-mark");
    crawlLine(el, nextColor(), true);
    place(el, left, box.top, STRIP_W, Math.max(box.height, 4));
  };
  const previewPoint = (box) => {
    clearHover();
    const host = marksHost();
    const el = hoverMark(host, "tm-mark");
    drawCaret(el, nextColor(), box, true);
    place(el, box.left - CARET_W / 2, box.top - (el.__h - box.height) / 2);
  };

  // The margin band that creates a region annotation: exactly the ground its
  // strip (or its chip) would cover, so what is previewed is what is made, and
  // a region that already has one cannot be given a second.
  // How far either side of a mark counts as being on it, and how much further
  // out its left side reaches, where the margin is empty.
  const REACH = 5;
  const MARGIN_REACH = 25;
  // Whether a point is on the frame of a region's rectangle — the ring around
  // it, not the space inside. The inside belongs to the words in it, which are
  // annotatable in their own right; only the frame says "this whole region".
  const onFrame = (x, y, box, enclosed) => {
    const reach = enclosed ? 0 : MARGIN_REACH;
    const left = box.left - BOX_INSET;
    const top = box.top - BOX_INSET;
    const right = box.right + BOX_INSET;
    const bottom = box.bottom + BOX_INSET;
    if (x < left - REACH - reach || x > right + REACH) return false;
    if (y < top - REACH || y > bottom + REACH) return false;
    return (
      (x >= left - REACH - reach && x <= left + REACH) ||
      Math.abs(x - right) <= REACH ||
      Math.abs(y - top) <= REACH ||
      Math.abs(y - bottom) <= REACH
    );
  };

  const regionZoneAt = (x, y) => {
    let best = null;
    for (const block of blocks) {
      // A region that already carries an annotation of this kind is not
      // offered a second one: the ground under the pointer belongs to the mark
      // that is already there, which is what a click there opens.
      const kind = blockKind(block.el, block.kind);
      const taken = takenBlocks.get(block.el);
      if (taken && taken.has(kind)) continue;
      const box = blockBox(block.el);
      if (kind === "item") {
        // The marker's own ground: the ring around it, and the room its letter
        // takes to the left — and only on the item's first line, where the
        // marker is.
        const line = markerLine(block, box);
        const ring = ITEM_SHAPE === "ring" ? line.ring : null;
        if (ring) {
          if (y < ring.top - REACH || y > ring.top + ring.height + REACH) continue;
          if (x < ring.left - MARGIN_REACH || x > ring.left + ring.width + REACH) continue;
        } else {
          if (y < line.top - 2 || y > line.top + line.height + 2) continue;
          if (x < line.left || x > box.left - 1) continue;
        }
      } else if (REGION_SHAPE === "box") {
        if (!onFrame(x, y, box, block.enclosed)) continue;
      } else {
        if (y < box.top - 2 || y > box.bottom + 2) continue;
        const left = stripLeftOf(block, [box]);
        if (x < left - 12 || x > left + STRIP_W + 5) continue;
      }
      // The innermost region under the pointer: an item beats the paragraph
      // it sits in, a heading beats the section around it.
      const area = box.width * box.height;
      if (!best || area < best.area) best = { ...block, area };
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
    // Leaving a half-written annotation keeps it: it stays on the page as a
    // draft, and clicking its mark takes up where it was left. An empty one is
    // a misclick, and goes.
    if (box && composeActive && !force) {
      const ta = box.querySelector("textarea");
      const text = ta ? ta.value.trim() : "";
      const draft = local.find((pin) => pin.uuid === DRAFT_ID);
      if (text && draft) {
        draft.content = text;
        box.remove();
        if (escHandler) {
          document.removeEventListener("keydown", escHandler, true);
          escHandler = null;
        }
        composeActive = false;
        openUuid = null;
        openSig = null;
        render();
        return true;
      }
    }
    if (box) box.remove();
    if (escHandler) {
      document.removeEventListener("keydown", escHandler, true);
      escHandler = null;
    }
    composeActive = false;
    dropLocal(DRAFT_ID);
    openUuid = null;
    openSig = null;
    render();
    return true;
  };

  const shell = (letter, stateText, acts) => {
    if (!closeBox()) return null;
    const box = document.createElement("div");
    box.id = BOX_ID;
    const hue = letterColor(letter);
    // The window is the annotation's own colour, mixed down to something a
    // paragraph of text can sit on.
    box.style.background = `color-mix(in oklab, ${hue} 16%, #0d0d10)`;
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
    title.style.background = hue;
    const letterEl = document.createElement("span");
    letterEl.style.cssText =
      "font:bold 12px ui-monospace,monospace;text-transform:uppercase;color:" + darkTint(hue);
    letterEl.textContent = letter;
    const right = document.createElement("span");
    right.style.cssText =
      "margin-left:auto;display:inline-flex;align-items:baseline;gap:5px;font-size:11px";
    const actsEl = document.createElement("span");
    actsEl.className = "ta-acts";
    actsEl.style.color = darkTint(hue);
    acts.forEach(([label, fn], idx) => {
      if (idx > 0) {
        const sep = document.createElement("span");
        sep.textContent = "/";
        sep.style.color = darkTint(hue);
        sep.style.opacity = "0.55";
        actsEl.appendChild(sep);
      }
      const act = document.createElement("span");
      act.dataset.act = label;
      act.textContent = label;
      act.onclick = fn;
      actsEl.appendChild(act);
    });
    const state = document.createElement("span");
    // The same dark tint of the annotation's own colour as its letter: white on
    // a light hue is barely there, and every title bar carries a light hue.
    state.style.color = darkTint(hue);
    state.textContent = stateText;
    right.append(actsEl, state);
    title.append(letterEl, right);
    const content = document.createElement("div");
    content.className = "ta-body";
    box.append(title, content);
    document.body.appendChild(box);
    return { box, content };
  };

  const msgRow = (hue, author, time, text, first) => {
    const wrap = document.createElement("div");
    wrap.style.marginTop = first ? "0" : "10px";
    const head = document.createElement("div");
    head.style.cssText = "display:flex;align-items:baseline";
    const who = document.createElement("b");
    who.style.cssText =
      "font-weight:650;font-size:12px;color:" +
      `color-mix(in oklab, ${hue} 30%, #ffffff)`;
    who.textContent = author || "unknown";
    const when = document.createElement("span");
    when.style.cssText = "margin-left:auto;font-size:11px;color:rgba(255,255,255,0.4)";
    when.textContent = time ? timeAgo(time) : "";
    when.title = time || "";
    head.append(who, when);
    const body = document.createElement("div");
    body.style.cssText =
      "margin-top:1px;white-space:pre-wrap;font-size:12.5px;color:" +
      `color-mix(in oklab, ${hue} 22%, #f6f6f8)`;
    body.textContent = text;
    wrap.append(head, body);
    return wrap;
  };

  const replyField = (hue, placeholder, hint, rows, first, onSubmit) => {
    const wrap = document.createElement("div");
    wrap.style.cssText = "position:relative;margin-top:" + (first ? "0" : "14px");
    const ta = document.createElement("textarea");
    ta.rows = rows;
    ta.placeholder = placeholder;
    ta.style.color = `color-mix(in oklab, ${hue} 22%, #f6f6f8)`;
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
      // Enter commits, since a comment is usually one line and sending it is
      // what you came to do; shift-enter is the way to write a second line.
      if (e.key === "Enter" && !e.shiftKey && !e.metaKey && !e.ctrlKey && !e.altKey) {
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
    JSON.stringify([pin.claimed, pin.resolved, pin.mtime, pin.type, pin.letter, pin.author,
                    pin.time, pin.content, pin.discussion]);
  const currentDraft = () => {
    const box = document.getElementById(BOX_ID);
    const ta = box && box.querySelector("textarea");
    return ta ? { draft: ta.value, focused: document.activeElement === ta } : null;
  };
  // Claimed and resolved are separate facts: an agent can be holding something
  // it has already answered, and a reopened thread is not the same as one
  // nobody has touched. Only what is passed is changed.
  // Flags applied here and sent, rather than waited for: setting one is a
  // single small change that either lands or can be clicked again, and a
  // window that does nothing until the server answers reads as broken.
  const flagged = new Map();
  const setFlags = (pin, flags) => {
    Object.assign(pin, flags);
    const held = { ...(flagged.get(pin.uuid) || {}), ...flags };
    flagged.set(pin.uuid, held);
    post("/dev/annotate/flags", { uuid: pin.uuid, ...flags });
    showAnnot(pin, currentDraft());
    render();
  };

  // What the server sent, with any flag this page has set since. The override
  // goes as soon as the server's own copy agrees with it.
  const applyFlags = () => {
    for (const pin of pins) {
      const held = flagged.get(pin.uuid);
      if (!held) continue;
      if (Object.entries(held).every(([key, value]) => pin[key] === value)) {
        flagged.delete(pin.uuid);
        continue;
      }
      Object.assign(pin, held);
    }
  };

  // A pin this page is holding rather than one the server sent: a draft goes
  // back to being written, and one that is waiting to be sent says so.
  const showLocal = (pin) => {
    if (pin.state === "draft") {
      const kind = pin.location && pin.location.type === "word" ? "comment" : pin.location.type;
      compose(kind, pin.location, pin);
    }
  };

  const showAnnot = (pin, restore) => {
    if (pin.state) return showLocal(pin);
    // What it is, not what to do with it: an annotation nothing has happened to
    // is a comment, and saying "open comment" reads as an instruction.
    const state = pin.resolved ? "resolved " : pin.claimed ? "claimed " : "";
    const acts = [];
    if (pin.resolved) acts.push(["reopen", () => setFlags(pin, { resolved: false })]);
    else acts.push(["resolve", () => setFlags(pin, { resolved: true, claimed: false })]);
    acts.push([
      "delete",
      () => {
        post("/dev/annotate/delete", { uuid: pin.uuid }).then(refresh);
        saveDraft(draftKey(pin), "");
        closeBox(true);
      },
    ]);
    const parts = shell(pin.letter || "?", `${state}${pin.type || "comment"}`, acts);
    if (!parts) return;
    openUuid = pin.uuid;
    openSig = pinSig(pin);
    const hue = ownColor(pin);
    parts.content.append(msgRow(hue, pin.author, pin.time, pin.content, true));
    for (const reply of pin.discussion || []) {
      parts.content.append(msgRow(hue, reply.author, reply.time, reply.content, false));
    }
    const { wrap, ta } = replyField(
      hue,
      pin.resolved ? "Type reply to re-open" : "Type reply",
      "⏎ send",
      1,
      false,
      (field) => {
        const text = field.value.trim();
        if (!text) return;
        post("/dev/annotate/reply", { uuid: pin.uuid, text }).then(refresh);
        // Answering a closed thread opens it again: the reply is the point,
        // and it would otherwise land somewhere nobody is looking.
        if (pin.resolved) post("/dev/annotate/flags", { uuid: pin.uuid, resolved: false });
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
  const compose = (kind, location, existing) => {
    const letter = existing ? existing.letter : nextLetter();
    const color = existing ? existing.color : nextColor();
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        const draft = { location, render: renderId, text, color, snapshot: snapshotOf(location) };
        dropDraft(draft);
        // The mark stays where it was put while the server has it, so that
        // pressing return does not make the annotation disappear and come back
        // a moment later. It travels at twice the speed until then.
        const held = `${DRAFT_ID}:${Date.now()}`;
        local.push({
          uuid: held,
          state: "pending",
          letter,
          color,
          location,
          content: text,
          draft,
        });
        send(held);
      }
      closeBox(true);
    };
    const parts = shell(letter, `draft ${existing && existing.kind ? existing.kind : "comment"}`, [
      ["save", () => submit(document.querySelector(`#${BOX_ID} textarea`))],
      ["cancel", () => closeBox()],
    ]);
    if (!parts) return;
    composeActive = true;
    const { wrap, ta } = replyField(letterColor(letter), "Type comment", "⏎ save", 3, true, submit);
    parts.content.append(wrap);
    if (existing && existing.content) {
      ta.value = existing.content;
      ta.dispatchEvent(new Event("input"));
    }
    ta.focus();
    dropLocal(DRAFT_ID);
    local.push({
      uuid: DRAFT_ID,
      state: "draft",
      letter,
      color,
      location,
      content: existing ? existing.content : "",
    });
    render();
  };

  // Sends a pending annotation, and keeps it on the page until the server has
  // it. A send that fails leaves it pending: offline, its mark stands still
  // with everything else, and it goes again when the connection returns.
  const send = (held) => {
    const pin = local.find((waiting) => waiting.uuid === held);
    if (!pin || pin.sending) return Promise.resolve();
    pin.sending = true;
    render();
    return post("/dev/annotate", pin.draft).then((res) => {
      pin.sending = false;
      if (res && res.ok) {
        // Held until the server's own copy arrives, so there is no moment with
        // nothing drawn.
        pin.waiting = res.uuid;
        dropDraft(pin.draft);
        return refresh();
      }
      // A refusal is about this annotation and will not fix itself; no
      // connection is about the page and will.
      keepDraft(pin.draft, res && res.error, online);
      if (online) dropLocal(held);
      render();
    });
  };

  // Everything written while the server was away, sent again now that it is
  // back: what this page is still holding, and what a previous visit left in
  // the browser.
  const flush = () => {
    for (const pin of local.filter((waiting) => waiting.state === "pending")) {
      send(pin.uuid);
    }
    sendStored();
  };

  // Drafts from an earlier visit. Their locations were taken against a
  // rendering this page no longer has, but the server keeps its last renderings
  // and can still read them; one it cannot is reported and kept.
  let sending = false;
  const sendStored = () => {
    if (sending || flying) return;
    const held = readDrafts().filter((draft) => draft.location && draft.render);
    if (!held.length) return;
    sending = true;
    const next = (rest) => {
      if (!rest.length) {
        sending = false;
        return refresh();
      }
      const draft = rest[0];
      return post("/dev/annotate", draft).then((res) => {
        if (res && res.ok) dropDraft(draft);
        else if (online) keepDraft(draft, res && res.error, true);
        // Offline again: leave the rest for the next connection.
        return online ? next(rest.slice(1)) : ((sending = false), undefined);
      });
    };
    next(held);
  };

  // What the annotation was about, in the reader's own words: kept with the
  // record so that an annotation whose anchor is later deleted can still say
  // what it referred to.
  const snapshotOf = (location) => {
    const ref = location.ref || location.begin;
    if (!ref) return undefined;
    if (ref.type === "word") return ref.w;
    if (ref.type === "text_cursor") {
      const run = byUid.get(ref.ref);
      return run ? run.text.slice(0, 60) : undefined;
    }
    const entry = byUid.get(ref.ref);
    return entry ? (entry.el.textContent || "").trim().slice(0, 60) : undefined;
  };

  // Drafts the server has not taken. Kept in the browser so that a comment
  // written offline, or against a document that has since changed, survives a
  // reload and can be submitted or re-placed later.
  const DRAFTS = "tinymist-html-drafts";
  const readDrafts = () => {
    try {
      return JSON.parse(localStorage.getItem(DRAFTS) || "[]");
    } catch (err) {
      return [];
    }
  };
  const writeDrafts = (list) => {
    try {
      localStorage.setItem(DRAFTS, JSON.stringify(list.slice(-32)));
    } catch (err) {}
  };
  const keepDraft = (draft, error, say) => {
    const list = readDrafts().filter((held) => held.text !== draft.text);
    list.push({ ...draft, error: error || null, at: new Date().toISOString() });
    writeDrafts(list);
    // While the connection is down the page already says so; a second line
    // about each comment would only repeat it.
    if (say) showBanner("Not placed; kept locally: " + (error || "the server refused it"));
  };
  const dropDraft = (draft) => {
    writeDrafts(readDrafts().filter((held) => held.text !== draft.text));
  };

  // ----------------------------------------------------------- interaction
  let drag = null;
  let swallowClick = false;
  const onOverlay = (ev) =>
    ev.target &&
    ev.target.closest &&
    ev.target.closest(`#${BOX_ID}, #${MARKS_ID}, #${STATUS_ID}`);

  // Where the pointer was last, so that pressing or releasing the modifier
  // changes what is offered without moving the mouse.
  let pointer = null;

  // The region the pointer is inside, whatever it is over: what a click offers
  // while the modifier is held. A word is inside a paragraph, which is inside a
  // figure — the innermost is the one meant.
  const regionUnder = (x, y) => {
    let el = documentAt(x, y);
    while (el) {
      const entry = byUid.get(uidOf(el));
      if (entry && BLOCK_KINDS.includes(entry.kind)) {
        const taken = takenBlocks.get(entry.el);
        const kind = blockKind(entry.el, entry.kind);
        if (!taken || !taken.has(kind)) return entry;
      }
      el = el.parentElement;
    }
    return null;
  };

  const previewAt = (ev) => {
    if (scrolling) return;
    pointer = { x: ev.clientX, y: ev.clientY, alt: ev.altKey, ctrl: ev.ctrlKey };
    // Held down, the modifier means a position — between two words, or between
    // two blocks — and nothing else. Positions are not offered otherwise: they
    // are places a pointer lands on only by being exact about it, and holding a
    // key is easier than that. It is also what makes snapping to the nearest
    // one safe, since nothing else is competing for the same pixels.
    if (ev.ctrlKey) {
      const spot = positionAt(ev.clientX, ev.clientY);
      if (!spot) return clearHover();
      return spot.edge ? previewEdge(spot.edge) : previewPoint(spot.gap.box);
    }
    if (ev.altKey) {
      const region = regionUnder(ev.clientX, ev.clientY);
      return region ? previewRegion(region) : clearHover();
    }
    const region = regionZoneAt(ev.clientX, ev.clientY);
    if (region) return previewRegion(region);
    const atom = atomAt(ev.clientX, ev.clientY);
    if (atom) {
      if (atom.kind === "math.block" || atom.kind === "svg") {
        return previewRegion({ el: atom.el, kind: "block", depth: 0 });
      }
      return previewUnderline(mergeLines(Array.from(atom.el.getClientRects())), false);
    }
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return clearHover();
    const word = wordAround(caret.run, caret.at);
    if (!word) return clearHover();
    previewUnderline(charRects(word.run.uid, word.start, word.end), false);
  };

  // The position the pointer means: over a line of text, the space between two
  // words on it; anywhere else, the space between the blocks it is between.
  // Text wins wherever there is text, so that a position on a line is never
  // taken to be a position between blocks that happens to be nearer in pixels.
  const positionAt = (x, y) => {
    const caret = caretAt(x, y);
    // Over the text, the pointer means a place on the line and nothing else:
    // falling through to the space between blocks would put a caret there for
    // a click that was aimed at a line.
    if (caret && withinText(caret.run.node, x, y)) {
      const gap = snapPoint(caret, x);
      return gap ? { gap } : null;
    }
    // Code, an equation, a drawing: the pointer is on something rather than
    // between things, and there is no position inside it to offer. Falling
    // through would put a caret between blocks for a pointer that is on a line.
    if (atomElementAt(x, y)) return null;
    const edge = verticalGapAt(x, y, SNAP_REACH);
    if (!edge) return null;
    const away = Math.abs(y - (edge.box.top + EDGE_H / 2));
    return away <= SNAP_NEAR ? { edge } : null;
  };

  // The space between two words nearest the pointer on the line it is over:
  // either side of the word it is nearest, whichever is nearer.
  const snapPoint = (caret, x) => {
    const word = wordAround(caret.run, caret.at);
    const ends = word ? [word.start, word.end] : [caret.at];
    let best = null;
    for (const at of ends) {
      const spot = gapAt({ run: caret.run, at });
      if (!spot) continue;
      const away = Math.abs(x - (spot.box.left + spot.box.width / 2));
      if (!best || away < best.away) best = { spot, away };
    }
    return best && best.spot;
  };

  // A caret lying along a block's edge, for the space between two blocks.
  const previewEdge = (gap) => {
    clearHover();
    const host = marksHost();
    const el = hoverMark(host, "tm-mark");
    drawEdgeCaret(el, nextColor(), gap.box, true);
    place(el, gap.box.left, gap.box.top, gap.box.width, EDGE_H);
  };

  // The insertion point the pointer sits at: where a chevron would go, and the
  // source offset a point anchor would use (the end of the word to its left).
  const gapAt = (caret) => {
    let run = caret.run;
    let at = Math.min(caret.at, run.text.length);
    while (at > 0 && /\s/.test(run.text[at - 1])) at -= 1;
    if (at === 0) {
      // The space after an inline element belongs to the run that follows it,
      // which has nothing to its left to anchor to. The word on the left is the
      // end of the previous run.
      const before = runBefore(run);
      if (!before) return null;
      run = before;
      at = run.text.length;
      while (at > 0 && /\s/.test(run.text[at - 1])) at -= 1;
      if (at === 0) return null;
    }
    return { run, at, box: gapBox(run.node, at) };
  };

  /// The run before this one that has something in it.
  const runBefore = (run) => {
    const at = runs.indexOf(run);
    for (let i = at - 1; i >= 0; i -= 1) {
      if (runs[i].text && runs[i].text.trim()) return runs[i];
    }
    return null;
  };

  // Whether the pointer is inside the box of a run's text, line boxes and all.
  //
  // Strictly inside, vertically: two lines of a list item nearly touch, and the
  // space between them has to be reachable or there would be no way to point
  // between two items. Sideways it reaches a little past the text, so that the
  // start of the first word and the end of the last are positions too.
  const TEXT_INSET = 3;
  const TEXT_SLACK = 20;
  const withinText = (node, x, y) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    return Array.from(range.getClientRects()).some(
      (r) =>
        x >= r.left - TEXT_SLACK &&
        x <= r.right + TEXT_SLACK &&
        y >= r.top + TEXT_INSET &&
        y <= r.bottom - TEXT_INSET,
    );
  };

  // The space a point annotation marks: between the word that ends here and
  // the one that starts next, so the caret is centred in the gap rather than
  // pressed against the word on its left.
  const gapBox = (node, at) => {
    const text = node.nodeValue || "";
    const left = document.createRange();
    left.setStart(node, at);
    left.setEnd(node, at);
    const box = left.getBoundingClientRect();
    let next = at;
    while (next < text.length && /\s/.test(text[next])) next += 1;
    if (next === at || next >= text.length) return box;
    const right = document.createRange();
    right.setStart(node, next);
    right.setEnd(node, next);
    const other = right.getBoundingClientRect();
    // Only when the two are on the same line: a gap that a line break falls
    // into is not a gap the eye sees.
    if (Math.abs(other.top - box.top) > 2) return box;
    const middle = (box.right + other.left) / 2;
    return new DOMRect(middle, box.top, 0, box.height);
  };

  const onMouseDown = (ev) => {
    if (!annotating || ev.button !== 0 || onOverlay(ev)) return;
    if (openUuid !== null || composeActive) return; // this click only dismisses
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    drag = { from: { x: ev.clientX, y: ev.clientY }, start: caret, moved: false };
  };
  // A mouse reports its position far more often than the page is drawn, and
  // every report would otherwise measure the page again. The last one before
  // the next frame is the only one that matters.
  let hovering = null;
  let hoverFrame = 0;
  const hoverSoon = (ev) => {
    hovering = {
      clientX: ev.clientX,
      clientY: ev.clientY,
      altKey: ev.altKey,
      ctrlKey: ev.ctrlKey,
    };
    if (hoverFrame) return;
    hoverFrame = requestAnimationFrame(() => {
      hoverFrame = 0;
      if (hovering) previewAt(hovering);
    });
  };
  const onMouseMove = (ev) => {
    if (!annotating) return;
    if (!drag) {
      if (openUuid !== null || composeActive || onOverlay(ev)) {
        hovering = null;
        return clearHover();
      }
      hoverSoon(ev);
      return;
    }
    if (!drag.moved && Math.hypot(ev.clientX - drag.from.x, ev.clientY - drag.from.y) < 4) {
      return;
    }
    drag.moved = true;
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    // A drag covers whole words: it starts at the beginning of the word it
    // began in and ends at the end of the word it is over.
    const from = wordAround(drag.start.run, drag.start.at);
    const to = wordAround(caret.run, caret.at);
    drag.ends = {
      begin: { run: drag.start.run, at: from ? from.start : drag.start.at },
      end: { run: caret.run, at: to ? to.end : caret.at },
    };
    previewUnderline(
      rectsOf(
        domRange(
          { node: drag.ends.begin.run.node, at: drag.ends.begin.at },
          { node: drag.ends.end.run.node, at: drag.ends.end.at },
        ),
      ),
      true,
    );
  };
  const onMouseUp = (ev) => {
    if (!annotating || !drag) return;
    const d = drag;
    drag = null;
    clearHover();
    if (!d.moved || !d.ends) return;
    const { begin, end } = d.ends;
    const empty = begin.run === end.run && begin.at >= end.at;
    if (empty) return;
    // A drag that never left the word it started on is a plain click.
    const word = wordAround(d.start.run, d.start.at);
    if (word && begin.run === end.run && begin.at === word.start && end.at === word.end) {
      return;
    }
    ev.preventDefault();
    ev.stopImmediatePropagation();
    swallowClick = true;
    compose("span", {
      type: "span.h",
      begin: cursorRef(begin.run, begin.at),
      end: cursorRef(end.run, end.at),
    });
  };

  const composeEdge = (edge) =>
    compose("position", {
      type: "pos.v",
      ref: { type: "node_cursor", ref: edge.block.uid, side: edge.side },
    });

  const onClick = (ev) => {
    if (!annotating) return;
    if (swallowClick) {
      swallowClick = false;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      return;
    }
    if (onOverlay(ev)) return;
    // In annotation mode a link is a thing to annotate; following it would
    // navigate away from the document being annotated.
    if (ev.target.closest && ev.target.closest("#" + DOC_ID + " a")) ev.preventDefault();
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
    if (ev.ctrlKey) {
      const spot = positionAt(ev.clientX, ev.clientY);
      if (!spot) return;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      if (spot.edge) composeEdge(spot.edge);
      else compose("point", { type: "pos.h", ref: cursorRef(spot.gap.run, spot.gap.at) });
      return;
    }
    // Held down, the modifier means the region rather than what is in it.
    const region = ev.altKey
      ? regionUnder(ev.clientX, ev.clientY)
      : regionZoneAt(ev.clientX, ev.clientY);
    if (region) {
      ev.preventDefault();
      ev.stopImmediatePropagation();
      const kind = blockKind(region.el, region.kind);
      compose(kind, nodeLocation(kind, region));
      return;
    }
    const atom = atomAt(ev.clientX, ev.clientY);
    if (atom) {
      ev.preventDefault();
      ev.stopImmediatePropagation();
      const named = {
        math: "equation",
        "math.block": "block equation",
        link: "link",
        raw: "code",
        inline: "inline",
        svg: "drawing",
      };
      compose(named[atom.kind] || atom.kind, nodeLocation(atom.kind, atom));
      return;
    }
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    const word = wordAround(caret.run, caret.at);
    if (!word) return;
    ev.preventDefault();
    ev.stopImmediatePropagation();
    compose("comment", wordLocation("word", word));
  };

  // Up/down arrows walk the annotations in document order — selecting,
  // scrolling to, and opening each — whenever no reply is being typed.
  const onArrow = (e) => {
    if (!annotating) return;
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const box = document.getElementById(BOX_ID);
    const ta = box && box.querySelector("textarea");
    if (ta && ta.value) return;
    const list = pins.filter((pin) => showResolved || !pin.resolved);
    if (!list.length) return;
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
  // How much of the bottom of the window the status panel is taking, published
  // for whatever else lives down there.
  const measureStatus = () => {
    const el = document.getElementById(STATUS_ID);
    const height = el && !el.hidden ? el.offsetHeight : 0;
    document.documentElement.style.setProperty("--tm-status-height", `${height}px`);
  };

  const showStatus = (lines) => {
    const el = document.getElementById(STATUS_ID);
    if (!el) return;
    if (!lines || !lines.length) {
      el.hidden = true;
      el.textContent = "";
      measureStatus();
      return;
    }
    const text = lines.join("\n");
    el.hidden = false;
    el.textContent = text;
    measureStatus();
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

  // The rendering this page is showing, as it was fetched. The document is a
  // file on the server — written when it compiled, not made afresh for each
  // reader — so this is a plain fetch with the browser's own revalidation
  // behind it, and the body is only replaced when it actually differs:
  // rewriting it drops the selection, and every compile would otherwise
  // flicker the page.
  let shownBody = null;
  const loadDocument = () =>
    getJson("/dev/html/doc")
      .then((res) => {
        if (!res || !res.ok) {
          if (!compileErrors) showBanner("Cannot load the document");
          return;
        }
        const doc = document.getElementById(DOC_ID);
        if (!doc) return;
        // The rendering and its map arrive together, so the ids in one always
        // mean what the other says they mean.
        renderId = res.render;
        nodeKinds = {};
        const nodes = (res.map && res.map.nodes) || {};
        for (const uid of Object.keys(nodes)) nodeKinds[uid] = nodes[uid].kind;
        if (shownBody !== res.body) {
          shownBody = res.body;
          doc.innerHTML = res.body;
        }
        // Indexed on every fetch: the map may have changed even when the body
        // did not, and the index is what everything else looks things up in.
        indexDocument();
      })
      .catch(() => {
        if (!compileErrors) showBanner("Cannot load the document");
      });

  const loadPins = () =>
    getJson("/dev/html/pins").then((res) => {
      if (res && res.ok) {
        pins = res.pins || [];
        applyFlags();
      }
    });

  const refresh = () => Promise.all([loadDocument(), loadPins()]).then(() => {
    // A pending annotation the server has now sent back is the server's; drop
    // this page's copy of it.
    local = local.filter(
      (pin) => pin.state !== "pending" || !pins.some((known) => known.uuid === pin.waiting),
    );
    render();
    refreshOpen();
  });

  let assetVersion = null;
  let docVersion = null;
  const listen = () => {
    const sse = new EventSource(url("/dev/diagnostics"));
    sse.onmessage = (ev) => {
      setOnline(true);
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
      // A compile that produced the same page is not a reason to fetch it
      // again: the annotations may have moved, the document has not.
      const version = data.docVersion;
      if (docVersion !== null && version === docVersion) {
        loadPins().then(() => {
          render();
          refreshOpen();
        });
        return;
      }
      docVersion = version === undefined ? docVersion : version;
      refresh();
    };
    sse.onerror = () => {
      // The server went away. What is on the page stays on the page, and says
      // so by standing still.
      setOnline(false);
    };
  };

  // Annotating can be switched off: the marks stay, but the document goes back
  // to being a page — text selects, links are links, nothing is proposed under
  // the pointer. The choice is remembered, since it is about how someone reads
  // rather than about this visit.
  const ANNOTATE_KEY = "tinymist-annotate-on";
  let annotating = ANNOTATE;
  try {
    if (localStorage.getItem(ANNOTATE_KEY) === "off") annotating = false;
  } catch (err) {}

  // Annotations that are finished with are out of the way by default: what is
  // left is what still wants doing.
  const RESOLVED_KEY = "tinymist-show-resolved";
  let showResolved = false;
  try {
    showResolved = localStorage.getItem(RESOLVED_KEY) === "on";
  } catch (err) {}

  // A line floating over the page, for something true right now rather than
  // something that happened: no connection, and nothing else so far.
  const BANNER_ID = "tinymist-banner";
  // Two kinds of line share it: one that is true until it is not — no
  // connection — and one that has just happened. The standing one wins, since
  // a notice about one comment matters less than the page being cut off.
  let standing = null;
  let passing = null;
  let passingTimer = null;
  const paintBanner = () => {
    const text = standing || passing;
    let el = document.getElementById(BANNER_ID);
    if (!text) {
      if (el) el.remove();
      return;
    }
    if (!el) {
      el = document.createElement("div");
      el.id = BANNER_ID;
      document.body.appendChild(el);
    }
    el.textContent = text;
  };
  const holdBanner = (text) => {
    standing = text || null;
    paintBanner();
  };
  const showBanner = (text) => {
    passing = text || null;
    clearTimeout(passingTimer);
    if (passing) passingTimer = setTimeout(() => showBanner(null), 5000);
    paintBanner();
  };

  // Offline is a state of the page, not a message: the marks that travel stop
  // travelling, and start again when the server answers.
  const showConnection = () => {
    document.documentElement.classList.toggle("tm-offline", !online);
    // Its own banner rather than the status panel: this is a passing state of
    // the page, and it should not move the annotation window.
    holdBanner(online ? null : "No connection; changes stored locally");
  };

  // While the server is away, ask for something small every few seconds; the
  // first answer puts the page back together.
  let retrying = null;
  const watchConnection = () => {
    onConnection((up) => {
      showConnection();
      if (up) {
        clearInterval(retrying);
        retrying = null;
        flush();
        refresh();
      } else if (!retrying) {
        retrying = setInterval(() => {
          if (flying) return;
          getJson("/dev/build");
        }, 3000);
      }
    });
  };

  const applyMode = () => {
    const root = document.documentElement;
    root.classList.toggle("tm-annotate", annotating);
    root.classList.toggle("tm-reading", !annotating);
    if (!annotating) {
      clearHover();
      closeBox(true);
    }
    const button = document.getElementById(TOGGLE_ID);
    if (button) {
      // Removed rather than emptied: the style that fills the box matches on
      // the attribute being there, and `data-on=""` is there.
      button.toggleAttribute("data-on", annotating);
      button.title = annotating
        ? "Annotating: click the text to comment on it"
        : "Reading: the document behaves as a page";
    }
  };

  // A checkbox: a box that fills and takes a tick, and a word beside it.
  const checkbox = (id, text, onChange) => {
    const button = document.createElement("button");
    button.id = id;
    button.className = "tm-toggle";
    // The box is an element rather than a character: a glyph is whatever the
    // platform's font has, and this one has to line up with its label.
    const box = document.createElement("span");
    box.className = "tm-check";
    const label = document.createElement("span");
    label.textContent = text;
    button.append(box, label);
    button.onclick = (ev) => {
      ev.stopPropagation();
      onChange(!button.hasAttribute("data-on"));
    };
    return button;
  };

  const buildToggle = () => {
    const host = document.createElement("div");
    host.id = "tinymist-toggles";
    host.appendChild(
      checkbox(TOGGLE_ID, "annotate", (on) => {
        annotating = on;
        try {
          localStorage.setItem(ANNOTATE_KEY, annotating ? "on" : "off");
        } catch (err) {}
        applyMode();
        render();
      }),
    );
    host.appendChild(
      checkbox("tinymist-resolved", "resolved", (on) => {
        showResolved = on;
        try {
          localStorage.setItem(RESOLVED_KEY, showResolved ? "on" : "off");
        } catch (err) {}
        document
          .getElementById("tinymist-resolved")
          .toggleAttribute("data-on", showResolved);
        // An annotation that has just been hidden cannot stay open.
        if (!showResolved) {
          const open = pins.find((pin) => pin.uuid === openUuid);
          if (open && open.resolved) closeBox(true);
        }
        render();
      }),
    );
    // Airplane mode: the page behaves as though the server were unreachable,
    // which is the only way to see the offline behaviour without unplugging
    // something.
    host.appendChild(
      checkbox("tinymist-airplane", "airplane", (on) => {
        flying = on;
        document
          .getElementById("tinymist-airplane")
          .toggleAttribute("data-on", flying);
        if (flying) {
          setOnline(false);
        } else {
          // Back at once rather than at the next tick, so the ants start
          // moving as soon as the box is cleared.
          getJson("/dev/build").then(refresh);
        }
      }),
    );
    document.body.appendChild(host);
    document
      .getElementById("tinymist-resolved")
      .toggleAttribute("data-on", showResolved);
    applyMode();
  };

  if (ANNOTATE) {
    buildToggle();
    document.documentElement.classList.add("tm-annotate");
    window.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("mousemove", onMouseMove, true);
    window.addEventListener("mouseup", onMouseUp, true);
    document.addEventListener("click", onClick, true);
    document.addEventListener("keydown", onArrow, true);
    const modifier = (e) => {
      if ((e.key !== "Alt" && e.key !== "Control") || !pointer) return;
      if (openUuid !== null || composeActive) return;
      const down = e.type === "keydown";
      previewAt({
        clientX: pointer.x,
        clientY: pointer.y,
        altKey: e.key === "Alt" ? down : !!pointer.alt,
        ctrlKey: e.key === "Control" ? down : !!pointer.ctrl,
      });
    };
    document.addEventListener("keydown", modifier, true);
    document.addEventListener("keyup", modifier, true);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && drag) {
        drag = null;
        clearHover();
      }
    });
  }
  // A preview is drawn in viewport coordinates for the thing under the pointer;
  // once the page scrolls under a still pointer, it is a mark for something
  // that is no longer there. It is dropped at the first scroll and stays away
  // until the page settles and the pointer moves again.
  let scrolling = null;
  const onScroll = () => {
    if (!scrolling) clearHover();
    clearTimeout(scrolling);
    scrolling = setTimeout(() => {
      scrolling = null;
    }, 150);
    render();
  };
  document.addEventListener("scroll", onScroll, { capture: true, passive: true });
  // A new width lays the page out again, so nothing that was measured holds.
  window.addEventListener("resize", forget);
  window.addEventListener("resize", render);
  // The panel wraps differently at a different width, so its height is not a
  // thing to measure once.
  window.addEventListener("resize", measureStatus);
  watchConnection();
  refresh()
    .then(() => sendStored())
    .then(listen);
  // Fonts and images settle after the first paint and move everything below
  // them; a slow tick keeps the marks on their text without watching for it.
  setInterval(render, 1000);
})();
