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

use crate::tool::render::html::{doc_file, framed_drawings, FramedDrawing};

use super::annotations::{
    read_sidecar, sidecar_path, AnnotationRecord, Scope, find_anchors,
};
/// One annotation, as the HTML client needs it: everything the sidecar holds,
/// plus where its anchors sit in the source. The client finds the element that
/// covers that offset; the server does not need to know how it is drawn.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlPin {
    /// The annotation kind.
    #[serde(rename = "type")]
    pub rtype: String,
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
    /// What the annotation refers to.
    pub scope: String,
    /// The colour it was made in, as `#rrggbb`, or empty when it predates the
    /// field: the client falls back to its palette then.
    pub color: String,
    /// The byte offset of the anchor in the source.
    pub start: usize,
    /// The byte offset the annotation reaches to: the end anchor of a span, or
    /// the start again for everything else.
    pub end: usize,
}









/// Every annotation with the source offsets of its anchors.
pub fn html_pins(art: &LspCompiledArtifact) -> Vec<HtmlPin> {
    let Some(sidecar) = sidecar_path(art) else {
        return vec![];
    };
    let (records, _) = read_sidecar(&sidecar);
    if records.is_empty() {
        return vec![];
    }
    let world = art.world();
    let Ok(source) = world.source(doc_file(world)) else {
        return vec![];
    };
    let text = source.text();

    records
        .iter()
        .filter_map(|rec| pin_for(rec, text))
        .collect()
}

fn pin_for(rec: &AnnotationRecord, text: &str) -> Option<HtmlPin> {
    let anchors = find_anchors(text, &rec.uuid);
    let (at, scope, label) = anchors.first().cloned()?;
    // A span's text lies *between* its two labels, so it starts past the first
    // one: the label itself is not part of what was annotated, and a range that
    // includes it reaches back into whatever came before.
    let (start, end) = match scope {
        Scope::Span => (
            at + label.len(),
            anchors
                .iter()
                .rev()
                .find(|(off, _, l)| *off > at && l.ends_with(".end>"))
                .map(|(off, _, _)| *off)
                .unwrap_or(at + label.len()),
        ),
        // Inline content is found by the offset *past* it, which is where the
        // text after it begins — and the label sits in between, so the anchor
        // is past the label as well. Everything else is anchored at the label.
        Scope::Inline => (at + label.len(), at + label.len()),
        // A sentence is a range too, and only the source knows where it starts
        // and ends: the browser sees a paragraph of text with no sentences in
        // it. Sent as a range, and drawn like a span.
        Scope::Sentence => {
            let range = super::annotations::sentence_range(text, at);
            (range.start, range.end)
        }
        _ => (at, at),
    };
    Some(HtmlPin {
        rtype: rec.rtype.clone(),
        uuid: rec.uuid.clone(),
        letter: rec.letter.clone(),
        author: rec.author.clone(),
        content: rec.content.clone(),
        time: rec.time.clone(),
        claimed: rec.claimed,
        resolved: rec.resolved,
        mtime: rec.changed().to_owned(),
        discussion: rec.discussion.clone(),
        scope: scope.as_str().into(),
        color: rec.color.clone(),
        start,
        end,
    })
}






/// The scopes that name a picture rather than text. Only these are captured:
/// everything else is in the source already, where an agent can read it.
fn graphical(scope: &str) -> bool {
    matches!(scope, "svg" | "math.block")
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
    let pins: Vec<_> = html_pins(art)
        .into_iter()
        .filter(|pin| graphical(&pin.scope))
        .collect();
    if pins.is_empty() {
        return;
    }
    let drawings = framed_drawings(body);
    if drawings.is_empty() {
        return;
    }
    for pin in pins {
        let Some(drawing) = drawing_at(&drawings, pin.start) else {
            continue;
        };
        // Stored first, and every time: the sidecar outlives the cache, so a
        // server that has just started is holding hashes for files it does not
        // have. Writing an unchanged drawing back under the hash it already has
        // is what makes those readable again — and costs nothing when the file
        // is there.
        let Some(hash) = capture::store(drawing.svg.as_bytes(), "svg") else {
            continue;
        };
        let seen = super::annotations::last_capture(art, &pin.uuid);
        if seen.as_deref() == Some(hash.as_str()) {
            continue;
        }
        let (width, height) = capture::pixel_size(&drawing.svg).unwrap_or((0, 0));
        let entry = super::annotations::AnnotationCapture {
            time: super::annotations::iso_now(),
            fmt: "svg".into(),
            hash,
            width,
            height,
            markup: None,
        };
        if let Err(err) = super::annotations::add_capture(art, &pin.uuid, &entry) {
            log::warn!("cannot record a capture of {}: {err}", pin.uuid);
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
