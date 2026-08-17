// Shims for Typst's HTML export.
//
// HTML export is incomplete: several layout elements are dropped along with
// everything inside them (`align`, `grid`, `line`, `pad`, `place`, `stack`,
// `v`), and others survive only as bare tags with their appearance discarded
// (`block`, `box` lose fill, inset, radius and stroke). A document that centres
// its title inside an `#align` therefore has no title at all in HTML, which
// reads as a bug in the preview rather than as a gap in the exporter.
//
// These rules recover the content and approximate the appearance in CSS. The
// server evaluates this file as a module and installs the rules it exports as
// default show rules, so the document itself needs no changes. The file is read
// from disk when a server starts, so an edit needs no rebuild — but it does need
// the server restarted.
//
// Everything here assumes the HTML target — `html.elem` is an error under
// paged export — and the server only installs it when serving HTML.

// -- small conversions from Typst values to CSS ------------------------------

// Lengths and ratios print as valid CSS ("8pt", "100%"). A relative length is
// a sum of the two, which Typst prints as "0% + 8pt" and CSS spells `calc`;
// when one half is zero — nearly always — the other half stands alone.
// Fractions ("1fr") have no CSS spelling, and anything else is not a size.
#let _len(v) = {
  if type(v) == length or type(v) == ratio {
    repr(v)
  } else if type(v) == relative {
    if v.ratio == 0% {
      repr(v.length)
    } else if v.length == 0pt {
      repr(v.ratio)
    } else {
      "calc(" + repr(v.ratio) + " + " + repr(v.length) + ")"
    }
  } else {
    none
  }
}

#let _color(v) = if type(v) == color {
  v.to-hex()
} else if type(v) == gradient or type(v) == tiling {
  none
} else {
  none
}

// A side-wise value (`inset`, `outset`, `radius`, `stroke`) is either one value
// for every side or a dictionary naming some of them. `x`/`y`/`rest` stand for
// several sides at once.
#let _sides(v) = {
  let out = (:)
  if type(v) == dictionary {
    for side in ("top", "bottom", "left", "right") {
      let picked = v.at(side, default: v.at(
        if side in ("left", "right") { "x" } else { "y" },
        default: v.at("rest", default: none),
      ))
      if picked != none { out.insert(side, picked) }
    }
  } else if v != none {
    for side in ("top", "bottom", "left", "right") { out.insert(side, v) }
  }
  out
}

#let _prop(name, value) = if value == none { () } else { (name + ": " + value,) }

// `padding: 6pt 8pt` and friends, from whichever sides were given.
#let _box-sides(prefix, v, suffix: "") = {
  let sides = _sides(v)
  let out = ()
  for (side, value) in sides {
    let len = _len(value)
    if len != none {
      out += _prop(prefix + "-" + side + suffix, len)
    }
  }
  out
}

#let _radius(v) = {
  if type(v) == dictionary {
    let out = ()
    for (corner, value) in v {
      let len = _len(value)
      if len == none { continue }
      let css = (
        "top-left": "border-top-left-radius",
        "top-right": "border-top-right-radius",
        "bottom-left": "border-bottom-left-radius",
        "bottom-right": "border-bottom-right-radius",
        "rest": "border-radius",
      ).at(corner, default: none)
      if css != none { out += _prop(css, len) }
    }
    out
  } else {
    _prop("border-radius", _len(v))
  }
}

#let _one-stroke(s) = {
  if s == none or s == auto { return none }
  let s = stroke(s)
  let thickness = _len(s.thickness)
  let paint = _color(s.paint)
  if thickness == none and paint == none { return none }
  let width = if thickness == none { "1pt" } else { thickness }
  let color = if paint == none { "currentColor" } else { paint }
  width + " solid " + color
}

#let _border(v) = {
  if v == none or v == auto { return () }
  if type(v) == dictionary {
    let out = ()
    for (side, value) in _sides(v) {
      let css = _one-stroke(value)
      if css != none { out += _prop("border-" + side, css) }
    }
    out
  } else {
    _prop("border", _one-stroke(v))
  }
}

#let _style(parts) = {
  let parts = parts.filter(p => p != none)
  if parts.len() == 0 { "" } else { parts.join("; ") }
}

// An element that CSS has nothing to say about is better left as its content
// than wrapped in an empty tag.
#let _wrap(tag, style, body) = if style == "" {
  body
} else {
  html.elem(tag, attrs: (style: style), body)
}

// Ink, as opposed to ground: a colour meant to be read *on* the page. Dark
// inks were chosen against white, so under a dark theme they are lifted rather
// than mirrored — a mid grey becomes a light grey, an accent keeps its hue and
// gains lightness.
#let _ink(v) = {
  if type(v) != color { return none }
  // Plain black is not a decision, it is the default; the page's own text
  // colour already follows the reader's theme.
  if v == black or v == luma(0%) { return none }
  let (l, a, b, ..) = oklab(v).components()
  let dark = if l < 62% { oklab(92% - l * 0.55, a, b).to-hex() } else { _color(v) }
  let light = _color(v)
  if dark == light { light } else { "light-dark(" + light + ", " + dark + ")" }
}

// The text size in effect around a run, recorded by the containers on the way
// past. A run that changes its size can only say so as a ratio — CSS sizes are
// relative to the parent element — and this is what it is a ratio of.
// Nothing until something says: a guessed size would make every block in a
// document set in another size carry a ratio against a number nobody chose.
#let _base-size = state("talimist-base-size", none)

// How one size compares with another, as CSS says it: `0.8em`, or nothing when
// the two are the same size.
#let _size-em(size, base) = {
  if base == none or base == 0pt { return none }
  let ratio = size / base
  if calc.abs(ratio - 1.0) < 0.03 { none } else {
    str(calc.round(ratio, digits: 3)) + "em"
  }
}

// The size a run is compared against, for the length of one element.
//
// Recording it is not enough, twice over. A heading is set larger than the
// prose around it, and a base left at the heading's size makes everything after
// it look smaller than it is — so what sets it puts it back. And a run inside
// an element of another size agrees with the new base and says nothing, so the
// element itself has to say what its runs no longer do: a raw block set at 8pt
// in a 10pt document would otherwise reach the page at the size of the prose.
//
// `carry: false` is for an element that is already building a wrapper and will
// put the size in it.
#let _in-base(body, carry: true) = context {
  let outer = _base-size.get()
  let own = _size-em(text.size, outer)
  _base-size.update(text.size)
  if own == none or not carry {
    body
  } else {
    html.elem("div", attrs: (style: "font-size: " + own), body)
  }
  _base-size.update(outer)
}

// Drawings — cetz canvases, fletcher diagrams, anything that puts shapes on a
// coordinate grid — are laid out rather than written, and HTML export drops
// every piece of them: a diagram becomes an empty box. `html.frame` lays
// content out the paged way and embeds the result as inline SVG, which is
// exactly what a drawing wants, so a block that contains shapes is framed
// whole rather than translated.
#let _shapes = (curve, polygon, circle, ellipse, rect, square, move, place)

#let _sole-child(it) = {
  let fields = it.fields()
  if "body" in fields and type(fields.body) == content { return fields.body }
  if "children" in fields and type(fields.children) == array and fields.children.len() == 1 {
    return fields.children.first()
  }
  none
}

// What "100%" means once a drawing is framed. A frame is laid out with no page
// to be a fraction of, so a container asking for 70% of the width would get 70%
// of nothing; this is the width it is measured against instead, and it matches
// the text column the stylesheet sets.
#let _column = 34em

// Inside a frame the content is laid out the paged way, and `html.elem` is not
// something that can be made there — an element handed to the paged exporter is
// dropped, taking whatever it wrapped with it, which is how a drawing becomes
// an empty box. The target does not say so (`target()` still reports "html",
// since that is what is being exported *to*), so the frame says it itself, with
// a set rule rather than with state: styles reach into the frame's layout,
// while introspection does not — a state update written beside the framed
// content is not visible from inside it. `zxx` is the language code for "no
// linguistic content", which is what a drawing is.
#let _drawing-lang = "zxx"

/// Lays a drawing out the paged way and frames it at its own size.
///
/// The frame becomes an `<svg>` element as wide as the region it was laid out
/// in, and that element is what a reader sees a box drawn around when the
/// drawing is annotated — so a frame the width of the column puts a box the
/// width of the column around a picture half that wide. Measured first, then
/// laid out again at the width it needs, the element is the picture.
///
/// Content that asks for a fraction of the page has no size of its own to
/// measure: it gets the column to be a fraction of, and is centred in it as
/// the paged rendering would centre it.
#let _frame-drawing(it) = html.frame(context {
  let body = {
    set text(lang: _drawing-lang)
    it
  }
  // How wide the drawing is when the column is what its fractions are of. The
  // layout below has to happen in the column all the same — a box asking for
  // 70% of the page, laid out again in its own width, would ask for 70% of
  // that and shrink — so the drawing is laid out wide and shown narrow.
  // In points, both of them: an em is only a length once something has
  // resolved it against the text size, and the two cannot be compared before
  // that.
  let column = measure(block(width: _column)).width
  let ink = measure(body, width: column)
  if ink.width > 0pt and ink.width < column {
    // Laid out in the column, so a drawing asking for a fraction of the page
    // still gets one, and placed rather than flowed, so the box around it is
    // the size of the drawing rather than of the column it was measured in.
    // Placing is what keeps the two apart: a placed thing has a position and
    // no size, so the box keeps the size given here and the drawing sits in
    // the middle of it, whichever way the document had centred it.
    box(
      width: ink.width,
      height: ink.height,
      place(center + horizon, block(width: column, body)),
    )
  } else {
    block(width: column, align(center, body))
  }
})

/// Whether an `html.elem` can be made here at all.
#let _html-here() = target() == "html" and text.lang != _drawing-lang

// `sole` says whether everything seen so far has been the only thing in its
// container. A cetz canvas or a fletcher diagram is a `context` and nothing
// else — that is the only way to recognise one, since `context` is not an
// element function that can be named — but a `context` in the middle of a
// paragraph is a counter reading a number, and a callout that numbers itself
// is prose, not a picture. Framing prose turns it into an image: unselectable,
// unannotatable, and re-wrapped to a width that is not the column's.
#let _draws(it, depth, sole: true) = {
  if depth <= 0 or type(it) != content { return false }
  if it.func() in _shapes { return true }
  if sole and repr(it.func()) == "context" { return true }
  let only = _sole-child(it)
  if only != none { return _draws(only, depth - 1, sole: sole) }
  // Across children, not only down a spine: a picture made by hand is a stack
  // of shapes or a heap of `place`d pieces, and either way there are several —
  // so none of them is the only thing in its container.
  let fields = it.fields()
  if "children" in fields and type(fields.children) == array {
    for child in fields.children.slice(0, calc.min(fields.children.len(), 32)) {
      if _draws(child, depth - 1, sole: false) { return true }
    }
  }
  false
}

// Whether a drawing has already been through one of these rules. Realization
// runs inwards-out, so by the time an outer block is asked about, the box
// inside it may already be a frame — and a frame put inside another frame is an
// element in a paged layout again, dropped along with everything it holds. The
// inner rule saw the drawing more closely, so the outer one stands down.
#let _made-html(it, depth) = {
  if depth <= 0 or type(it) != content { return false }
  // Neither is an element function that can be named, only recognised.
  if repr(it.func()) in ("frame", "elem") { return true }
  let fields = it.fields()
  for key in ("body", "child") {
    if key in fields and _made-html(fields.at(key), depth - 1) { return true }
  }
  if "children" in fields and type(fields.children) == array {
    for child in fields.children.slice(0, calc.min(fields.children.len(), 32)) {
      if _made-html(child, depth - 1) { return true }
    }
  }
  false
}

/// Whether this content is a drawing that still wants a frame around it.
#let _wants-frame(it, depth) = _draws(it, depth) and not _made-html(it, depth)

// -- the rules ---------------------------------------------------------------
//
// Every rule below asks the target first. Inside `html.frame` — where drawings
// are laid out the paged way — `html.elem` is not a thing that can be made,
// and a rule that reaches for one there loses the content it was given. The
// answer is only knowable in context, which is why each rule is one.

// `align` is dropped whole, title blocks and all. Its horizontal component is
// the part CSS can honour; the vertical one has no meaning in flow layout.
#let _text-align(a) = {
  let name = repr(a)
  if "center" in name { "center" } else if "right" in name { "right" } else if "left" in name {
    "left"
  } else { none }
}

#let _rule-align = it => context {
  if not _html-here() { return it }
  // The frame goes around the drawing, not around the container: a container
  // is often `width: 100%`, and a frame has no width for that to be a
  // percentage of. The div keeps the alignment; the frame keeps the drawing.
  if _wants-frame(it, 5) {
    // A framed drawing is a block-level element, which `text-align` cannot
    // move; a flex row can.
    let side = _text-align(it.alignment)
    let justify = if side == "center" {
      "center"
    } else if side == "right" { "flex-end" } else { "flex-start" }
    return html.elem(
      "div",
      attrs: (style: "display: flex; justify-content: " + justify),
      html.frame(it.body),
    )
  }
  // A centred title is sized against the text around the block it sits in,
  // not against the last paragraph before it.
  _in-base(_wrap("div", _style(_prop("text-align", _text-align(it.alignment))), it.body))
}

// A document's palette is chosen against a white page. Read in a dark theme,
// a near-white panel is a hole burnt in the page — so every fill is paired with
// a counterpart of the same hue for dark grounds, and CSS picks whichever the
// reader is actually in. Panels that are already dark are left alone: they were
// meant to be dark.
#let _dark-fill(v) = {
  let (l, a, b, ..) = oklab(v).components()
  // Mirrored, then pinned a little above the page's own ground, so a panel
  // still reads as raised rather than as a hole in the other direction.
  let dark = if l > 55% { 24% + (100% - l) * 0.4 } else { l }
  oklab(dark, a, b).to-hex()
}

// A background that follows the reader's theme, or a plain colour when the two
// would be the same.
#let _ground(v) = {
  let light = _color(v)
  if light == none { return none }
  let dark = _dark-fill(v)
  if dark == light { light } else { "light-dark(" + light + ", " + dark + ")" }
}

// Text on that ground: dark on the light version, light on the dark one.
#let _contrast(v) = {
  if type(v) != color { return none }
  let lightness = oklab(v).components().first()
  if lightness > 55% { "light-dark(#1a1a1a, #e8e8e8)" } else { "#f2f2f2" }
}

// `block` and `box` keep their content but lose every bit of their appearance,
// which is what makes a callout look like a callout.
// The space a block keeps around itself is spacing, which HTML export drops:
// two filled blocks in a row then share an edge and read as one box. Only what
// the block actually declares is used — `fields()` rather than the field, since
// a field that was never given is not knowable here — with a default for filled
// blocks, whose whole point is to be separate from what surrounds them.
#let _spacing(it) = {
  let fields = it.fields()
  let above = _len(fields.at("above", default: none))
  let below = _len(fields.at("below", default: none))
  let default = if fields.at("fill", default: none) != none { "0.9em" } else { none }
  // Parenthesised: in a code block a newline ends the statement, and a `+` at
  // the head of the next line is a unary plus on an array rather than a sum.
  (
    _prop("margin-top", if above == none { default } else { above })
      + _prop("margin-bottom", if below == none { default } else { below })
  )
}

#let _container-props(it) = (
  _prop("background", _ground(it.fill))
    + _prop("color", _contrast(it.fill))
    + _box-sides("padding", it.inset)
    + _box-sides("margin", it.outset)
    + _spacing(it)
    + _radius(it.radius)
    + _border(it.stroke)
    + _prop("width", _len(it.width))
)

#let _container-style(it) = _style(_container-props(it))

#let _rule-block = it => context {
  // A block is a place where the surrounding text size is settled, so it is
  // also a place to record it: a title inside one is sized against the block,
  // not against whatever paragraph came before.
  if not _html-here() { return it }
  // Recording it is not enough. A block set in another size — a raw block,
  // which Typst sets smaller than the text around it — makes every run inside
  // it agree with the new base and say nothing, and the block itself would
  // carry no size either: the page would show it at the size of the prose. So
  // the block says what the runs no longer have to.
  let outer = _base-size.get()
  let own = if outer == none or outer == 0pt { none } else {
    let ratio = text.size / outer
    if calc.abs(ratio - 1.0) < 0.03 { none } else {
      str(calc.round(ratio, digits: 3)) + "em"
    }
  }
  // A drawing inside an `align` is left to the align rule: it knows which way
  // to put the drawing, and a frame made here would take that decision away
  // and leave it flush left.
  let child = _sole-child(it)
  let aligned = child != none and child.func() == align
  if not aligned and _wants-frame(it, 6) { return _frame-drawing(it) }
  let style = _style(_container-props(it) + _prop("font-size", own))
  // Left alone, a block still becomes a `<div>`; only its appearance is lost,
  // and only that is worth a wrapper.
  _in-base(
    if style == "" { it } else { html.elem("div", attrs: (style: style), it.body) },
    carry: false,
  )
}

// A box is inline, and its outset paints outside the line without taking part
// in it: the chip behind a code fragment must not push the lines around it
// apart. Padding on an inline element does exactly that, and a negative margin
// of the same size keeps the width the text would have had — which is what an
// outset is for.
#let _outset(v) = {
  let out = ()
  for (side, value) in _sides(v) {
    let len = _len(value)
    if len == none { continue }
    out += _prop("padding-" + side, len)
    out += _prop("margin-" + side, "-" + len)
  }
  out
}

#let _rule-box = it => context {
  if not _html-here() { return it }
  if _wants-frame(it, 5) { return _frame-drawing(it) }
  // Inline, deliberately: `inline-block` would make the chip's own padding
  // grow the line box, and the paragraph would open up around every fragment
  // of code in it.
  let style = _style(
    ("display: inline",)
      + _prop("background", _ground(it.fill))
      + _prop("color", _contrast(it.fill))
      + _box-sides("padding", it.inset)
      + _outset(it.outset)
      + _radius(it.radius)
      + _border(it.stroke),
  )
  html.elem("span", attrs: (style: style), it.body)
}

// Text styling set on a run — a colour, a weight, a slant — is dropped whole,
// so a document that says something with colour says nothing in HTML. Only
// what differs from the default is emitted, or every run in the document would
// grow a span. Size is deliberately not among them: a `#set text(size: 10pt)`
// makes every run report 10pt, and there is no way to tell that from a run
// that asked for it.
// A run set in a monospaced face is saying something by it — a path, an
// identifier, a fragment of code — and the browser has its own monospace to
// say it with. Other families are left to the page: a document's serif is
// unlikely to be installed in the reader's browser, and every run would carry
// a family it cannot honour.
#let _mono-family(fonts) = {
  let names = if type(fonts) == array { fonts } else { (fonts,) }
  let names = names.map(f => if type(f) == str { f } else { f.at("name", default: "") })
  let mono = names.any(name => {
    let name = lower(name)
    ("mono", "code", "courier", "consolas", "menlo", "typewriter").any(hint => hint in name)
  })
  if mono { "ui-monospace, SFMono-Regular, Menlo, monospace" } else { none }
}

// The size the surrounding block is set in. A run that changes its size — a
// title, a piece of small print — has to say so as a *ratio*: CSS sizes are
// relative to the parent element, and an absolute `10pt` copied out of the
// document would fight the page's own typography and shrink every styled run
// to boot. The block's size is the only thing a run can be compared against,
// and only the block knows it, so it leaves it here on the way past.
#let _rule-par = it => _in-base(it)

#let _rule-heading = it => _in-base(it)

// A size worth mentioning: anything that is not the block's own, give or take
// rounding.
#let _size-ratio() = {
  let base = _base-size.get()
  if base == none or base == 0pt { return none }
  let ratio = text.size / base
  if calc.abs(ratio - 1.0) < 0.03 { return none }
  str(calc.round(ratio, digits: 3)) + "em"
}

// Read from the style chain rather than from the element: a run's colour and
// weight are set on it, and settable fields are only knowable in context.
#let _text-style() = _style(
  _prop("color", _ink(text.fill))
    + _prop("font-size", _size-ratio())
    + _prop("font-family", _mono-family(text.font))
    + (if text.weight == "bold" {
      ("font-weight: 700",)
    } else if type(text.weight) == int and text.weight != 400 {
      ("font-weight: " + str(text.weight),)
    } else { () })
    + (if text.style != "normal" { ("font-style: " + text.style,) } else { () }),
)

#let _rule-text = it => context {
  if not _html-here() { return it }
  let style = _text-style()
  if style == "" { it } else { html.elem("span", attrs: (style: style), it) }
}

// Math is emitted as MathML, whose elements take characters as children and
// not markup: a `<span>` inside an `<mi>` stops the browser treating it as
// math at all, and the equation falls back to body text. Resetting the text
// styling at the mouth of an equation makes the rule above find nothing to
// say, so nothing is wrapped. The browser's own math font takes it from there
// (see the stylesheet), which is the right answer anyway — the document's math
// font is a file on this machine, not something a page can assume.
#let _rule-math-equation = it => {
  set text(fill: black, weight: 400, style: "normal")
  it
}

// Vertical space is dropped; a div of that height says the same thing.
//
// Weak space is different: in a paged layout it is space that gives way to the
// space already there — the gap a heading leaves under itself does not add to
// the paragraph's own gap, it merges with it. A margin does exactly that in
// CSS, since the margins of an empty block collapse with each other and with
// its neighbours', so weak space is written as a margin and strong space as a
// height. Written as a height, the two gaps added up and every heading sat
// twice as far from its text in HTML as on paper.
#let _rule-v = it => context {
  if not _html-here() { return it }
  let amount = _len(it.amount)
  if amount == none { return none }
  let weak = it.fields().at("weak", default: false)
  html.elem(
    "div",
    attrs: (style: if weak { "margin-top: " + amount } else { "height: " + amount }),
    [],
  )
}

// A stroke's paint may be a gradient or a tiling, which `_color` declines; the
// rules above fall back to `currentColor` rather than dropping the border.

// A rule across the page.
#let _rule-line = it => context {
  if not _html-here() { return it }
  html.elem("hr", attrs: (style: _style(_prop("border-top", _one-stroke(it.stroke)))), [])
}

// `pad`, `place` and `stack` lose their children. Padding becomes CSS;
// placement cannot be honoured in flow, so the content is kept in place;
// a horizontal stack becomes a flex row, a vertical one plain flow.
#let _rule-pad = it => context if not _html-here() { it } else {
  _wrap(
    "div",
    _style(
      _prop("padding-left", _len(it.left))
        + _prop("padding-right", _len(it.right))
        + _prop("padding-top", _len(it.top))
        + _prop("padding-bottom", _len(it.bottom)),
    ),
    it.body,
  )
}

#let _rule-place = it => context if not _html-here() { it } else {
  _wrap("div", _style(_prop("text-align", _text-align(it.alignment))), it.body)
}

#let _rule-stack = it => context {
  if not _html-here() { return it }
  let sideways = it.dir == ltr or it.dir == rtl
  let gap = _len(it.spacing)
  _wrap(
    "div",
    if sideways {
      "display: flex; gap: " + if gap == none { "0" } else { gap }
    } else { "" },
    it.children.join(),
  )
}

// A grid is a table in all but name, and the browser has one of those. The
// children arrive as a flat run of cells, which the column count cuts into
// rows; a cell's own body is what survives, since `grid.cell` is dropped by
// the exporter exactly as `grid` is.
#let _cell-body(cell) = if "body" in cell.fields() { cell.body } else { cell }

#let _rule-grid = it => context {
  if not _html-here() { return it }
  let cells = it.children.filter(c => c.func() == grid.cell)
  let columns = if type(it.columns) == array { it.columns.len() } else { 1 }
  let columns = calc.max(columns, 1)
  html.elem(
    "table",
    attrs: (style: "border-collapse: collapse"),
    cells
      .chunks(columns)
      .map(row => html.elem(
        "tr",
        row.map(cell => html.elem("td", _cell-body(cell))).join(),
      ))
      .join(),
  )
}

// -- the rules, as data ------------------------------------------------------

// Installed by the server as the document's own default show rules, rather than
// applied by a file the document is compiled through. A wrapper meant the
// compile's main file was not the document, which every part of the system then
// had to know about: source ranges came from a file the reader has never seen,
// and everything downstream needed telling where the document really went.
//
// In file order, which is the order they were written in when they were show
// rules and the order they are applied in now.
#let rules = (
  (align, _rule-align),
  (block, _rule-block),
  (box, _rule-box),
  (par, _rule-par),
  (heading, _rule-heading),
  (text, _rule-text),
  (math.equation, _rule-math-equation),
  (v, _rule-v),
  (line, _rule-line),
  (pad, _rule-pad),
  (place, _rule-place),
  (stack, _rule-stack),
  (grid, _rule-grid),
)
