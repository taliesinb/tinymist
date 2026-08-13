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
  const ATOM_ATTR = "data-typst-atom";
  const DOC_ID = "tinymist-doc";
  const MARKS_ID = "tinymist-marks";
  const BOX_ID = "tinymist-annot-box";
  const STATUS_ID = "tinymist-status";
  const TOGGLE_ID = "tinymist-annotate-toggle";
  const ANNOTATE = location.pathname.replace(/\/+$/, "") === "/annotate";

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
  const pinOpacity = (pin) => (pin.status === "resolved" ? RESOLVED_OPACITY : 1);
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
  const pinColor = (pin) =>
    pin.uuid === openUuid
      ? lighten(letterColor(pin.letter), 0.28)
      : letterColor(pin.letter);
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
  let blocks = []; // { el, kind, anchor, lo, hi }
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
    // A Typst `block` is a `<div>`, and a callout, a theorem environment or a
    // definition box is a block before it is anything else. Divs with no text
    // in them — the spacers and wrappers the export leaves behind — are
    // dropped below.
    DIV: "block",
    SECTION: "block",
    ARTICLE: "block",
    ASIDE: "block",
  };
  const BLOCK_SELECTOR = Object.keys(BLOCK_TAGS).join(",");
  // Two runs further apart in the source than this came from different places
  // in the file, not from the same passage. Runs within a passage are usually
  // adjacent, separated only by the markup between them; a helper's own text
  // and the text it was called with are separated by the rest of the helper.
  const CLUSTER_GAP = 96;

  const parseRange = (el, attr) => {
    const raw = el.getAttribute(attr);
    if (!raw) return null;
    const [s, e] = raw.split(":").map((n) => parseInt(n, 10));
    return Number.isFinite(s) && Number.isFinite(e) ? { s, e } : null;
  };

  const indexDocument = () => {
    runs = [];
    blocks = [];
    atoms = [];
    const doc = document.getElementById(DOC_ID);
    if (!doc) return;
    // One walk of the document, in order, over the two things that carry a
    // source position: runs of text, and atoms — the elements annotated whole.
    // A run inside an atom is not a run: an equation has no words, and the
    // text inside a link belongs to the link.
    const atomEls = Array.from(doc.querySelectorAll(ATOM_SELECTOR));
    const pending = [];
    let atomIdx = 0;
    let inside = null;
    for (const el of doc.querySelectorAll(ATOM_SELECTOR + ", [" + TEXT_ATTR + "]")) {
      if (inside && !inside.el.contains(el)) inside = null;
      if (atomIdx < atomEls.length && atomEls[atomIdx] === el) {
        atomIdx += 1;
        if (inside) continue; // an atom within an atom: a link around a `code`
        // A run of pure whitespace is not a thing: an atom for the space
        // between two words draws a mark a couple of pixels wide and offers
        // to annotate nothing at all. A drawing has no text in it and is very
        // much a thing.
        const drawing = el.localName === "svg" || el.localName === "math";
        if (!drawing && !el.textContent.trim()) continue;
        const told = parseInt(el.getAttribute(ATOM_ATTR) || "", 10);
        inside = {
          el,
          scope: atomScope(el),
          own: parseRange(el, SRC_ATTR),
          told: Number.isFinite(told) ? told : null,
        };
        pending.push(inside);
        continue;
      }
      if (inside) continue;
      const range = parseRange(el, TEXT_ATTR);
      const only = el.childNodes.length === 1 ? el.firstChild : null;
      if (!range || !only || only.nodeType !== Node.TEXT_NODE) continue;
      const text = only.nodeValue || "";
      const run = {
        el,
        node: only,
        s: range.s,
        e: range.e,
        text,
        // A run whose source is exactly as long as its text maps character
        // for character; anything else (escapes, smart quotes, a run the
        // compiler assembled) is mapped by clamping, which still lands in
        // the right run.
        // Whether the run's source is exactly what it renders, measured in
        // the bytes the source is counted in.
        exact: range.e - range.s === byteLen(text),
      };
      pending.push(run);
    }

    // A document is written in order, and what it renders comes out in that
    // order — unless a helper made it. Text a helper produced carries the
    // helper's own position, which is somewhere else entirely and, worse, the
    // *same* somewhere for every call: every `#defn(..)` title reports the one
    // position inside `env`. Taken at face value, one title would be every
    // title, and a word annotation would be written into the helper.
    //
    // So a position is believed only while it moves forward with the document.
    // A run that jumps backwards is not this part of the file; it keeps its
    // text but loses its offsets, and is annotated as a whole thing anchored
    // between the runs that still make sense — which is to say, at the call.
    // The mark moves with where each run *starts*, not where it ends: ranges
    // overlap and enclose one another quite legitimately — a heading's
    // numbering reports the range of the whole heading — and taking the end
    // would leave the heading's own text looking like a step backwards.
    let watermark = -Infinity;
    for (const item of pending) {
      if (item.scope) continue;
      if (item.s >= watermark) {
        item.trusted = true;
        watermark = item.s;
      }
    }
    for (const item of pending) {
      if (item.scope || item.trusted) continue;
      item.scope = "inline";
      item.own = { s: item.s, e: item.e };
      item.told = null;
    }
    for (const item of pending) {
      if (!item.scope) runs.push(item);
    }

    // Each atom's anchor, from the runs on either side of it that the document
    // still vouches for.
    for (let i = 0; i < pending.length; i += 1) {
      const atom = pending[i];
      if (!atom.scope) continue;
      let prev = null;
      let next = null;
      for (let j = i - 1; j >= 0; j -= 1) {
        if (pending[j].trusted) {
          prev = pending[j];
          break;
        }
      }
      for (let j = i + 1; j < pending.length; j += 1) {
        if (pending[j].trusted) {
          next = pending[j];
          break;
        }
      }
      const where = atomAnchor(atom.own, atom.told, prev, next);
      if (where) atoms.push({ el: atom.el, scope: atom.scope, ...where });
    }
    for (const el of doc.querySelectorAll(Object.keys(BLOCK_TAGS).join(","))) {
      blocks.push({ el, kind: BLOCK_TAGS[el.tagName], own: parseRange(el, SRC_ATTR) });
    }
    runs.sort((a, b) => a.s - b.s || a.e - b.e);
    // What a region covers is what its text covers. Its own range, where it
    // has one, says only where it began — a label written into a paragraph
    // ends the paragraph's span there — and a region built by a helper has no
    // range of its own at all.
    blocks = blocks
      .map((block) => {
        const inside = runs.filter((run) => block.el.contains(run.node));
        if (!inside.length) return null;
        const body = dominantCluster(inside);
        const own = block.own;
        return {
          ...block,
          // Where a new annotation on this region anchors. A region that knows
          // its own start is anchored there, marker and all — the server moves
          // the label past a bullet or a heading's "=" itself.
          anchor: own ? own.s : body.lo,
          lo: Math.min(own ? own.s : body.lo, body.lo),
          hi: Math.max(own ? own.e : body.hi, body.hi),
        };
      })
      .filter(Boolean);
    blocks.sort((a, b) => a.lo - b.lo || b.hi - a.hi);
    // Regions nest, and so do their marks: a paragraph's strip sits closest to
    // its own text, and every region that contains it stands one step further
    // out. Without that, a definition box and the paragraph inside it put their
    // strips within a few pixels of each other and the outer one can never be
    // pointed at.
    const bySize = blocks.slice().sort((a, b) => a.hi - a.lo - (b.hi - b.lo));
    for (const block of bySize) {
      // Whether anything else encloses it. Only the outermost mark may reach
      // out into the margin; one that reaches from inside another covers its
      // container's frame and takes the clicks meant for it.
      block.enclosed = blocks.some(
        (other) => other !== block && other.el.contains(block.el),
      );
      block.depth = 0;
      for (const inner of bySize) {
        if (inner === block || inner.depth === undefined) continue;
        if (block.el.contains(inner.el)) {
          block.depth = Math.max(block.depth, inner.depth + 1);
        }
      }
    }
  };

  // Content a helper produced carries the source position of the helper, not
  // of the call: a definition box holds its title from wherever `env` was
  // written and its body from where it was used. Those are two passages in one
  // element, and the annotation belongs to the larger — the body.
  const dominantCluster = (rs) => {
    const sorted = rs.slice().sort((a, b) => a.s - b.s);
    const groups = [];
    for (const run of sorted) {
      const last = groups[groups.length - 1];
      if (last && run.s - last.hi <= CLUSTER_GAP) {
        last.hi = Math.max(last.hi, run.e);
        last.weight += run.e - run.s;
      } else {
        groups.push({ lo: run.s, hi: run.e, weight: run.e - run.s });
      }
    }
    return groups.sort((a, b) => b.weight - a.weight)[0];
  };

  // Source offsets are byte offsets — that is what Typst counts in, and what
  // the sidecar's anchors mean — while a JavaScript string is indexed in
  // UTF-16 units. The two agree only while a run is pure ASCII, and a document
  // of mathematics is anything but: without converting, a word's underline
  // lands a few characters further along with every symbol earlier in the run.
  const encoder = new TextEncoder();
  const byteLen = (text) => encoder.encode(text).length;
  // The byte offsets of every index in a run, worked out once.
  const prefixOf = (run) => {
    if (!run.prefix) {
      const prefix = new Array(run.text.length + 1);
      let at = 0;
      prefix[0] = 0;
      for (let i = 0; i < run.text.length; i += 1) {
        at += byteLen(run.text[i]);
        prefix[i + 1] = at;
      }
      run.prefix = prefix;
    }
    return run.prefix;
  };
  // An index in the run's text, as a source offset.
  const byteAt = (run, index) =>
    run.s + prefixOf(run)[Math.max(0, Math.min(index, run.text.length))];

  // The run an offset falls in: the innermost, shortest run that covers it.
  const runAt = (offset) => {
    let best = null;
    for (const run of runs) {
      if (offset < run.s || offset > run.e) continue;
      if (!best || run.e - run.s < best.e - best.s) best = run;
    }
    return best;
  };
  // A source offset, as an index in the run's text. A run whose source is not
  // what it renders — an escape, a smart quote, a ligature — cannot be mapped
  // through, so it is taken whole.
  const charIn = (run, offset) => {
    if (!run.exact) return run.text.length;
    const want = offset - run.s;
    const prefix = prefixOf(run);
    if (want <= 0) return 0;
    if (want >= prefix[run.text.length]) return run.text.length;
    let lo = 0;
    let hi = run.text.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (prefix[mid] < want) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };

  // A DOM position for a source offset, and a DOM range for a source range.
  const pointAt = (offset) => {
    const run = runAt(offset);
    if (run) return { node: run.node, index: charIn(run, offset), run };
    // Not every offset lands in a run: an equation, a link and anything a
    // helper produced have no words to land in. The nearest edge of the
    // nearest run is where that offset is, as far as the document is
    // concerned.
    let before = null;
    let after = null;
    for (const other of runs) {
      if (other.e <= offset && (!before || other.e > before.e)) before = other;
      if (other.s >= offset && (!after || other.s < after.s)) after = other;
    }
    if (before && (!after || offset - before.e <= after.s - offset)) {
      return { node: before.node, index: before.text.length, run: before };
    }
    return after ? { node: after.node, index: 0, run: after } : null;
  };
  // A range between two places in the document, in the document's own terms.
  // Where both ends are known as DOM positions this is exact; going through
  // source offsets is not, because a run whose text is not the same length as
  // its source maps every offset to its end, and two such runs can produce a
  // range that reaches halfway across the paragraph.
  const domRange = (a, b) => {
    const range = document.createRange();
    try {
      range.setStart(a.node, Math.min(a.at, (a.node.nodeValue || "").length));
      range.setEnd(b.node, Math.min(b.at, (b.node.nodeValue || "").length));
    } catch (err) {
      return null;
    }
    if (range.collapsed && (a.node !== b.node || a.at !== b.at)) {
      try {
        range.setStart(b.node, Math.min(b.at, (b.node.nodeValue || "").length));
        range.setEnd(a.node, Math.min(a.at, (a.node.nodeValue || "").length));
      } catch (err) {
        return null;
      }
    }
    return range;
  };
  const runRange = (word) =>
    domRange({ node: word.run.node, at: word.start }, { node: word.run.node, at: word.end });

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
  // The boxes a range covers, as lines rather than as fragments. A range that
  // crosses markup — an equation, a bold word, a label — comes back in pieces
  // with slivers between them, and an underline drawn piece by piece reads as
  // a dashed line with dots in it. One box per line says what the annotation
  // covers.
  const mergeLines = (rects) => {
    const lines = [];
    // Sorted down the page, so a line is built from its own pieces before the
    // next line's arrive.
    const boxes = Array.from(rects).sort((a, b) => a.top - b.top || a.left - b.left);
    for (const box of boxes) {
      if (box.width < 1 || box.height < 1) continue;
      // Same line if they overlap vertically at all: an inline equation is
      // taller than the words around it and sits a little lower, and drawing
      // one underline for the words and another for the equation is two
      // underlines for one line of text.
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

  // The word ending at an offset: what a word anchor names, since the label is
  // written just past the word it belongs to.
  const wordEndingAt = (offset) => {
    const inRun = (run) => {
      const at = charIn(run, offset);
      let end = at;
      while (end > 0 && /\s/.test(run.text[end - 1])) end -= 1;
      let start = end;
      while (start > 0 && !/\s/.test(run.text[start - 1])) start -= 1;
      if (start === end) return null;
      return { run, start, end, s: byteAt(run, start), e: byteAt(run, end) };
    };
    // A word that ends where the next run begins — one before a bold word, an
    // equation, a link — sits in *two* runs by offset, and only the earlier of
    // them has the word in it. Runs that end here are tried first, then the
    // tightest.
    const covering = runs
      .filter((run) => offset >= run.s && offset <= run.e)
      .sort(
        (a, b) =>
          (b.e === offset) - (a.e === offset) || a.e - a.s - (b.e - b.s),
      );
    for (const run of covering) {
      const word = inRun(run);
      if (word) return word;
    }
    return null;
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
    return { run, start, end, s: byteAt(run, start), e: byteAt(run, end) };
  };

  // Where the pointer is, in document terms. Browsers disagree on the name of
  // this, and on nothing else about it.
  const caretAt = (x, y) => {
    // `caretRangeFromPoint` answers with the *nearest* position, which in the
    // page's margin is the nearest word — and a preview would appear for text
    // the pointer is nowhere near. What is actually under the pointer settles
    // it: the column itself, or anything outside the document, is not text.
    const under = document.elementFromPoint(x, y);
    const doc = document.getElementById(DOC_ID);
    if (!under || !doc || under === doc || !doc.contains(under)) return null;
    // An equation or a link is annotated whole; its innards are not words.
    if (under.closest(ATOM_SELECTOR)) return null;
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
    // Nearest is still not under: a point in a callout's padding, or in the
    // space after the last word of a line, resolves to a character somewhere
    // else entirely. The character's own box has to be near the pointer, with
    // enough slack sideways that the gaps between words still count.
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

  // Some things in a document are one thing, not a run of words: an equation
  // has no annotatable halves, and a link is a single destination however many
  // words it wears. They are annotated whole, with the anchor written just
  // after them in the source, and they carry a scope of their own so the
  // annotation still says what it is about.
  //
  //   math   an inline equation, `$x + y$`
  //   math.block  a block equation, `$ x + y $` standing on its own
  //   link   a link, whatever its text
  // `svg` is a drawing the shims laid out and embedded: one picture, with no
  // source position of its own — where it sits is worked out from the text on
  // either side of it, like anything else a helper produced.
  const ATOM_SELECTOR = "math[" + SRC_ATTR + "], svg, a, code, [" + ATOM_ATTR + "]";
  let atoms = []; // { el, scope, anchor }

  const atomScope = (el) => {
    if (el.localName === "math") {
      // `localName`, not `tagName`: MathML is not HTML, and its tag names come
      // back in the case they were written in rather than upper-cased.
      return el.getAttribute("display") === "block" ? "math.block" : "math";
    }
    if (el.localName === "svg") return "svg";
    if (el.localName === "a") return "link";
    if (el.localName === "code") return "raw";
    // Text a call produced: the call is the thing, and the server has said
    // where it ends.
    return "inline";
  };

  // Where an atom's anchor goes: immediately after it in the source.
  //
  // An element's own range is the first choice, but it cannot be trusted on
  // its own — an element a helper produced carries the helper's position, and
  // `#code(vertex_weight)` reports a range in the middle of a `#let` fifty
  // lines up. The runs on either side say where the atom really sits, and an
  // own range that does not lie between them is not this atom's position at
  // all; then the anchor goes where the text after the atom begins, which in
  // the source is exactly past it.
  //
  // The result is a stretch of source rather than a point: an annotation's
  // label is written *into* that stretch, between the atom and the text after
  // it, so the anchor recorded in the document sits at the head of it while a
  // freshly measured "start of the next run" sits after it.
  const atomAnchor = (own, told, prev, next) => {
    const between = (at) =>
      at !== null && (!prev || at >= prev.e - 2) && (!next || at <= next.s + 2);
    // What the server worked out, when it belongs to this part of the file.
    if (between(told)) return { anchor: told, lo: told, hi: told };
    if (own && between(own.s) && between(own.e)) {
      return { anchor: own.e, lo: own.e, hi: next ? Math.max(own.e, next.s) : own.e };
    }
    const lo = prev ? prev.e : next ? next.s : null;
    if (lo === null) return null;
    const hi = next ? next.s : lo;
    return { anchor: hi, lo, hi: Math.max(lo, hi) };
  };

  const atomAt = (x, y) => {
    const under = document.elementFromPoint(x, y);
    let el = under && under.closest && under.closest(ATOM_SELECTOR);
    // Atoms nest — a styled run inside a `code` inside a link — and only the
    // outermost is registered, since that is the thing being annotated. The
    // pointer lands on the innermost, so walk out until something is known.
    while (el) {
      const atom = atoms.find((known) => known.el === el);
      if (atom) return atom;
      el = el.parentElement && el.parentElement.closest(ATOM_SELECTOR);
    }
    return null;
  };

  // The atom an annotation is on: the one whose anchor is where the label was
  // written.
  const atomEndingAt = (offset, scope) =>
    atoms.find(
      (atom) => atom.scope === scope && offset >= atom.lo && offset <= atom.hi,
    ) || null;
  // A call can render as several runs — `#src("src/proof.rs")` arrives as two —
  // and they are one annotation between them, so they are marked as one.
  const atomGroup = (atom) =>
    atom ? atoms.filter((a) => a.scope === atom.scope && a.anchor === atom.anchor) : [];
  const atomBoxes = (atom) =>
    mergeLines(atomGroup(atom).flatMap((a) => Array.from(a.el.getClientRects())));

  // The boxes of whole elements inside a source range, for an annotation that
  // covers something with no text of its own to measure — an equation, a
  // figure, an image.
  const elementBoxesIn = (from, to) => {
    const doc = document.getElementById(DOC_ID);
    if (!doc) return [];
    const boxes = [];
    // Equations only: they are the elements that hold no text of their own and
    // whose range can be believed. Any element would let a paragraph whose own
    // range is a helper's stand in for the annotation.
    for (const el of doc.querySelectorAll("math[" + SRC_ATTR + "]")) {
      const range = parseRange(el, SRC_ATTR);
      if (!range || range.s < from || range.e > to) continue;
      boxes.push(...el.getClientRects());
    }
    return mergeLines(boxes);
  };

  // The innermost block that covers an offset — an item beats the paragraph it
  // sits in, a heading beats the section around it.
  //
  // A region's anchor sits at the head of the region, which in the source is
  // just before its first word: the bracket, the newline and the indent that
  // open a content block are all in between. So an offset a little short of a
  // region still belongs to it, but only if nothing contains it outright.
  const HEAD_SLACK = 48;
  const blockCovering = (offset, kind) => {
    const smallest = (slack, wanted) => {
      let best = null;
      for (const b of blocks) {
        if (wanted && b.kind !== wanted) continue;
        if (offset < b.lo - slack || offset > b.hi) continue;
        if (!best || b.hi - b.lo < best.hi - best.lo) best = b;
      }
      return best;
    };
    // An annotation says what kind of region it is on, and regions nest: a
    // definition box holds paragraphs, and the innermost thing covering the
    // anchor is not necessarily the thing that was annotated.
    return (
      (kind && (smallest(0, kind) || smallest(HEAD_SLACK, kind))) ||
      smallest(0) ||
      smallest(HEAD_SLACK)
    );
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

  const geometryOf = (pin) => {
    const scope = pin.scope || "point";
    if (scope === "span") {
      const range = rangeFor(pin.start, pin.end);
      const boxes = rectsOf(range);
      // A span over an equation has no runs to measure; the equation's own box
      // is the mark.
      return { scope, boxes: boxes.length ? boxes : elementBoxesIn(pin.start, pin.end) };
    }
    if (scope === "math" || scope === "link" || scope === "raw" || scope === "inline") {
      const atom = atomEndingAt(pin.start, scope);
      return atom ? { scope, boxes: atomBoxes(atom) } : null;
    }
    if (scope === "svg") {
      // A drawing is a picture, marked as one: a frame around all of it.
      const atom = atomEndingAt(pin.start, scope);
      if (!atom) return null;
      const box = atom.el.getBoundingClientRect();
      const enclosed = !!(
        atom.el.parentElement && atom.el.parentElement.closest(BLOCK_SELECTOR)
      );
      return { scope, boxes: [box], block: { el: atom.el, depth: 0 }, enclosed };
    }
    if (scope === "math.block") {
      // A block equation is a region of the document, marked like one — but
      // the element spans the whole column while the equation itself is
      // centred in it, so the mark hangs off the equation's own ink.
      const atom = atomEndingAt(pin.start, scope);
      if (!atom) return null;
      const enclosed = !!(
        atom.el.parentElement && atom.el.parentElement.closest(BLOCK_SELECTOR)
      );
      return { scope, boxes: inkOf(atom.el), block: { el: atom.el, depth: 0 }, enclosed };
    }
    if (scope === "word" || scope === "sentence") {
      const word = wordEndingAt(pin.start);
      if (!word) return null;
      return { scope, boxes: rectsOf(runRange(word)) };
    }
    if (scope === "point") {
      const point = pointAt(pin.start);
      if (!point) return null;
      const box = gapBox(point.node, point.index);
      return { scope, boxes: [box], caret: box };
    }
    // A region: the box of the element the anchor landed in, of the kind the
    // annotation was made on.
    const block = blockCovering(pin.start, scope);
    if (!block) return null;
    return { scope, boxes: [block.el.getBoundingClientRect()], block };
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

  const drawPin = (host, pin, geom, dim) => {
    if (!geom || !geom.boxes.length) return;
    const plain = pinColor(pin);
    const key = pin.uuid;
    const opacity = pinOpacity(pin);
    // `dim` means the annotation is still being written: same colour, dotted,
    // and travelling.
    const paint = (el, vertical) => {
      if (dim) {
        crawlLine(el, plain, vertical);
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
        drawBubble(el, pin, "right", dim);
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
    if (scope === "point") {
      const el = mark(host, key + ":point", "tm-glyph tm-over");
      el.style.opacity = opacity;
      drawChevron(el, plain);
      const caretX = boxes[0].left - el.__w / 2;
      const caretY = boxes[0].bottom + 1;
      place(el, caretX, caretY);
      // A caret says where, the letter says which: without it a point
      // annotation is the one mark on the page that cannot be told from its
      // neighbour, and there is no chip to read it off either.
      const glyph = mark(host, key + ":letter", "tm-glyph tm-over");
      glyph.style.opacity = opacity;
      drawLetter(glyph, pin);
      place(glyph, caretX + el.__w + 1, caretY - 1);
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
  const drawBubble = (el, pin, dir, hollow) => {
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
    const sig = ["bubble", letter, color, border, fill, dir, hollow].join("|");
    if (el.dataset.sig === sig) return;
    el.dataset.sig = sig;
    // A chip for an annotation that has not been made yet is an outline, and
    // the outline travels: the same dots as every other transient mark, drawn
    // as a dashed stroke because a path cannot carry a gradient.
    const shape = hollow
      ? `<path d="${path}" fill="none" stroke="${color}" stroke-width="2"` +
        ` stroke-dasharray="2 3" stroke-linejoin="round" class="tm-crawl-stroke"></path>`
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
    for (const [pin, x, y, state] of placements) {
      const el = mark(host, pin.uuid + ":edge", "tm-edge");
      const fresh = el.dataset.state === undefined;
      el.textContent = (pin.letter || "?").toUpperCase();
      el.title = (pin.author ? pin.author + ": " : "") + (pin.content || "");
      const selected = pin.uuid === openUuid;
      el.style.background = pinColor(pin);
      el.style.color = darkTint(pinColor(pin));
      el.style.opacity = pinOpacity(pin);
      el.style.boxShadow = selected
        ? `0 0 6px 2px ${letterColor(pin.letter)}, 0 0 12px 3px ${letterColor(pin.letter)}66`
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
    const placements = above
      .map(({ pin }, idx) => [pin, lane - SLOT * idx, 10, "top"])
      .concat(
        below.map(({ pin }, idx) => [
          pin,
          lane - SLOT * idx,
          window.innerHeight - 30,
          "bottom",
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
      placements.push([pin, lane, top, "lane"]);
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
  let ghost = null;
  const render = () => {
    const host = marksHost();
    for (const el of host.children) el.dataset.seen = "";
    const list = ghost ? pins.concat([ghost]) : pins;
    const marked = [];
    for (const pin of list) {
      const geom = geometryOf(pin);
      if (!geom || !geom.boxes.length) continue;
      const top = Math.min(...geom.boxes.map((b) => b.top));
      const bot = Math.max(...geom.boxes.map((b) => b.bottom));
      // The mark itself is only drawn where its text is; the chip is drawn
      // wherever the chip belongs.
      if (bot > 0 && top < window.innerHeight) drawPin(host, pin, geom, pin === ghost);
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
    const box =
      block.el.localName === "math" ? inkOf(block.el)[0] : block.el.getBoundingClientRect();
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
      drawBubble(el, { uuid: "hover", letter: nextLetter(), status: "created" }, "right", true);
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
    const el = hoverMark(host, "tm-glyph");
    drawChevron(el, nextColor());
    place(el, box.left - el.__w / 2, box.bottom + 1);
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
      const box = block.el.getBoundingClientRect();
      if (block.kind === "item") {
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
    const hue = letterColor(pin.letter);
    parts.content.append(msgRow(hue, pin.author, pin.time, pin.content, true));
    for (const reply of pin.discussion || []) {
      parts.content.append(msgRow(hue, reply.author, reply.time, reply.content, false));
    }
    const { wrap, ta } = replyField(
      hue,
      status === "resolved" ? "Type reply to re-open" : "Type reply",
      "⏎ send",
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
    const { wrap, ta } = replyField(letterColor(letter), "Type comment", "⏎ save", 3, true, submit);
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

  const previewAt = (ev) => {
    if (scrolling) return;
    const region = regionZoneAt(ev.clientX, ev.clientY);
    if (region) return previewRegion(region);
    const atom = atomAt(ev.clientX, ev.clientY);
    if (atom) {
      if (atom.scope === "math.block" || atom.scope === "svg") {
        return previewRegion({ el: atom.el, kind: "block", depth: 0 });
      }
      return previewUnderline(atomBoxes(atom), false);
    }
    const caret = caretAt(ev.clientX, ev.clientY);
    if (!caret) return clearHover();
    const word = wordAround(caret.run, caret.at);
    if (word) {
      previewUnderline(rectsOf(runRange(word)), false);
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
    return { s: byteAt(caret.run, at), box: gapBox(caret.run.node, at) };
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
  const onMouseMove = (ev) => {
    if (!annotating) return;
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
      s: byteAt(drag.start.run, drag.start.at),
      e: byteAt(drag.start.run, drag.start.at),
    };
    const to = wordAround(caret.run, caret.at) || {
      s: byteAt(caret.run, caret.at),
      e: byteAt(caret.run, caret.at),
    };
    drag.range = { s: Math.min(from.s, to.s), e: Math.max(from.e, to.e) };
    const ends = [
      { node: drag.start.run.node, at: from.start !== undefined ? from.start : drag.start.at },
      { node: caret.run.node, at: to.end !== undefined ? to.end : caret.at },
    ];
    previewUnderline(rectsOf(domRange(ends[0], ends[1])), true);
  };
  const onMouseUp = (ev) => {
    if (!annotating || !drag) return;
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
    const region = regionZoneAt(ev.clientX, ev.clientY);
    if (region) {
      ev.preventDefault();
      ev.stopImmediatePropagation();
      // The anchor goes at the head of the region; the server moves it past a
      // list marker so the label binds to the text rather than to the bullet.
      compose(region.kind, { s: region.anchor, scope: region.kind }, {
        scope: region.kind,
        start: region.anchor,
      });
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
      compose(named[atom.scope], { s: atom.anchor, scope: atom.scope }, {
        scope: atom.scope,
        start: atom.anchor,
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
    if (!annotating) return;
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

  // Annotating can be switched off: the marks stay, but the document goes back
  // to being a page — text selects, links are links, nothing is proposed under
  // the pointer. The choice is remembered, since it is about how someone reads
  // rather than about this visit.
  const ANNOTATE_KEY = "tinymist-annotate-on";
  let annotating = ANNOTATE;
  try {
    if (localStorage.getItem(ANNOTATE_KEY) === "off") annotating = false;
  } catch (err) {}

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
      button.dataset.on = annotating ? "1" : "";
      button.textContent = annotating ? "● annotate" : "○ annotate";
      button.title = annotating
        ? "Annotating: click the text to comment on it"
        : "Reading: the document behaves as a page";
    }
  };

  const buildToggle = () => {
    const button = document.createElement("button");
    button.id = TOGGLE_ID;
    button.onclick = (ev) => {
      ev.stopPropagation();
      annotating = !annotating;
      try {
        localStorage.setItem(ANNOTATE_KEY, annotating ? "on" : "off");
      } catch (err) {}
      applyMode();
      render();
    };
    document.body.appendChild(button);
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
  window.addEventListener("resize", render);
  refresh().then(listen);
  // Fonts and images settle after the first paint and move everything below
  // them; a slow tick keeps the marks on their text without watching for it.
  setInterval(render, 1000);
})();
