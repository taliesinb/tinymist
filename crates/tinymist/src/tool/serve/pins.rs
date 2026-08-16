//! Where the annotations land on a rendered document, and what the drawings
//! looked like when they did.
//!
//! The document itself is rendered by `tool/render`, which knows nothing about
//! annotations: it labels every element with the source range it came from, and
//! the client finds the element covering an anchor's offset. This is the half
//! that knows what an anchor is — which offsets to send, and which drawings to
//! keep a picture of.

use tinymist_project::LspCompiledArtifact;
use typst::World;

use serde::Serialize;

use crate::tool::render::html::{framed_drawings, FramedDrawing};

use super::annotations::{read_sidecar, sidecar_path, AnnotationRecord};
use tinymist_annos::location::HtmlLocation;

/// One annotation, as the page needs it: what it says, and where to draw it.
///
/// The location is expressed against the rendering the page is showing, so the
/// client never sees a byte offset into a `.typ` file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlPin {
    /// The annotation kind: a comment, a question, a request.
    #[serde(rename = "type")]
    pub kind: String,
    /// The unique id.
    pub uuid: String,
    /// The display letter.
    pub letter: String,
    /// The author.
    pub author: String,
    /// The message.
    pub content: String,
    /// Creation time, ISO 8601 UTC.
    pub time: String,
    /// Whether somebody is working on it.
    pub claimed: bool,
    /// Whether it is done.
    pub resolved: bool,
    /// When it last changed, ISO 8601 UTC.
    pub mtime: String,
    /// The discussion thread.
    pub discussion: Vec<super::annotations::AnnotationReply>,
    /// The colour it was made in, as `#rrggbb`.
    pub color: String,
    /// Where to draw it, or nothing when its anchor is no longer in the
    /// document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<HtmlLocation>,
    /// What was there when the annotation was made, for saying what a lost
    /// annotation was about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

/// Every annotation of a document, placed on the rendering a page is showing.
pub fn html_pins(art: &LspCompiledArtifact) -> Vec<HtmlPin> {
    let Some(sidecar) = sidecar_path(art) else {
        return vec![];
    };
    let records = read_sidecar(&sidecar).annotations;
    if records.is_empty() {
        return vec![];
    }
    let world = art.world();
    let Ok(source) = world.source(world.main()) else {
        return vec![];
    };
    let Some(map) = super::renders::latest() else {
        return vec![];
    };
    let text = source.text().to_owned();
    let ctx = tinymist_annos::resolve::Context {
        map: &map,
        was: &text,
        source: &source,
        seed: 0,
    };
    records.iter().map(|rec| pin_for(rec, &ctx)).collect()
}

fn pin_for(rec: &AnnotationRecord, ctx: &tinymist_annos::resolve::Context) -> HtmlPin {
    HtmlPin {
        kind: rec.kind.clone(),
        uuid: rec.uuid.clone(),
        letter: rec.letter.clone(),
        author: rec.author.clone(),
        content: rec.content.clone(),
        time: rec.time.clone(),
        claimed: rec.claimed,
        resolved: rec.resolved,
        mtime: rec.mtime.clone(),
        discussion: rec.discussion.clone(),
        color: rec.color.clone(),
        location: tinymist_annos::resolve::project(ctx, &rec.location).ok(),
        snapshot: rec.snapshot.clone(),
    }
}

/// The locations that name a picture rather than text. Only these are captured:
/// everything else is in the source already, where an agent can read it.
fn graphical(location: &tinymist_annos::TypstLocation) -> bool {
    matches!(location.kind(), "svg" | "math.block")
}

/// The drawing an anchor names, if one of them is.
///
/// An anchor for a drawing sits just after it — `})<anno.G001.svg>` — so the
/// enclosing range ends where the anchor begins rather than containing it, and
/// the smallest range that reaches the anchor is the drawing's own container
/// rather than the section it is in.
fn drawing_at(drawings: &[FramedDrawing], at: usize) -> Option<&FramedDrawing> {
    const SLACK: usize = 8;
    drawings
        .iter()
        .filter(|drawing| drawing.range.start <= at && at <= drawing.range.end + SLACK)
        .min_by_key(|drawing| drawing.range.end.saturating_sub(drawing.range.start))
}

/// Records what each graphical annotation's drawing looks like now.
///
/// Called once per compile, with the body that compile produced. A drawing that
/// hashes to what the annotation last saw is not recorded again: the list is a
/// history of how the picture changed, not of how often the document was built.
pub fn record_captures(art: &LspCompiledArtifact, body: &str) {
    use crate::tool::serve::capture;
    let Some(sidecar_path) = sidecar_path(art) else {
        return;
    };
    let records: Vec<AnnotationRecord> = read_sidecar(&sidecar_path)
        .annotations
        .into_iter()
        .filter(|rec| graphical(&rec.location))
        .collect();
    if records.is_empty() {
        return;
    }
    let drawings = framed_drawings(body);
    if drawings.is_empty() {
        return;
    }
    let world = art.world();
    let Ok(source) = world.source(world.main()) else {
        return;
    };

    for rec in records {
        // Where the annotation's anchor sits, which is just after the drawing.
        let Some(label) = rec.labels().first().map(|label| label.to_string()) else {
            continue;
        };
        let id = tinymist_annos::anchor_id(&label).unwrap_or(&label);
        let Some(anchor) = tinymist_annos::anchor::find(&source, id) else {
            continue;
        };
        let Some(drawing) = drawing_at(&drawings, anchor.at()) else {
            continue;
        };
        // Stored first, and every time: the sidecar outlives the cache, so a
        // server that has just started is holding hashes for files it does not
        // have. Writing an unchanged drawing back under the hash it already has
        // is what makes those readable again, and costs nothing when the file
        // is there.
        let Some(hash) = capture::store(drawing.svg.as_bytes(), "svg") else {
            continue;
        };
        if rec.captures.last().map(|last| last.hash.as_str()) == Some(hash.as_str()) {
            continue;
        }
        let (width, height) = capture::pixel_size(&drawing.svg).unwrap_or((0, 0));
        let entry = super::annotations::AnnotationCapture {
            time: tinymist_project::iso_now(),
            fmt: "svg".into(),
            hash,
            width,
            height,
            markup: None,
        };
        let uuid = rec.uuid.clone();
        let written = super::annotations::revise(&sidecar_path, |sidecar| {
            let record = sidecar
                .find_mut(&uuid)
                .ok_or_else(|| format!("no annotation {uuid}"))?;
            record.captures.push(entry.clone());
            Ok(())
        });
        if let Err(err) = written {
            log::warn!("cannot record a capture of {}: {err}", rec.uuid);
        }
    }
}

#[cfg(test)]
mod capture_tests {
    use super::{drawing_at, framed_drawings};

    /// A rendered figure, in the shape the exporter writes: the document's own
    /// elements carry ranges, the frame inside them carries none, and the
    /// drawing is an SVG that belongs to whatever last opened around it.
    const BODY: &str = concat!(
        r#"<p data-typst-src="0:10">before</p>"#,
        r#"<figure data-typst-src="20:120"><div style="x">"#,
        r#"<svg viewBox="0 0 374 110" width="374pt" height="110pt"><g><svg><circle/></svg></g></svg>"#,
        r#"</div><figcaption data-typst-src="60:100">a caption</figcaption></figure>"#,
        r#"<p data-typst-src="200:210">after</p>"#,
    );

    #[test]
    fn finds_a_drawing_and_what_it_belongs_to() {
        let found = framed_drawings(BODY);
        assert_eq!(found.len(), 1, "one drawing, not its nested SVG");
        assert_eq!(found[0].range, 20..120);
        // Taken whole, closing SVG included, nesting counted.
        assert!(found[0].svg.starts_with("<svg viewBox"));
        assert!(found[0].svg.ends_with("</svg>"));
        assert_eq!(found[0].svg.matches("</svg>").count(), 2);
    }

    #[test]
    fn an_anchor_just_after_a_drawing_still_names_it() {
        let found = framed_drawings(BODY);
        // The anchor sits past the end of what it points at, as `})<anno…>`
        // does in the source.
        assert!(drawing_at(&found, 121).is_some());
        assert!(drawing_at(&found, 205).is_none());
    }

    #[test]
    fn reads_the_size_it_has_on_the_page() {
        let found = framed_drawings(BODY);
        let size = crate::tool::serve::capture::pixel_size(&found[0].svg);
        // 374pt at 4/3, which is what a browser lays it out at.
        assert_eq!(size, Some((499, 147)));
    }
}
