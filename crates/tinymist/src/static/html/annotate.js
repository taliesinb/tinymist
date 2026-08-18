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
  // Which face this page wears rides in the mode's prefix: `/a/` is the
  // annotator, `/v/` a document served to be read, `/p/` an editor's preview.
  //
  // Read from where the server says it is mounted rather than from the start of
  // the address: published under a path — behind a proxy that strips it — the
  // page is at `/nlab/a/…`, and a page that looked for `/a/` at the front would
  // decide it was not the annotator and quietly show nothing.
  const MOUNT = (() => {
    const said = document.querySelector('meta[name="tm-mount"]');
    return (said && said.content) || "/a/";
  })();
  const ANNOTATE = /\/a\/$/.test(MOUNT);

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
  // The letters this page has given out as well as the ones the server has: a
  // second draft written before the first is sent would otherwise be given the
  // same letter, and the two would be told apart by nothing.
  const nextLetter = () =>
    indexLetter(
      Math.max(
        0,
        ...pins.map((p) => letterIndex(p.letter)),
        ...local.map((p) => letterIndex(p.letter)),
      ) + 1,
    );

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
  // Regions with nothing annotatable inside them.
  const PICTURE_KINDS = ["svg", "image", "math.block"];
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

  // Whether an element holds anything a reader can see: words, or a picture.
  // An image is a picture and has nothing inside it, so the element itself
  // counts as well as its descendants.
  const SEEN_INSIDE = "img,svg,math,table,hr,canvas,video,iframe";
  const hasSubstance = (el) =>
    !!el.textContent.trim() || el.matches(SEEN_INSIDE) || !!el.querySelector(SEEN_INSIDE);

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
      // A block with nothing in it is not a place. The exporter writes the
      // space between two paragraphs as an empty div, and offering it means a
      // frame can be drawn around a gap.
      if (!hasSubstance(block.el)) return false;
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
  // Where the overlay's own corner sits in the coordinates everything is
  // measured in. It is fixed to the viewport, so this is zero — except in a
  // browser that counts client coordinates from the visual viewport while
  // placing fixed elements against the layout one, which is what a hiding
  // toolbar or a pinch zoom does. Measured rather than assumed, so a mark lands
  // where the text it names is either way.
  let originAt = null;
  const forgetOrigin = () => {
    originAt = null;
  };
  const origin = () => {
    if (originAt) return originAt;
    const host = document.getElementById(MARKS_ID);
    if (!host) return { x: 0, y: 0 };
    const box = host.getBoundingClientRect();
    originAt = { x: box.left, y: box.top };
    return originAt;
  };
  const place = (el, x, y, w, h) => {
    const from = origin();
    x -= from.x;
    y -= from.y;
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
  // How far the document has been scrolled, read from the document rather than
  // from `scrollY`. The two disagree while a page is bouncing at its end, and
  // in a browser whose client coordinates follow the visual viewport rather
  // than the layout one; a measurement kept in page coordinates would then be
  // wrong by that difference for as long as it is kept.
  const scrolled = () => {
    const root = document.documentElement.getBoundingClientRect();
    return { x: -root.left, y: -root.top };
  };
  const page = (b) => {
    const by = scrolled();
    return {
      top: b.top + by.y,
      bottom: b.bottom + by.y,
      left: b.left + by.x,
      right: b.right + by.x,
    };
  };
  const viewport = (b) => {
    const by = scrolled();
    const top = b.top - by.y;
    const bottom = b.bottom - by.y;
    const left = b.left - by.x;
    const right = b.right - by.x;
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
    // About the document, so there is nothing in the document to draw it on:
    // it lives in the corner instead.
    if (kind === "document") return { scope: "document", boxes: [] };
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
      case "pos.v": {
        const el = elementOf(loc.ref);
        if (!el) return null;
        const edge = edgeOf(el, loc.ref.side);
        return { scope: "edge", boxes: [edge], caret: edge, el };
      }
      case "span.v": {
        const from = elementOf(loc.begin);
        const to = elementOf(loc.end);
        if (!from || !to) return null;
        const box = sidelineBox(edgeOf(from, loc.begin.side), edgeOf(to, loc.end.side));
        return { scope: "sideline", boxes: [box], caret: box, el: from };
      }
      case "math":
      case "link":
      case "raw":
      case "opaque": {
        const el = elementOf(loc.ref);
        if (!el) return null;
        return { scope: kind, boxes: mergeLines(Array.from(el.getClientRects())), el };
      }
      case "image":
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
        if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
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
        if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
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

    if (scope === "math.block" || scope === "svg" || scope === "image") {
      // A block equation, a drawing and an image all stand on their own, with
      // space around them: a strip down one side would say nothing about where
      // they end. Nothing inside is annotatable on its own, so all of it opens
      // it.
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
            if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
          } else {
            el.style.border = `2px solid ${plain}`;
            el.style.boxShadow = haloRing(plain);
          }
          el.style.opacity = opacity;
          place(el, ring.left, ring.top, ring.width, ring.height);
          const glyph = mark(host, key + ":letter", "tm-glyph");
          drawLetter(glyph, pin);
          place(glyph, ring.left - 4 - glyph.__w, ring.top + ring.height / 2 - glyph.__h / 2);
          {
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
        {
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
      {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, left - 12, top, 12 + STRIP_W + 5, Math.max(bot - top, 4));
        openFor(hit, pin);
      }
      return;
    }
    if (scope === "sideline") {
      const el = mark(host, key + ":sideline", "tm-mark tm-over");
      el.style.opacity = opacity;
      const box = boxes[0];
      drawSideline(el, plain, dim);
      if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
      place(el, box.left, box.top, box.width, box.height);
      const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      place(glyph, box.left - 16, box.top - 2);
      {
        const hit = mark(host, key + ":hit", "tm-hit");
        place(hit, box.left - 6, box.top, 14, box.height);
        openFor(hit, pin);
      }
      return;
    }
    if (scope === "edge") {
      const el = mark(host, key + ":edge-caret", "tm-mark tm-over");
      el.style.opacity = opacity;
      const box = boxes[0];
      drawEdgeCaret(el, plain, box, dim);
      if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
      place(el, box.left, box.top, box.width, EDGE_H);
      const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      place(glyph, box.right + 4, box.top - 6);
      {
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
      if (state === "pending" && !pin.error) el.classList.add("tm-hurry");
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
      {
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

  // The caret a location's reference names.
  const edgeOf = (el, side) =>
    edgeCaretBox(blockBox(el), side === "top" ? "top" : "bottom", el);

  // The stretch between two places between blocks, drawn as a line down the
  // left of everything it covers: an underline turned on its side, on the outer
  // edge of the leftmost of the two carets it runs between.
  const SIDE_W = 2;
  const sidelineBox = (from, to) => {
    const left = Math.min(from.left, to.left);
    const top = Math.min(from.top, to.top);
    const bottom = Math.max(from.bottom, to.bottom);
    return { left, right: left + SIDE_W, top, bottom, width: SIDE_W, height: bottom - top };
  };
  const drawSideline = (el, color, crawling) => {
    el.innerHTML = "";
    el.style.borderRadius = "1px";
    if (crawling) {
      crawlLine(el, color, true);
    } else {
      el.style.background = color;
      el.style.boxShadow = `0 0 0 1.5px ${darkTint(color)}`;
    }
  };
  const previewSideline = (box) => {
    clearHover();
    const host = marksHost();
    const el = hoverMark(host, "tm-mark");
    drawSideline(el, nextColor(), true);
    place(el, box.left, box.top, box.width, box.height);
  };

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
  // Annotations about the document itself: they have nowhere on the page to
  // point at, so they gather in a bubble in the corner — under the row of chips
  // for what is above the window, and inside the lane that runs down its right
  // edge. It is there only when there are any.
  const DOC_BUBBLE = "tinymist-doc-chips";
  const drawDocumentChips = (host, pins) => {
    let bubble = document.getElementById(DOC_BUBBLE);
    if (!pins.length) {
      if (bubble) bubble.remove();
      return;
    }
    if (!bubble) {
      bubble = document.createElement("div");
      bubble.id = DOC_BUBBLE;
      host.appendChild(bubble);
    }
    bubble.dataset.seen = "1";
    bubble.style.right = window.innerWidth - laneLeft() + SLOT + "px";
    for (const el of [...bubble.children]) el.dataset.seen = "";
    for (const pin of pins) {
      let chip = bubble.querySelector(`[data-key="${CSS.escape(pin.uuid)}"]`);
      if (!chip) {
        chip = document.createElement("div");
        chip.className = "tm-edge tm-doc-chip";
        chip.dataset.key = pin.uuid;
        bubble.appendChild(chip);
      }
      chip.dataset.seen = "1";
      chip.textContent = (pin.letter || "?").toUpperCase();
      chip.title = (pin.author ? pin.author + ": " : "") + (pin.content || "");
      const hollow = !!pin.state;
      chip.style.background = hollow ? "var(--tm-bg)" : pinColor(pin);
      chip.style.color = hollow ? pinColor(pin) : darkTint(pinColor(pin));
      chip.style.border = hollow ? `1.5px solid ${pinColor(pin)}` : "";
      chip.style.opacity = pinOpacity(pin);
      chip.style.boxShadow =
        pin.uuid === openUuid
          ? `0 0 6px 2px ${ownColor(pin)}, 0 0 12px 3px ${ownColor(pin)}66`
          : "";
      chip.__pin = pin;
      chip.onclick = (ev) => {
        ev.stopPropagation();
        if (openUuid === chip.__pin.uuid) closeBox();
        else showAnnot(chip.__pin);
        render();
      };
    }
    for (const el of [...bubble.children]) {
      if (!el.dataset.seen) el.remove();
    }
  };

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
  // The id a page-held annotation goes under, which is never one of the
  // server's. Each one gets its own: several may be waiting to be sent, and
  // several may be half written.
  const DRAFT_ID = "tinymist-draft";
  let drafted = 0;
  const draftId = () => `${DRAFT_ID}:${++drafted}:${Date.now()}`;
  // The draft the window is open on, if it is open on one.
  let composing = null;
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
    const aboutDocument = [];
    takenBlocks.clear();
    framed = [];
    for (const pin of list) {
      if (pin.location && pin.location.type === "document") {
        aboutDocument.push(pin);
        continue;
      }
      const geom = geometryOf(pin) || heldGeometry(pin);
      if (!geom || !geom.boxes.length) continue;
      // An annotation the server has not sent back yet is drawn where it was
      // put, even once the rendering it was put against has been replaced: the
      // ids in a new rendering are new, so its location stops meaning anything
      // until the server's own copy of it arrives. It would otherwise vanish
      // for as long as that takes.
      if (pin.state) holdGeometry(pin, geom);
      if (drawable(pin, geom)) framed.push(pin);
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
    drawDocumentChips(host, aboutDocument);
    for (const el of [...host.children]) {
      // A stroke being drawn belongs to the pointer rather than to the
      // annotations, and outlives a redraw the way a hover mark does.
      if (el.classList.contains(HOVER_CLASS) || el.classList.contains("tm-sketch")) continue;
      if (!el.dataset.seen) el.remove();
    }
  };

  // The last place a pin was drawn, in page coordinates so that it stays with
  // the text it was put on while the page scrolls.
  const holdGeometry = (pin, geom) => {
    pin.held = {
      scope: geom.scope,
      boxes: geom.boxes.map((b) => page(b)),
    };
  };
  const heldGeometry = (pin) => {
    if (!pin.held) return null;
    const boxes = pin.held.boxes.map((b) => viewport(b));
    return { scope: pin.held.scope, boxes, caret: boxes[0] };
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
      } else if (PICTURE_KINDS.includes(kind)) {
        // A picture has nothing inside it to annotate instead, so all of it
        // offers itself rather than only its edges.
        if (x < box.left - REACH || x > box.right + REACH) continue;
        if (y < box.top - REACH || y > box.bottom + REACH) continue;
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
      const draft = local.find((pin) => pin.uuid === composing);
      if (text && draft) {
        draft.content = text;
        box.remove();
        if (escHandler) {
          document.removeEventListener("keydown", escHandler, true);
          escHandler = null;
        }
        composeActive = false;
        composing = null;
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
    if (composing) dropLocal(composing);
    composing = null;
    openUuid = null;
    openSig = null;
    render();
    keepHeld();
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
      return;
    }
    // Sent, and not answered for yet. It can be read but not changed: the
    // server has a copy of these words, and a second version of them here
    // would be a third thing that is neither what was sent nor what is on the
    // page. Waiting is the only thing to do with it — unless the server has
    // said no, which is a thing to be told and then to throw away.
    const parts = shell(
      pin.letter || "?",
      pin.error ? "unplaced comment" : "sending comment",
      pin.error
        ? [
            [
              "discard",
              () => {
                dropDraft(pin.draft);
                dropLocal(pin.uuid);
                closeBox(true);
                keepHeld();
                render();
              },
            ],
          ]
        : [],
    );
    if (!parts) return;
    openUuid = pin.uuid;
    openSig = null;
    const hue = letterColor(pin.letter);
    parts.content.append(msgRow(hue, "you", pin.time || new Date().toISOString(), pin.content, true));
    const note = document.createElement("div");
    note.className = "tm-locked";
    note.textContent = pin.error
      ? `Not placed: ${pin.error}. It is kept here and in this browser.`
      : online
        ? "Waiting for the server."
        : "Waiting for the connection; it is kept here until then.";
    parts.content.append(note);
    render();
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
    // An annotation whose place is gone says so, and says what it was about:
    // the words it named are the only way back to what it meant.
    if (pin.orphaned) {
      const note = document.createElement("div");
      note.className = "tm-locked";
      note.textContent =
        `No longer placed: ${pin.orphaned}.` +
        (pin.snapshot ? ` It was about “${pin.snapshot}”.` : "");
      parts.content.append(note);
    }
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
    // Writing an annotation that was left half written takes up that draft;
    // writing a new one starts one of its own. Either way the window is open on
    // exactly one, and the others stay on the page.
    const draftUuid = (existing && existing.uuid) || draftId();
    const submit = (field) => {
      const text = field.value.trim();
      if (text) {
        const draft = { location, render: renderId, text, color, snapshot: snapshotOf(location) };
        dropDraft(draft);
        // The mark stays where it was put while the server has it, so that
        // pressing return does not make the annotation disappear and come back
        // a moment later. It travels at twice the speed until then.
        const held = draftId();
        local.push({
          uuid: held,
          // Which draft this was written as, so that anything drawn on it
          // while it was a draft is filed under the annotation it becomes.
          drawnAs: draftUuid,
          state: "pending",
          letter,
          color,
          location,
          content: text,
          time: new Date().toISOString(),
          draft,
        });
        send(held);
      }
      dropLocal(draftUuid);
      closeBox(true);
      keepHeld();
    };
    const parts = shell(letter, `draft ${existing && existing.kind ? existing.kind : "comment"}`, [
      ["save", () => submit(document.querySelector(`#${BOX_ID} textarea`))],
      ["cancel", () => closeBox()],
    ]);
    if (!parts) return;
    composeActive = true;
    const { wrap, ta } = replyField(letterColor(letter), "Type comment", "⏎ save", 3, true, submit);
    parts.content.append(wrap);
    // From the moment a place is picked, not from the first word: choosing
    // where a comment goes is most of the work of writing one.
    for (const event of ["input", "keyup", "click", "select"]) {
      ta.addEventListener(event, touchHeld);
    }
    if (existing && existing.content) {
      ta.value = existing.content;
      ta.dispatchEvent(new Event("input"));
    }
    touchHeld();
    // A draft the window was opened on, with nothing done to it yet, is
    // deleted by backspace: the annotation was made by a click and this undoes
    // that click. Anything else — a keystroke, a click in the window — makes it
    // an ordinary backspace over the text.
    let untouched = true;
    const touched = () => {
      untouched = false;
    };
    parts.box.addEventListener("mousedown", touched);
    ta.addEventListener("input", touched);
    ta.addEventListener(
      "keydown",
      (e) => {
        if (!untouched) return;
        if (e.key === "Backspace") {
          e.preventDefault();
          e.stopImmediatePropagation();
          dropLocal(draftUuid);
          composing = null;
          closeBox(true);
          render();
          keepHeld();
          return;
        }
        if (e.key.length === 1 || e.key === "Enter" || e.key === "Delete") touched();
      },
      true,
    );
    ta.focus();
    if (existing && existing.at_) {
      const [from, to] = existing.at_;
      try {
        ta.setSelectionRange(from, to);
      } catch (err) {}
    }
    dropLocal(draftUuid);
    composing = draftUuid;
    local.push({
      uuid: draftUuid,
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
        keepHeld();
        // What was drawn on it before it was sent, now that there is an
        // annotation to file the marks under.
        sendMarks(pin.uuid, res.uuid);
        if (pin.drawnAs) sendMarks(pin.drawnAs, res.uuid);
        return refresh();
      }
      // No connection is about the page and will fix itself; a refusal is
      // about this annotation and will not. What was written is still worth
      // keeping, so it is sent again as an annotation about the document: it
      // has to be somewhere, and the place it was about is not available.
      if (!online) {
        keepDraft(pin.draft, res && res.error, false);
        keepHeld();
        render();
        return;
      }
      if (!pin.retried) {
        pin.retried = true;
        pin.draft = { ...pin.draft, location: { type: "document" }, snapshot: pin.draft.snapshot };
        pin.location = { type: "document" };
        keepHeld();
        render();
        return send(held);
      }
      keepDraft(pin.draft, res && res.error, true);
      pin.error = (res && res.error) || "the server refused it";
      keepHeld();
      render();
    });
  };

  // Everything written while the server was away, sent again now that it is
  // back: what this page is still holding, and what a previous visit left in
  // the browser.
  const flush = () => {
    // Only what the server has never taken: one that was accepted before the
    // page reloaded is waiting for its own copy to come back, and sending it
    // again would make two of it.
    for (const pin of local.filter(
      (held) => held.state === "pending" && !held.waiting && !held.error,
    )) {
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
      const onwards = (rest) =>
        online ? next(rest.slice(1)) : ((sending = false), undefined);
      const place = (where) => post("/dev/annotate", { ...draft, location: where });
      return place(draft.location).then((res) => {
        if (res && res.ok) {
          dropDraft(draft);
          return onwards(rest);
        }
        // Offline: leave the rest for the next connection.
        if (!online) {
          sending = false;
          return;
        }
        // The server has refused the place. A server that has restarted holds
        // none of the renderings its drafts were written against, so the place
        // can never be found again and asking on every visit only produces the
        // same refusal. The words are what matter: they go on the document,
        // where the snapshot says what they were about.
        return place({ type: "document" }).then((moved) => {
          if (moved && moved.ok) {
            dropDraft(draft);
            showBanner("Kept as a note on the document: " + (res.error || "the place is gone"));
          } else {
            keepDraft(draft, (moved && moved.error) || res.error, true);
          }
          return onwards(rest);
        });
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

  // Everything this page is holding: the annotations the server does not have,
  // which of them is open, what is being typed into it, and where the page was
  // left. The page reloads itself whenever the server's assets change, and all
  // of that would otherwise go with it — including the work of having chosen
  // where a comment goes, which is most of the work of writing one.
  //
  // One store for drafts and pending annotations alike, since they are the same
  // thing at different stages and differ only in whether they have been sent.
  // Why the page reloaded, written just before it does and read once it is
  // back. The page cannot otherwise tell a reload it asked for from one the
  // reader asked for.
  const RELOADED = `tinymist-html-reloaded:${BASE}`;
  const HELD = `tinymist-html-held:${BASE}`;
  const HELD_FIELDS = [
    "uuid",
    "state",
    "letter",
    "color",
    "location",
    "content",
    "time",
    "draft",
    "waiting",
    "error",
    "held",
  ];
  let heldTimer = null;
  // Nothing is written until what was held has been read: the page reads it
  // once the document and the annotations are in, and everything that happens
  // before then — the first refresh, the browser restoring a scroll position —
  // would otherwise write an empty page over it.
  let resumed = false;
  // Called from everything that changes what is held, which is a lot of small
  // changes in a row while somebody types.
  const touchHeld = () => {
    clearTimeout(heldTimer);
    heldTimer = setTimeout(keepHeld, 150);
  };
  const keepHeld = () => {
    clearTimeout(heldTimer);
    if (!resumed || restoring) return;
    try {
      const box = document.getElementById(BOX_ID);
      const ta = box && box.querySelector("textarea");
      const open = local.some((pin) => pin.uuid === openUuid) ? openUuid : composing;
      const record = {
        pins: local.map((pin) => {
          const kept = {};
          for (const field of HELD_FIELDS) {
            if (pin[field] !== undefined) kept[field] = pin[field];
          }
          return kept;
        }),
        open: open || null,
        // What is in the field is not in the draft until the window is closed.
        typing: ta && composing ? { text: ta.value, at: [ta.selectionStart, ta.selectionEnd] } : null,
        // Only used when nothing is open: with something open, that is where
        // the page should be looking.
        scroll: window.scrollY,
        at: Date.now(),
      };
      // Where the reader had got to is worth keeping on its own. A document
      // being annotated is read over days, and opening it again at the top is
      // opening it in the wrong place; the page has nothing else to hold when
      // nothing is being written, which is most of the time.
      if (!record.pins.length && !record.open && !record.scroll) {
        localStorage.removeItem(HELD);
      } else {
        localStorage.setItem(HELD, JSON.stringify(record));
      }
    } catch (err) {}
  };
  const takeHeld = () => {
    try {
      return JSON.parse(localStorage.getItem(HELD) || "null");
    } catch (err) {
      return null;
    }
  };

  // Putting the page back where it was read to.
  //
  // Asking for it once is not enough: the document is still growing when the
  // annotations arrive — fonts are loading, images have no height yet — and a
  // page shorter than the position asked for scrolls as far as it can, which
  // is the top. So it is asked for again until it takes, and nothing is
  // written back in the meantime: the clamped position would otherwise be
  // stored over the one being restored.
  const RESTORE_TRIES = 12;
  const RESTORE_WAIT = 120;
  let restoring = false;
  const restoreScroll = (to) => {
    if (!to) return;
    restoring = true;
    let tries = 0;
    const go = () => {
      window.scrollTo(0, to);
      if (Math.abs(window.scrollY - to) <= 2 || ++tries >= RESTORE_TRIES) {
        restoring = false;
        return;
      }
      setTimeout(go, RESTORE_WAIT);
    };
    go();
    // A document whose last picture decides its height is not finished until
    // everything in it is.
    window.addEventListener("load", go, { once: true });
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
    pointer = {
      x: ev.clientX,
      y: ev.clientY,
      alt: ev.altKey,
      shift: ev.shiftKey,
      meta: ev.metaKey,
    };
    // Held down, shift means a position — between two words, or between two
    // blocks — and nothing else. Positions are not offered otherwise: they are
    // places a pointer lands on only by being exact about it, and holding a key
    // is easier than that. It is also what makes snapping to the nearest one
    // safe, since nothing else is competing for the same pixels.
    //
    // Shift rather than control: on macOS a control-click is a secondary click,
    // and the browser answers it with its own menu.
    // Held down, command means the pen. The pointer shows one wherever it is —
    // in the stroke's colour over a picture, grey over everything else — so
    // that where a drag would draw is something the reader can see rather than
    // remember. Over a picture nothing is annotated on yet, the frame that
    // would be made is shown as well.
    if (ev.metaKey) {
      const target = penTarget(ev.clientX, ev.clientY);
      penCursor(penColor(target));
      if (target && !target.pin) {
        return previewRegion({ el: target.el, kind: "block", depth: 0 });
      }
      return clearHover();
    }
    penCursor(false);
    if (ev.shiftKey) {
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
  // The start of a run is a position of its own: in front of the first word,
  // where a label cannot be written but which the resolver expresses as being
  // to the left of the anchor it writes after that word.
  const gapAt = (caret) => {
    const run = caret.run;
    let at = Math.min(caret.at, run.text.length);
    while (at > 0 && /\s/.test(run.text[at - 1])) at -= 1;
    return { run, at, box: gapBox(run.node, at) };
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

  // -------------------------------------------------------------- the pen
  // An annotation about a picture usually means something about a part of it:
  // this line, that corner, the label in the middle. Holding shift inside the
  // frame of such an annotation turns the pointer into a pen, and dragging
  // draws on the picture.
  //
  // What is drawn is stored as a capture of the annotation: a picture of what
  // was annotated with the marks over it, which is what an agent is handed
  // when it asks what the annotation is about. The page takes the picture
  // itself, because the thing the reader drew on is a laid-out page and the
  // server has only the source it was made from.
  const PEN_W = 5;
  const PEN_INK = "#e8442f";
  // What the pen looks like where there is nothing to draw on.
  const PEN_GREY = "#8b8b93";
  // The locations that are pictures. A heading or a paragraph is text that an
  // agent reads in the source, and a drawing over it would say nothing the
  // source does not.
  const PEN_KINDS = ["svg", "image", "math", "math.block"];
  // A region counts as a picture when it holds one, which is how a figure of a
  // diagram comes to be drawable.
  const PICTURE_INSIDE = "img,svg,math,canvas,video";

  // The annotations whose frames a pen may draw in, refreshed as the page is
  // drawn. The geometry is measured again at the moment it is needed, since
  // the page scrolls between.
  let framed = [];
  const drawable = (pin, geom) => {
    const kind = pin.location && pin.location.type;
    if (!kind || !geom) return false;
    const el = (geom.block && geom.block.el) || geom.el;
    if (!el) return false;
    if (PEN_KINDS.includes(kind)) return true;
    return !!(el.matches(PICTURE_INSIDE) || el.querySelector(PICTURE_INSIDE));
  };

  // The union of what an annotation is drawn over: the frame a pen draws in.
  const frameBox = (geom) => {
    const boxes = geom.boxes.filter((b) => b.width > 0 && b.height > 0);
    if (!boxes.length) return null;
    const left = Math.min(...boxes.map((b) => b.left));
    const top = Math.min(...boxes.map((b) => b.top));
    const right = Math.max(...boxes.map((b) => b.right));
    const bottom = Math.max(...boxes.map((b) => b.bottom));
    return new DOMRect(left, top, right - left, bottom - top);
  };

  // The framed annotation under a point, if the point is in one. The smallest
  // wins: a diagram inside a figure is the thing being pointed at.
  const penAt = (x, y) => {
    let found = null;
    for (const pin of framed) {
      const geom = geometryOf(pin);
      if (!geom) continue;
      const box = frameBox(geom);
      if (!box) continue;
      if (x < box.left || x > box.right || y < box.top || y > box.bottom) continue;
      const el = (geom.block && geom.block.el) || geom.el;
      const area = box.width * box.height;
      if (!found || area < found.area) found = { pin, geom, box, el, area };
    }
    return found;
  };

  // The picture under a point, whether or not anything is annotated there.
  // The innermost wins, since the elements nest: a drawing inside a figure
  // inside a section.
  const pictureUnder = (x, y) => {
    let el = document.elementFromPoint(x, y);
    while (el && el !== document.body) {
      const entry = byUid.get(uidOf(el));
      if (entry) {
        const kind = blockKind(entry.el, entry.kind);
        if (PEN_KINDS.includes(kind)) return { el: entry.el, uid: entry.uid, kind };
      }
      el = el.parentElement;
    }
    return null;
  };

  // The frame a picture is drawn in: the same box the annotation of it would
  // be given, so that what is drawn before there is an annotation lands where
  // it does afterwards.
  const pictureBox = (el, kind) => {
    const boxes = kind === "math.block" ? inkOf(el) : [el.getBoundingClientRect()];
    return frameBox({ boxes });
  };

  // What a drag with the pen would draw on: the annotation whose frame the
  // pointer is in, or the picture it is over, which has none yet.
  const penTarget = (x, y) => {
    const found = penAt(x, y);
    if (found) return found;
    const picture = pictureUnder(x, y);
    if (!picture) return null;
    const box = pictureBox(picture.el, picture.kind);
    return box ? { pin: null, el: picture.el, box, kind: picture.kind, uid: picture.uid } : null;
  };

  // The colour a stroke would be drawn in: the annotation's own, or the one
  // the annotation about to be made will be given.
  const penColor = (target) =>
    (target && target.pin && ownColor(target.pin)) || (target ? nextColor() : PEN_GREY);

  let sketch = null;

  // The pen the pointer becomes. Drawn rather than shipped as a file, because
  // it is drawn in the colour the stroke would be: the reader can see, before
  // pressing anything, both that a drag would draw and what it would draw.
  // Grey says the same thing in the negative — here, nothing.
  const pens = new Map();
  const penBitmap = (color) => {
    const held = pens.get(color);
    if (held) return held;
    const canvas = document.createElement("canvas");
    canvas.width = 16;
    canvas.height = 16;
    const ink = canvas.getContext("2d");
    // A pen seen from the side: pointed at the bottom left, where the hotspot
    // is, and cut off square at the top right. Outlined in near-black so that
    // it reads on a dark page and a light one.
    const barrel = [
      [3.4, 10.8],
      [10.2, 4.0],
      [13.0, 6.8],
      [6.2, 13.6],
    ];
    const nib = [
      [1.0, 15.0],
      [3.4, 10.8],
      [6.2, 13.6],
    ];
    const point = [
      [1.0, 15.0],
      [2.6, 12.6],
      [4.2, 14.2],
    ];
    const shape = (points) => {
      ink.beginPath();
      ink.moveTo(points[0][0], points[0][1]);
      for (const [x, y] of points.slice(1)) ink.lineTo(x, y);
      ink.closePath();
    };
    ink.lineJoin = "round";
    ink.strokeStyle = "#101014";
    ink.lineWidth = 2.2;
    shape(barrel);
    ink.stroke();
    shape(nib);
    ink.stroke();
    ink.fillStyle = color;
    shape(barrel);
    ink.fill();
    ink.fillStyle = "#f4f4f6";
    shape(nib);
    ink.fill();
    // The very point, in the ink the pen would draw with.
    ink.fillStyle = color;
    shape(point);
    ink.fill();
    // The image and its hotspot only: the stylesheet says what to fall back to,
    // and a keyword here would land in the middle of the list and make the
    // whole declaration invalid, which reads as no cursor at all.
    const url = `url("${canvas.toDataURL("image/png")}") 1 15`;
    pens.set(color, url);
    return url;
  };

  // Holding the key shows a pen wherever the pointer is: in the stroke's
  // colour where a drag would draw, and in grey where it would not.
  const penCursor = (color) => {
    const root = document.documentElement;
    root.classList.toggle("tm-pen", !!color);
    if (color) root.style.setProperty("--tm-pen", penBitmap(color));
  };

  // The path as it is being drawn, over the frame and clipped to it.
  const drawSketch = (sk) => {
    const host = marksHost();
    let el = host.querySelector(".tm-sketch");
    if (!el) {
      el = document.createElement("div");
      el.className = "tm-sketch";
      el.innerHTML =
        `<svg><path fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
      host.appendChild(el);
    }
    el.dataset.seen = "1";
    place(el, sk.box.left, sk.box.top, sk.box.width, sk.box.height);
    const path = el.querySelector("path");
    path.setAttribute("stroke", sk.color);
    path.setAttribute("stroke-width", PEN_W);
    path.setAttribute("d", pathData(sk.points));
  };
  const clearSketch = () => {
    const el = document.getElementById(MARKS_ID);
    const drawn = el && el.querySelector(".tm-sketch");
    if (drawn) drawn.remove();
  };

  // A stroke as an SVG path: straight segments between the points the pointer
  // reported, which at the rate a mouse reports them is a curve.
  const pathData = (points) =>
    points.map((p, i) => `${i ? "L" : "M"}${round(p.x)} ${round(p.y)}`).join(" ");
  const round = (n) => Math.round(n * 10) / 10;

  // What the reader drew, in the coordinates of the picture it was drawn on,
  // clipped to the frame. `at` maps a point from the viewport into those
  // coordinates, and `unit` is what one pixel measures there.
  const markupOf = (points, at, unit, clip) => {
    const drawn = pathData(points.map(at));
    const id = `tm-clip-${Math.random().toString(36).slice(2, 8)}`;
    const rect =
      `<rect x="${round(clip.x)}" y="${round(clip.y)}" ` +
      `width="${round(clip.width)}" height="${round(clip.height)}"/>`;
    return (
      `<clipPath id="${id}">${rect}</clipPath>` +
      `<g clip-path="url(#${id})"><path d="${drawn}" fill="none" stroke="${PEN_INK}" ` +
      `stroke-width="${round(PEN_W * unit)}" stroke-linecap="round" stroke-linejoin="round"/></g>`
    );
  };

  // ------------------------------------------------------- taking a picture
  // Three ways, in order of how faithful they are. A drawing is already SVG
  // and is copied as it stands. A picture file is drawn onto a canvas, which
  // is what it is. Anything else — an equation, a table, a figure of several
  // things — is laid out by the browser and has to be rasterised: the element
  // is copied into an SVG `foreignObject`, which the browser will draw as an
  // image, and that image is drawn onto a canvas.
  const soleChild = (el, selector) => {
    if (el.matches(selector)) return el;
    const found = el.querySelectorAll(selector);
    return found.length === 1 ? found[0] : null;
  };

  const svgPicture = (el) => {
    const svg = soleChild(el, "svg");
    if (!svg) return null;
    const copy = svg.cloneNode(true);
    copy.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    if (!copy.getAttribute("xmlns:xlink")) {
      copy.setAttribute("xmlns:xlink", "http://www.w3.org/1999/xlink");
    }
    const box = svg.getBoundingClientRect();
    // Into the drawing's own units, which is what its marks have to be in for
    // the server to put them inside it.
    const ctm = svg.getScreenCTM();
    if (!ctm) return null;
    const inverse = ctm.inverse();
    const at = (p) => {
      const point = new DOMPoint(p.x, p.y).matrixTransform(inverse);
      return { x: point.x, y: point.y };
    };
    const corner = at({ x: box.left, y: box.top });
    const far = at({ x: box.right, y: box.bottom });
    return {
      fmt: "svg",
      data: new XMLSerializer().serializeToString(copy),
      width: Math.round(box.width),
      height: Math.round(box.height),
      at,
      // One pixel, in the drawing's units.
      unit: 1 / (ctm.a || 1),
      clip: new DOMRect(corner.x, corner.y, far.x - corner.x, far.y - corner.y),
    };
  };

  // A canvas holding the element as it is on the page, at the screen's own
  // resolution. Marks over one of these are in CSS pixels from its corner.
  const rasterPicture = (canvas, box) => ({
    fmt: "png",
    data: canvas.toDataURL("image/png").replace(/^data:[^,]*,/, ""),
    width: Math.round(box.width),
    height: Math.round(box.height),
    at: (p) => ({ x: p.x - box.left, y: p.y - box.top }),
    unit: 1,
    clip: new DOMRect(0, 0, box.width, box.height),
  });

  const canvasFor = (box) => {
    const ratio = Math.min(window.devicePixelRatio || 1, 3);
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(box.width * ratio));
    canvas.height = Math.max(1, Math.round(box.height * ratio));
    const ctx = canvas.getContext("2d");
    // A picture is read on a page, and the page has a colour; left transparent
    // it would arrive as ink on nothing.
    ctx.fillStyle = pageIsDark() ? "#111114" : "#ffffff";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.scale(canvas.width / box.width, canvas.height / box.height);
    return { canvas, ctx };
  };

  const imagePicture = (el) => {
    const img = soleChild(el, "img");
    if (!img || !img.complete || !img.naturalWidth) return null;
    const box = img.getBoundingClientRect();
    const { canvas, ctx } = canvasFor(box);
    try {
      ctx.drawImage(img, 0, 0, box.width, box.height);
      return rasterPicture(canvas, box);
    } catch (err) {
      return null;
    }
  };

  // The document's own styles, as text, so that a copy of an element laid out
  // on its own looks the way it does in place: what Typst emitted, and the
  // stylesheet the server was told to inject. Not the annotator's own
  // stylesheet, which is written for a page with an annotator on it and makes
  // the copy come out blank.
  //
  // Read once, on the first rasterisation, so that a page nobody draws on pays
  // nothing for this.
  let sheets = null;
  const styleText = async () => {
    if (sheets !== null) return sheets;
    const parts = [];
    const own = document.getElementById("tinymist-typst-style");
    if (own) parts.push(own.textContent || "");
    for (const link of document.querySelectorAll('link[data-tm-injected="css"]')) {
      try {
        const said = await fetch(link.href);
        if (said.ok) parts.push(await said.text());
      } catch (err) {
        /* a stylesheet that cannot be read is one the copy goes without */
      }
    }
    sheets = parts.join("\n");
    return sheets;
  };

  // Every picture the element holds, as data: an image inside a foreignObject
  // is not loaded, since the browser draws it as an image of its own with no
  // access to anything outside it.
  const inlineImages = async (root) => {
    const images = Array.from(root.querySelectorAll("img"));
    for (const img of images) {
      if (/^data:/.test(img.getAttribute("src") || "")) continue;
      try {
        const bytes = await (await fetch(img.src)).blob();
        img.setAttribute(
          "src",
          await new Promise((done) => {
            const reader = new FileReader();
            reader.onload = () => done(reader.result);
            reader.readAsDataURL(bytes);
          }),
        );
      } catch (err) {
        img.removeAttribute("src");
      }
    }
  };

  const rasterise = async (el) => {
    const box = el.getBoundingClientRect();
    const copy = el.cloneNode(true);
    if (copy.namespaceURI === "http://www.w3.org/1998/Math/MathML") {
      copy.setAttribute("xmlns", "http://www.w3.org/1998/Math/MathML");
    }
    await inlineImages(copy);
    const holder = document.createElement("div");
    holder.setAttribute("xmlns", "http://www.w3.org/1999/xhtml");
    holder.id = DOC_ID;
    holder.className = "tm-doc";
    // The colours the element has on the page, said outright: they are set by
    // the annotator's stylesheet, which the copy does not carry.
    const shown = getComputedStyle(el);
    holder.setAttribute(
      "style",
      `width:${box.width}px;height:${box.height}px;margin:0;` +
        `color:${shown.color};background:${pageIsDark() ? "#111114" : "#ffffff"};` +
        `font:${shown.font || `${shown.fontSize} ${shown.fontFamily}`}`,
    );
    holder.appendChild(copy);
    const style = document.createElement("style");
    style.textContent = await styleText();
    const wrapper = document.createElement("div");
    wrapper.setAttribute("xmlns", "http://www.w3.org/1999/xhtml");
    wrapper.appendChild(style);
    wrapper.appendChild(holder);
    const inner = new XMLSerializer().serializeToString(wrapper);
    const svg =
      `<svg xmlns="http://www.w3.org/2000/svg" width="${Math.ceil(box.width)}" ` +
      `height="${Math.ceil(box.height)}"><foreignObject width="100%" height="100%">` +
      `${inner}</foreignObject></svg>`;
    const img = new Image();
    const drew = await new Promise((done) => {
      img.onload = () => done(true);
      img.onerror = () => done(false);
      img.src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
    });
    if (!drew) return null;
    const { canvas, ctx } = canvasFor(box);
    try {
      ctx.drawImage(img, 0, 0, box.width, box.height);
    } catch (err) {
      return null;
    }
    return rasterPicture(canvas, box);
  };

  const pictureOf = async (el) =>
    svgPicture(el) || imagePicture(el) || (await rasterise(el));

  // ------------------------------------------------------ keeping the marks
  // What was drawn on an annotation the server has not got yet, kept until it
  // has: a draft has no id to file a capture under until it has been sent.
  const heldMarks = new Map();
  const keepMarks = (uuid, capture) => {
    const held = heldMarks.get(uuid) || [];
    held.push(capture);
    heldMarks.set(uuid, held);
  };
  const sendMarks = (draftUuid, uuid) => {
    const held = heldMarks.get(draftUuid);
    if (!held || !held.length) return Promise.resolve();
    heldMarks.delete(draftUuid);
    return held
      .reduce(
        (queue, capture) => queue.then(() => post("/dev/annotate/capture", { ...capture, uuid })),
        Promise.resolve(),
      )
      .then(() => refresh());
  };

  const finishSketch = async (sk) => {
    if (sk.points.length < 2) return;
    const picture = await pictureOf(sk.el);
    if (!picture) {
      showBanner("cannot take a picture of this to draw on", "warn");
      return;
    }
    // The frame, in the picture's own coordinates: what is drawn is clipped to
    // it, so a stroke that ran off the edge stops at the edge.
    const near = picture.at({ x: sk.box.left, y: sk.box.top });
    const far = picture.at({ x: sk.box.right, y: sk.box.bottom });
    const clip = new DOMRect(
      Math.min(near.x, far.x),
      Math.min(near.y, far.y),
      Math.abs(far.x - near.x),
      Math.abs(far.y - near.y),
    );
    const capture = {
      fmt: picture.fmt,
      data: picture.data,
      width: picture.width,
      height: picture.height,
      markup: markupOf(
        sk.points.map((p) => ({ x: p.x + sk.box.left, y: p.y + sk.box.top })),
        picture.at,
        picture.unit,
        clip,
      ),
    };
    // An annotation the server has not got yet has no id to file a capture
    // under, so what was drawn on it waits until it has one.
    if (sk.pin.state || String(sk.pin.uuid).startsWith(DRAFT_ID)) {
      keepMarks(sk.pin.uuid, capture);
      showBanner("the marks go with the annotation when it is sent", "note");
      return;
    }
    const res = await post("/dev/annotate/capture", { ...capture, uuid: sk.pin.uuid });
    if (res && res.ok) {
      showBanner(`marked ${sk.pin.letter || "an annotation"}`, "note");
      refresh();
    } else {
      showBanner((res && res.error) || "the server would not take the marks", "warn");
    }
  };

  const onMouseDown = (ev) => {
    if (!annotating || ev.button !== 0 || onOverlay(ev)) return;
    // Held down, command draws on a picture. Offered even while a window is
    // open, since what is being written is often about the picture being drawn
    // on, and offered on a picture nothing is annotated on yet: the annotation
    // is made by the same press that starts the stroke, since a mark on a
    // picture is a remark about it.
    if (ev.metaKey) {
      const target = penTarget(ev.clientX, ev.clientY);
      if (!target) return;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      let pin = target.pin;
      if (!pin) {
        compose(target.kind, nodeLocation(target.kind, target));
        pin = local.find((held) => held.uuid === composing);
        if (!pin) return;
      }
      sketch = {
        pin,
        el: target.el,
        box: target.box,
        color: ownColor(pin) || PEN_INK,
        points: [{ x: ev.clientX - target.box.left, y: ev.clientY - target.box.top }],
      };
      clearHover();
      // The annotation being drawn on is the one to be looking at. Opened
      // where it is: scrolling to it would take the picture out from under the
      // pointer that is drawing on it.
      if (target.pin && openUuid !== pin.uuid) showAnnot(pin);
      return;
    }
    // Shift drags from one place between blocks to another, which is the
    // stretch of document between them.
    if (ev.shiftKey) {
      if (openUuid !== null || composeActive) return; // this click only dismisses
      const spot = positionAt(ev.clientX, ev.clientY);
      if (!spot || !spot.edge) return;
      vdrag = { from: { x: ev.clientX, y: ev.clientY }, start: spot.edge, moved: false };
      return;
    }
    if (openUuid !== null || composeActive) return; // this click only dismisses
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return;
    drag = { from: { x: ev.clientX, y: ev.clientY }, start: caret, moved: false };
  };
  // A mouse reports its position far more often than the page is drawn, and
  // every report would otherwise measure the page again. The last one before
  // the next frame is the only one that matters.
  let vdrag = null;
  let hovering = null;
  let hoverFrame = 0;
  const hoverSoon = (ev) => {
    hovering = {
      clientX: ev.clientX,
      clientY: ev.clientY,
      altKey: ev.altKey,
      shiftKey: ev.shiftKey,
      metaKey: ev.metaKey,
    };
    if (hoverFrame) return;
    hoverFrame = requestAnimationFrame(() => {
      hoverFrame = 0;
      if (hovering) previewAt(hovering);
    });
  };
  const onMouseMove = (ev) => {
    if (!annotating) return;
    if (sketch) {
      const at = { x: ev.clientX - sketch.box.left, y: ev.clientY - sketch.box.top };
      const last = sketch.points[sketch.points.length - 1];
      // A mouse reports a position far more often than a stroke has corners.
      if (Math.hypot(at.x - last.x, at.y - last.y) >= 2) {
        sketch.points.push(at);
        drawSketch(sketch);
      }
      return;
    }
    if (vdrag) {
      if (!vdrag.moved && Math.hypot(ev.clientX - vdrag.from.x, ev.clientY - vdrag.from.y) < 4) {
        return;
      }
      vdrag.moved = true;
      const spot = positionAt(ev.clientX, ev.clientY);
      if (!spot || !spot.edge) return;
      vdrag.end = spot.edge;
      previewSideline(sidelineBox(vdrag.start.box, vdrag.end.box));
      return;
    }
    if (!drag) {
      if (openUuid !== null || composeActive || onOverlay(ev)) {
        hovering = null;
        clearHover();
        // The pen still works while a window is open: drawing on the picture
        // is part of writing the annotation about it, and the window is where
        // the writing happens.
        if (ev.metaKey && !onOverlay(ev)) {
          penCursor(penColor(penTarget(ev.clientX, ev.clientY)));
        }
        return;
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
    if (sketch) {
      const drawn = sketch;
      sketch = null;
      clearSketch();
      ev.preventDefault();
      ev.stopImmediatePropagation();
      swallowClick = true;
      finishSketch(drawn);
      return;
    }
    if (vdrag) {
      const dragged = vdrag;
      vdrag = null;
      clearHover();
      if (!dragged.moved || !dragged.end) return;
      const { start, end } = dragged;
      // Two names for the same place are one place, and a span needs two.
      if (start.block.uid === end.block.uid && start.side === end.side) return;
      ev.preventDefault();
      ev.stopImmediatePropagation();
      swallowClick = true;
      const [above, below] =
        start.box.top <= end.box.top ? [start, end] : [end, start];
      compose("blocks", {
        type: "span.v",
        begin: { type: "node_cursor", ref: above.block.uid, side: above.side },
        end: { type: "node_cursor", ref: below.block.uid, side: below.side },
      });
      return;
    }
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
    // Command is the pen, which acts on the drag and has nothing to say about
    // a click.
    if (ev.metaKey) return;
    if (ev.shiftKey) {
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
        image: "image",
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
  // Failing to load is only worth saying once it has lasted. The page asks for
  // the document as soon as it opens, which is often before the first compile
  // has landed, and a notice that is followed a moment later by the document
  // itself says nothing except that the page was early.
  const LOAD_GRACE = 3000;
  let loadWait = null;
  const CANNOT_LOAD = "Cannot load the document";
  const cannotLoad = (res) => {
    // The server has nothing yet and knows it: it is compiling, which is worth
    // saying, since a page that shows nothing otherwise looks broken. The
    // status panel already carries a compile's own errors.
    if (compileErrors) return void holdWork(null);
    if (res && res.waiting) return void holdWork("Compiling…");
    if (shownBody) return void showBanner(CANNOT_LOAD);
    if (loadWait) return;
    loadWait = setTimeout(() => {
      loadWait = null;
      if (!shownBody && !compileErrors) showBanner(CANNOT_LOAD);
    }, LOAD_GRACE);
  };
  const loaded = () => {
    clearTimeout(loadWait);
    loadWait = null;
    holdWork(null);
    // Only what a failed load put there: a document loads twice in quick
    // succession — once when the page opens, once when the compile reports —
    // and the second must not take down what the first said.
    if (passing === CANNOT_LOAD) showBanner(null);
  };

  // Whether this page has shown a document yet, for telling the first arrival
  // from every one after it.
  let shown = false;
  const announceLoad = (res) => {
    const first = !shown;
    const changed = !!renderId && res.render !== renderId;
    shown = true;
    // A page that has just reloaded says why it reloaded, and then says the
    // document arrived: the reason is read first and the document is the thing
    // being waited for.
    if (first) showBanner("Document loaded", "note");
    else if (changed) showBanner("Document updated", "ok");
    // The document is loaded twice in quick succession when a page opens —
    // once by the page, once when the first compile reports — and the second
    // is not news.
    else if (Date.now() - announcedAt > SETTLE) showBanner("Document reloaded", "note");
    announcedAt = Date.now();
    const files = (res.map && res.map.files) || [];
    console.log(
      `talimist: ${first ? "loaded" : changed ? "updated" : "reloaded"}`,
      {
        document: files[0] ? files[0].path : documentName(res),
        source: files[0] ? files[0].hash : undefined,
        render: res.render,
        was: renderId || undefined,
        files: files.length,
        serverPid,
      },
    );
  };

  // When the page last said something about a load.
  let announcedAt = 0;
  // How close together two loads have to be to count as one.
  const SETTLE = 2000;

  // Locations this page is holding, carried from the rendering they were taken
  // against to the one now on the page.
  //
  // Drawing them where they last were is what happens without this, and that is
  // right only for as long as the text does not move. A draft written while
  // somebody edits the paragraph above it would otherwise end up pointing at
  // whatever slid into its place.
  const carryLocal = (was) => {
    const held = local.filter((pin) => pin.location);
    if (!held.length) return;
    post("/dev/html/relocate", {
      render: was,
      locations: held.map((pin) => pin.location),
    }).then((res) => {
      if (!res || !res.ok || !Array.isArray(res.locations)) return;
      let moved = 0;
      let lost = 0;
      held.forEach((pin, at) => {
        // A place that is no longer in the document leaves its annotation
        // about the document instead. It keeps its letter and its words, it is
        // drawn in the corner with the others that lost their place, and it can
        // be read and copied — which a mark frozen over text it no longer
        // names cannot claim.
        const now = res.locations[at] || { type: "document" };
        if (!res.locations[at]) lost += 1;
        else moved += 1;
        pin.location = now;
        // What is sent when it goes travels with it: the draft points at the
        // rendering the page is showing, not the one it was written against.
        if (pin.draft) pin.draft = { ...pin.draft, location: now, render: renderId };
        delete pin.held;
      });
      if (moved || lost) console.log("talimist: carried", { moved, lost, render: renderId });
      keepHeld();
      render();
      refreshOpen();
    });
  };

  // The styles the exporter put in the document's own head: the rules an
  // equation needs, and whatever else Typst decides a document cannot do
  // without. They sit after this page's own stylesheet, which is about the page
  // rather than the document, and before a stylesheet the server was told to
  // put in front of documents, which is meant to win.
  const DOC_STYLE_ID = "tinymist-typst-style";
  const applyDocumentStyle = (css) => {
    let el = document.getElementById(DOC_STYLE_ID);
    if (!css) {
      if (el) el.remove();
      return;
    }
    if (!el) {
      el = document.createElement("style");
      el.id = DOC_STYLE_ID;
      const theirs = document.querySelector('link[data-tm-injected="css"]');
      if (theirs) theirs.before(el);
      else document.head.appendChild(el);
    }
    if (el.textContent !== css) el.textContent = css;
  };

  // What to call the document: the file it was rendered from, by name.
  const documentName = (res) => {
    const path = res.map && res.map.files && res.map.files[0] && res.map.files[0].path;
    if (path) return path.replace(/^.*[\\/]/, "");
    return res.title || "the document";
  };

  const loadDocument = () =>
    getJson("/dev/html/doc")
      .then((res) => {
        if (!res || !res.ok) {
          cannotLoad(res);
          return;
        }
        loaded();
        const was = renderId;
        // Which document the page is showing, and which version of it, is the
        // one thing the reader cannot see. The banner says which of the three
        // things happened; the console says everything about it, since a line
        // long enough to be complete is too long to read at a glance.
        announceLoad(res);
        const doc = document.getElementById(DOC_ID);
        if (!doc) return;
        // The rendering and its map arrive together, so the ids in one always
        // mean what the other says they mean.
        renderId = res.render;
        nodeKinds = {};
        const nodes = (res.map && res.map.nodes) || {};
        for (const uid of Object.keys(nodes)) nodeKinds[uid] = nodes[uid].kind;
        applyDocumentStyle(res.style || "");
        if (shownBody !== res.body) {
          shownBody = res.body;
          doc.innerHTML = res.body;
        }
        // Indexed on every fetch: the map may have changed even when the body
        // did not, and the index is what everything else looks things up in.
        indexDocument();
        // What this page is still holding was written against the rendering
        // that has just been replaced, and every id in it means something else
        // now. The server knows where those places went, since it knows what
        // the document was and what it is.
        if (was && was !== renderId) carryLocal(was);
      })
      .catch(() => cannotLoad(null));

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
    // A pending annotation the server has sent back is the server's now. It is
    // recognised by the id the server gave it, or — after a reload, when that
    // id was never learned — by saying the same thing.
    local = local.filter(
      (pin) =>
        pin.state !== "pending" ||
        !pins.some((known) => known.uuid === pin.waiting || known.content === pin.content),
    );
    render();
    keepHeld();
    refreshOpen();
  });

  let assetVersion = null;
  let docVersion = null;
  let serverPid = null;
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
      // Which process is answering, and which version of the page it is
      // serving. A different process is a server that has been restarted; the
      // same process with new assets is this page's own script and styles
      // having changed. Both mean the page reloads, and they are worth telling
      // apart afterwards.
      if (serverPid === null && data.pid) {
        serverPid = data.pid;
        console.log("talimist: connected", {
          serverPid,
          assets: data.assetVersion,
          url: location.href,
        });
      } else if (data.pid && data.pid !== serverPid) {
        try {
          sessionStorage.setItem(RELOADED, `server ${serverPid} ${data.pid}`);
        } catch (err) {}
        return void location.reload();
      }
      if (assetVersion === null) assetVersion = data.assetVersion;
      else if (data.assetVersion !== assetVersion) {
        // The page is about to reload itself, and afterwards it has no way of
        // knowing why: a note left here is what it reads when it comes back.
        try {
          sessionStorage.setItem(RELOADED, `client ${data.assetVersion}`);
        } catch (err) {}
        return void location.reload();
      }
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
  // Three kinds of line share it: one that is true until it is not — no
  // connection — one that is true while something is happening — the first
  // compile — and one that has just happened. In that order, since the page
  // being cut off matters more than what it is waiting for, and both matter
  // more than a notice about one comment.
  let standing = null;
  let working = null;
  let passing = null;
  let passingKind = "warn";
  let passingTimer = null;
  // How long something that has just happened stays on the page, and how long
  // it takes to go once its time is up.
  const PASSING = 5000;
  const PASSING_FADE = 1000;
  const paintBanner = () => {
    const text = standing || working || passing;
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
    // Waiting for something, or being told what was loaded, is not the same as
    // something being wrong, and the two do not look alike.
    el.dataset.kind = standing ? "warn" : working === text ? "note" : passingKind;
    if (text !== el.textContent) el.classList.remove("tm-going");
    el.textContent = text;
  };
  const holdBanner = (text) => {
    standing = text || null;
    paintBanner();
  };
  const holdWork = (text) => {
    working = text || null;
    paintBanner();
  };
  const showBanner = (text, kind) => {
    passing = text || null;
    passingKind = kind || "warn";
    clearTimeout(passingTimer);
    if (passing) {
      // Faded rather than removed, and removed when the fade is done: a line
      // that vanishes mid-sentence reads as a glitch.
      passingTimer = setTimeout(() => {
        const el = document.getElementById(BANNER_ID);
        if (el && !standing && !working) el.classList.add("tm-going");
        passingTimer = setTimeout(() => showBanner(null), PASSING_FADE);
      }, PASSING - PASSING_FADE);
    }
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

  // While the server is there, ask for something small every few seconds as
  // well. The event stream is what usually reports the server going away, but a
  // connection that is cut rather than closed — the machine sleeps, the network
  // drops, Safari holds a stream open that nothing is behind — never errors,
  // and the page would go on believing it is connected.
  const BEAT = 5000;
  const BEAT_WAIT = 4000;
  const beat = () => {
    if (flying || !online) return;
    const stop = new AbortController();
    const timer = setTimeout(() => stop.abort(), BEAT_WAIT);
    fetch(url("/dev/build"), { signal: stop.signal })
      .then((r) => setOnline(r.ok))
      .catch(() => setOnline(false))
      .finally(() => clearTimeout(timer));
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
      if ((e.key !== "Alt" && e.key !== "Shift" && e.key !== "Meta") || !pointer) return;
      if (openUuid !== null || composeActive) return;
      const down = e.type === "keydown";
      previewAt({
        clientX: pointer.x,
        clientY: pointer.y,
        altKey: e.key === "Alt" ? down : !!pointer.alt,
        shiftKey: e.key === "Shift" ? down : !!pointer.shift,
        metaKey: e.key === "Meta" ? down : !!pointer.meta,
      });
    };
    document.addEventListener("keydown", modifier, true);
    document.addEventListener("keyup", modifier, true);
    document.addEventListener("keyup", (e) => {
      if (e.key === "Meta" && !sketch) penCursor(false);
    }, true);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && (drag || sketch)) {
        drag = null;
        sketch = null;
        clearSketch();
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
    // Where the page has got to, which is the thing it is holding when nothing
    // is being written. Debounced with everything else that is held, so a
    // scroll writes once it stops rather than at every frame of it.
    touchHeld();
    // The overlay's corner moves with a toolbar that hides as the page
    // scrolls, so where a mark has to be put to land on its text moves too.
    forgetOrigin();
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
  window.addEventListener("resize", forgetOrigin);
  window.addEventListener("resize", render);
  // Zooming and a toolbar sliding away move the visible part of the page
  // without scrolling the document.
  if (window.visualViewport) {
    for (const event of ["resize", "scroll"]) {
      window.visualViewport.addEventListener(event, () => {
        forgetOrigin();
        render();
      });
    }
  }
  // The panel wraps differently at a different width, so its height is not a
  // thing to measure once.
  window.addEventListener("resize", measureStatus);
  // What the page was holding when it last stopped: the annotations the server
  // does not have, the one that was open, and — if none was — where the page
  // was scrolled to.
  const resumeHeld = () => {
    if (!ANNOTATE || composeActive || openUuid !== null) return;
    const held = takeHeld();
    resumed = true;
    if (!held || !held.pins) return;
    local = held.pins
      .filter((pin) => pin.location)
      // One the server has taken while this page was away is the server's.
      .filter(
        (pin) =>
          pin.state !== "pending" ||
          !pins.some((known) => known.uuid === pin.waiting || known.content === pin.content),
      );
    // Ids are handed out in order, and the ones just restored are already
    // taken.
    drafted = local.length;
    render();
    const open = held.open && local.find((pin) => pin.uuid === held.open);
    if (open && open.state === "draft") {
      const kind = open.location.type === "word" ? "comment" : open.location.type;
      compose(kind, open.location, {
        ...open,
        content: (held.typing && held.typing.text) || open.content || "",
        at_: held.typing && held.typing.at,
      });
    } else if (open) {
      showLocal(open);
    } else if (held.scroll) {
      restoreScroll(held.scroll);
    }
    // Anything that never reached the server goes again.
    flush();
  };

  // Said once, on the way back in.
  try {
    const why = (sessionStorage.getItem(RELOADED) || "").split(" ");
    if (why[0]) sessionStorage.removeItem(RELOADED);
    if (why[0] === "server") {
      showBanner("Server reloaded", "warn");
      console.log("talimist: server reloaded", { was: why[1], now: why[2] });
    } else if (why[0] === "client") {
      showBanner("Client reloaded", "warn");
      console.log("talimist: client reloaded", { assets: why[1] });
    }
  } catch (err) {}

  watchConnection();
  setInterval(beat, BEAT);
  refresh()
    .then(() => sendStored())
    .then(resumeHeld)
    .then(listen);
  // Where the page is looking is part of what is held, for a reload with
  // nothing open.
  document.addEventListener("scroll", touchHeld, { capture: true, passive: true });
  // Fonts and images settle after the first paint and move everything below
  // them; a slow tick keeps the marks on their text without watching for it.
  setInterval(render, 1000);
})();
