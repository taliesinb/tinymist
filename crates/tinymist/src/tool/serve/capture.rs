//! Captures: what a graphical annotation looked like when it was made.
//!
//! An agent asked about a plot has no way of seeing the plot. The document is
//! source, the annotation is a sentence about a picture, and the picture only
//! exists once something has laid it out. So the server keeps one: every time a
//! document compiles, the drawing each graphical annotation points at is taken
//! out of the rendered page as SVG, hashed, and stored under that hash. The
//! annotation records the hashes it has been through, in order, and an agent can
//! ask for one and be handed an image.
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



/// Puts the reader's own marks on top of a capture.
///
/// Markup is inline SVG in the capture's own coordinates — a circle around the
/// outlier, an arrow at the label — so it goes in just before the drawing ends,
/// which is what "on top" means in SVG.
pub fn with_markup(svg: &str, markup: &str) -> String {
    match svg.rfind("</svg>") {
        Some(at) => format!("{}{markup}{}", &svg[..at], &svg[at..]),
        None => svg.to_owned(),
    }
}

/// Draws a reader's marks over a capture that is already a picture.
///
/// A capture the page rasterised is PNG, so the marks cannot be put inside it
/// the way they are put inside an SVG. They are rendered over it instead, at
/// the size the picture was taken at, which is the size their coordinates are
/// in.
pub fn png_with_markup(png: &[u8], markup: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = resvg::tiny_skia::Pixmap::decode_png(png)
        .map_err(|err| format!("cannot read the capture as PNG: {err}"))?;
    let (w, h) = (width.max(1), height.max(1));
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\">{markup}</svg>"
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
