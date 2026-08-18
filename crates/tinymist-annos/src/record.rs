//! What an annotation is, apart from where it points.

use serde::{Deserialize, Serialize};

use crate::location::TypstLocation;

/// One annotation: something somebody said about a place in a document, and
/// everything that has happened to it since.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// What this annotation is, for as long as it exists.
    ///
    /// Never written into the document — the document carries anchors, and an
    /// anchor may be shared by several annotations — so it is free to be long
    /// enough that nothing has to check for collisions.
    pub uuid: String,

    /// The short name a reader sees: `a`, `b`, `c`. Assigned in reading order
    /// and reassigned as the document changes, so it says where an annotation
    /// is rather than when it was made.
    pub letter: String,

    /// Where it points.
    pub location: TypstLocation,

    /// What was there when the annotation was made.
    ///
    /// An annotation whose anchor has been deleted can otherwise only report
    /// its own absence. With this it can say what it was about, which is the
    /// difference between something a reader can act on and something they can
    /// only be puzzled by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,

    /// What kind of remark it is: a comment, a question, a request.
    #[serde(rename = "type")]
    pub kind: String,

    /// The colour it is drawn in.
    pub color: String,

    /// Who made it.
    pub author: String,

    /// When, ISO 8601 UTC.
    pub time: String,

    /// When it last changed — a reply, a flag, an edit. The same as `time`
    /// for an annotation nothing has happened to.
    pub mtime: String,

    /// Whether somebody has taken it on.
    #[serde(default)]
    pub claimed: bool,

    /// Whether it is finished with.
    #[serde(default)]
    pub resolved: bool,

    /// What it says.
    pub content: String,

    /// What was drawn with it, if anything was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scribble: Option<Scribble>,

    /// What was said afterwards, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion: Vec<Reply>,

    /// What the thing it points at looked like, when it could be pictured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<Capture>,
}

/// Something said in reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// Who said it.
    pub author: String,
    /// When, ISO 8601 UTC.
    pub time: String,
    /// What they said.
    pub content: String,
    /// What they drew while saying it, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scribble: Option<Scribble>,
}

/// A picture of what an annotation pointed at, at a moment.
///
/// Kept by hash rather than by value: the picture itself lives in the capture
/// store, and a hash is cheap enough to keep forever — which matters, because
/// the drawing an annotation was about may have been redrawn since.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    /// What a scribble drawn on it refers to. Written since captures had
    /// scribbles to refer to them by; one from before that is named by its
    /// hash, which is what it was identified by then.
    #[serde(default)]
    pub id: String,
    /// When it was taken, ISO 8601 UTC.
    pub time: String,
    /// What it is: `svg`, `png`.
    pub fmt: String,
    /// Where to find it in the capture store.
    pub hash: String,
    /// How wide it is on the page, in CSS pixels.
    pub width: u32,
    /// How tall.
    pub height: u32,
}

/// A drawing somebody made on a picture, as part of what they were saying.
///
/// Drawing on a picture is a way of saying something about it, so a scribble
/// belongs to the remark it was drawn with — the annotation itself, or a reply
/// — and takes its author and its time from there. What it was drawn on is
/// named rather than held: the picture is a capture of the annotation, and the
/// same picture may be scribbled on more than once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scribble {
    /// Its own name, made by whoever drew it.
    pub id: String,
    /// The capture it is drawn on top of, by that capture's id.
    pub capture: String,
    /// What was drawn, in that capture's coordinates.
    pub shapes: Vec<Mark>,
}

/// Something the reader drew on a capture.
///
/// A tagged shape rather than a picture: the points are the drawing, and how it
/// is painted — the joins, the caps, the clip to the picture's edge — is
/// decided when the capture is composited. A shape built into the file would be
/// that decision taken early and stored a thousand times, and a kind of mark
/// added later would have nowhere to go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Mark {
    /// A stroke of the pen.
    Path {
        /// The points, x and y alternating: little-endian `f32`, base64. A
        /// stroke is a few hundred numbers, and numbers written as text are
        /// four times the size and no more readable at that length.
        coords: String,
        /// How thick, in the capture's coordinates.
        width: f32,
        /// What colour, as `#rrggbb`.
        color: String,
    },
    /// A place, from a click that never became a stroke.
    Point {
        /// Where, in the capture's coordinates.
        x: f32,
        /// Likewise.
        y: f32,
        /// How big, in the capture's coordinates: the width the pen would have
        /// drawn with.
        width: f32,
        /// What colour, as `#rrggbb`.
        color: String,
    },
}

impl Mark {
    /// A stroke from the points it was drawn through.
    pub fn path(points: &[(f32, f32)], width: f32, color: &str) -> Self {
        use base64::Engine as _;
        let mut bytes = Vec::with_capacity(points.len() * 8);
        for (x, y) in points {
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        Self::Path {
            coords: base64::engine::general_purpose::STANDARD.encode(&bytes),
            width,
            color: color.to_owned(),
        }
    }

    /// The points of a stroke, as they were drawn. Empty for anything else.
    pub fn points(&self) -> Vec<(f32, f32)> {
        use base64::Engine as _;
        let Self::Path { coords, .. } = self else {
            return vec![];
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(coords) else {
            return vec![];
        };
        bytes
            .chunks_exact(8)
            .map(|pair| {
                let read = |at: usize| {
                    f32::from_le_bytes([pair[at], pair[at + 1], pair[at + 2], pair[at + 3]])
                };
                (read(0), read(4))
            })
            .collect()
    }

    /// How thick it is.
    pub fn width(&self) -> f32 {
        match self {
            Self::Path { width, .. } | Self::Point { width, .. } => *width,
        }
    }

    /// What colour it is.
    pub fn color(&self) -> &str {
        match self {
            Self::Path { color, .. } | Self::Point { color, .. } => color,
        }
    }
}

impl Capture {
    /// What a scribble names it by: its own id, or its hash for one written
    /// before captures had ids.
    pub fn name(&self) -> &str {
        if self.id.is_empty() { &self.hash } else { &self.id }
    }
}

impl Annotation {
    /// Every anchor this annotation depends on.
    pub fn labels(&self) -> Vec<&str> {
        self.location.labels()
    }
}

#[cfg(test)]
mod tests {
    use super::Mark;

    /// The points go out and come back as they were, and a mark that is not a
    /// stroke has none.
    #[test]
    fn a_stroke_survives_being_written_down() {
        let drawn = [(0.0, 0.0), (12.5, -3.25), (90.0, 75.5)];
        let mark = Mark::path(&drawn, 5.0, "#7bd88f");
        assert_eq!(mark.points(), drawn);
        assert_eq!(mark.width(), 5.0);
        assert_eq!(mark.color(), "#7bd88f");

        let json = serde_json::to_string(&mark).expect("a mark is JSON");
        assert!(json.contains("\"type\":\"path\""), "{json}");
        let back: Mark = serde_json::from_str(&json).expect("and back again");
        assert_eq!(back.points(), drawn);

        let place = Mark::Point { x: 3.0, y: 4.0, width: 5.0, color: "#e8442f".into() };
        assert!(place.points().is_empty());
        let json = serde_json::to_string(&place).expect("a point is JSON");
        assert!(json.contains("\"type\":\"point\""), "{json}");
    }
}
