//! Captures: what a graphical annotation looked like when it was made.
//!
//! An agent asked about a plot has no way of seeing the plot. The document is
//! source, the annotation is a sentence about a picture, and the picture only
//! exists once something has laid it out. So the server keeps one: every time a
//! document compiles, the drawing each graphical annotation points at is taken
//! out of the rendered page as SVG, hashed, and stored under that hash. A page
//! adds to the same list when a reader draws on something the compile cannot
//! reach — an equation, a table, a figure of several things — by taking its own
//! picture of it. The annotation records the captures it has been through, in
//! order, and an agent can ask for one and be handed an image.
//!
//! What was drawn on a capture is not held here. A scribble belongs to the
//! remark it was drawn with and names the capture it is on, so this module is
//! given the shapes when it is asked for a picture and paints them then.
//!
//! Kept next to the server registry rather than in the per-process render
//! cache, and so outliving both the server and the drawing: an annotation made
//! in March about a plot that has since been redrawn still shows the plot it was
//! about. The sidecar keeps only the hash, and hashes are cheap to keep around —
//! what is expensive is the picture that no longer exists anywhere.
//!
//! Rasterising happens on request rather than on compile: most captures are
//! never looked at, and the one that is wants whatever size the asker needs.

use std::path::PathBuf;

pub use crate::tool::render::html::{hash_of, pixel_size};

/// Where captures are kept: beside the registry, under the same state
/// directory, so they survive a restart.
pub fn captures_dir() -> PathBuf {
    crate::tool::registry::registry_dir()
        .parent()
        .map(|base| base.join("captures"))
        .unwrap_or_else(|| std::env::temp_dir().join("talimist").join("captures"))
}


/// Where a capture is kept, if this process keeps captures at all. The format
/// is part of the name: a capture says what it is, and that is how to read it.
pub fn path_of(hash: &str, fmt: &str) -> Option<PathBuf> {
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if fmt.is_empty() || !fmt.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(captures_dir().join(format!("{hash}.{fmt}")))
}

/// A drawing without the rendering's bookkeeping on it.
///
/// The exporter labels every element with an id and the source range it came
/// from, and those change whenever anything above them in the document does. A
/// picture is what it looks like, so a drawing that is the same drawing should
/// hash to the same bytes however many times the document has been rebuilt
/// around it — otherwise every edit anywhere leaves another copy of every
/// drawing in the store and another entry in every annotation.
pub fn plain_svg(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find(" data-") {
        let after = &rest[at + " data-".len()..];
        let named = after
            .split_once('=')
            .filter(|(name, _)| matches!(*name, "uid" | "typst-src" | "typst-text" | "typst-atom"));
        let Some((_, value)) = named else {
            out.push_str(&rest[..at + " data-".len()]);
            rest = after;
            continue;
        };
        // The value is quoted, and an attribute value cannot contain the quote
        // that opened it.
        let quote = value.chars().next().unwrap_or('"');
        let Some(end) = value[1..].find(quote) else {
            break;
        };
        out.push_str(&rest[..at]);
        rest = &value[1 + end + 1..];
    }
    out.push_str(rest);
    out
}

/// Stores a capture, returning its hash. Writing is skipped when the file is
/// already there: the same drawing compiles to the same bytes, and most
/// compiles change nothing about it.
pub fn store(bytes: &[u8], fmt: &str) -> Option<String> {
    let hash = hash_of(bytes);
    let path = path_of(&hash, fmt)?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        std::fs::write(&path, bytes).ok()?;
    }
    Some(hash)
}

/// Reads a stored capture back.
pub fn read(hash: &str, fmt: &str) -> Option<Vec<u8>> {
    std::fs::read(path_of(hash, fmt)?).ok()
}



/// The reader's marks, as SVG to paint over a capture.
///
/// The file holds shapes and nothing else, so everything about how a mark looks
/// is decided here: round joins and caps, because a pen has them, and a clip to
/// the picture, because a mark that ran past the edge of what it was drawn on
/// is about the picture only as far as the picture goes.
pub fn marks_svg(marks: &[tinymist_annos::Mark], width: u32, height: u32) -> String {
    use tinymist_annos::Mark;
    let mut out = String::new();
    for mark in marks {
        let color = escape(mark.color());
        match mark {
            Mark::Path { .. } => {
                let points = mark.points();
                if points.len() < 2 {
                    continue;
                }
                let path = curve_through(&points);
                out.push_str(&format!(
                    "<path d=\"{path}\" fill=\"none\" stroke=\"{color}\" \
                     stroke-width=\"{:.2}\" stroke-linecap=\"round\" \
                     stroke-linejoin=\"round\"/>",
                    mark.width(),
                ));
            }
            // A cross, which is what a hand makes when it means "here": a dot
            // is easily read as part of the picture, and two strokes are not.
            Mark::Point { x, y, width, .. } => {
                let reach = width;
                out.push_str(&format!(
                    "<path d=\"M{:.1} {:.1} L{:.1} {:.1} M{:.1} {:.1} L{:.1} {:.1}\" \
                     fill=\"none\" stroke=\"{color}\" stroke-width=\"{:.2}\" \
                     stroke-linecap=\"round\"/>",
                    x - reach,
                    y - reach,
                    x + reach,
                    y + reach,
                    x - reach,
                    y + reach,
                    x + reach,
                    y - reach,
                    width * 0.55,
                ));
            }
        }
    }
    if out.is_empty() {
        return out;
    }
    format!(
        "<clipPath id=\"tm-marks\"><rect x=\"0\" y=\"0\" width=\"{width}\" \
         height=\"{height}\"/></clipPath><g clip-path=\"url(#tm-marks)\">{out}</g>"
    )
}

/// A curve through the points a stroke was drawn through.
///
/// Straight segments between them show every place the hand changed direction
/// and every place the subsampling dropped a report, which is a shape nobody
/// drew. A Catmull-Rom spline passes through the points and is a cubic Bézier
/// in disguise, so it is written as one: each segment takes its handles from
/// the neighbours on either side, a sixth of the way along.
fn curve_through(points: &[(f32, f32)]) -> String {
    let at = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    let (x, y) = points[0];
    let mut path = format!("M{x:.1} {y:.1}");
    if points.len() < 3 {
        for (x, y) in &points[1..] {
            path.push_str(&format!(" L{x:.1} {y:.1}"));
        }
        return path;
    }
    for i in 0..points.len() as isize - 1 {
        let (before, from, to, after) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let c1 = (from.0 + (to.0 - before.0) / 6.0, from.1 + (to.1 - before.1) / 6.0);
        let c2 = (to.0 - (after.0 - from.0) / 6.0, to.1 - (after.1 - from.1) / 6.0);
        path.push_str(&format!(
            " C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1}",
            c1.0, c1.1, c2.0, c2.1, to.0, to.1
        ));
    }
    path
}

/// What a colour may contain, so that a value from a page cannot close the
/// attribute it is written into.
fn escape(color: &str) -> String {
    color
        .chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '#' | '(' | ')' | ',' | '.' | '%' | ' ')
        })
        .collect()
}

/// Puts a scribble's marks on top of a capture that is SVG.
///
/// They are in the drawing's own coordinates, so they go in just before the
/// drawing ends, which is what "on top" means in SVG.
pub fn with_marks(
    svg: &str,
    marks: &[tinymist_annos::Mark],
    width: u32,
    height: u32,
) -> String {
    let drawn = marks_svg(marks, width, height);
    if drawn.is_empty() {
        return svg.to_owned();
    }
    match svg.rfind("</svg>") {
        Some(at) => format!("{}{drawn}{}", &svg[..at], &svg[at..]),
        None => svg.to_owned(),
    }
}

/// Draws a scribble's marks over a capture that is already a picture.
///
/// A capture the page rasterised is PNG, so the marks cannot be put inside it
/// the way they are put inside an SVG. They are rendered over it instead, at
/// the size the picture was taken at, which is the size their coordinates are
/// in.
pub fn png_with_marks(
    png: &[u8],
    marks: &[tinymist_annos::Mark],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut pixmap = resvg::tiny_skia::Pixmap::decode_png(png)
        .map_err(|err| format!("cannot read the capture as PNG: {err}"))?;
    let (w, h) = (width.max(1), height.max(1));
    let drawn = marks_svg(marks, w, h);
    if drawn.is_empty() {
        return pixmap
            .encode_png()
            .map_err(|err| format!("cannot encode the capture as PNG: {err}"));
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\">{drawn}</svg>"
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|err| format!("cannot read the marks as SVG: {err}"))?;
    // The picture was rasterised at whatever the reader's screen does, so it
    // is not the size the marks are measured in; the marks are scaled to it.
    let scale = resvg::tiny_skia::Transform::from_scale(
        pixmap.width() as f32 / w as f32,
        pixmap.height() as f32 / h as f32,
    );
    resvg::render(&tree, scale, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|err| format!("cannot encode the capture as PNG: {err}"))
}

/// How big a capture is rasterised, by default: half of its natural size,
/// unless that is still over the ceiling below.
const AUTO_SCALE: f32 = 0.5;

/// The longest a rasterised capture's long edge gets on its own. The size is a
/// budget question rather than a fidelity one: an image handed to an agent is
/// counted in tokens, and a diagram that reads perfectly at 1000px costs four
/// times as much at 2000. An asker who needs the fine print can say so.
const AUTO_LONG_EDGE: f32 = 1000.0;

/// Renders a capture to PNG.
///
/// `scale` is a multiple of the size the drawing has *on the page*, so that
/// asking for 1 gets what the reader sees rather than what the file happens to
/// be measured in. `None` means auto — half of that, or smaller when half would
/// still be over the long-edge ceiling.
pub fn png(svg: &str, scale: Option<f32>) -> Result<Vec<u8>, String> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(svg, &options)
        .map_err(|err| format!("cannot read the capture as SVG: {err}"))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if w <= 0.0 || h <= 0.0 {
        return Err("the capture has no size".into());
    }
    // What the drawing measures on the page, against what it measures in its
    // own units: a frame is written in points and shown at 4/3 of that, and a
    // size asked for in pixels means the ones the reader is looking at.
    let on_page = pixel_size(svg)
        .filter(|(width, _)| *width > 0)
        .map(|(width, _)| width as f32 / w)
        .unwrap_or(1.0);
    let scale = match scale {
        // An asked-for size is taken at its word, up to what is sane.
        Some(asked) => on_page * asked.clamp(0.05, 8.0),
        // Left to itself: half of the page size, and less than that when half
        // would still be over the ceiling.
        None => (on_page * AUTO_SCALE).min(AUTO_LONG_EDGE / w.max(h)),
    };
    let width = ((w * scale).ceil() as u32).max(1);
    let height = ((h * scale).ceil() as u32).max(1);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| "cannot make a canvas that size".to_owned())?;
    // A drawing is made to be read on a page, and a page is white; left
    // transparent it would arrive as dark-on-dark wherever it is shown.
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let transform = resvg::tiny_skia::Transform::from_scale(width as f32 / w, height as f32 / h);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|err| format!("cannot encode the capture as PNG: {err}"))
}
