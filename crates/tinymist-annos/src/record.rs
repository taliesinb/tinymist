//! What an annotation is, apart from where it points.

use serde::{Deserialize, Serialize};

use crate::location::TypstLocation;

/// One annotation: something somebody said about a place in a document, and
/// everything that has happened to it since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// What was said afterwards, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discussion: Vec<Reply>,

    /// What the thing it points at looked like, when it could be pictured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<Capture>,
}

/// Something said in reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Who said it.
    pub author: String,
    /// When, ISO 8601 UTC.
    pub time: String,
    /// What they said.
    pub content: String,
}

/// A picture of what an annotation pointed at, at a moment.
///
/// Kept by hash rather than by value: the picture itself lives in the capture
/// store, and a hash is cheap enough to keep forever — which matters, because
/// the drawing an annotation was about may have been redrawn since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
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
    /// A reader's own marks over it, as inline SVG in the capture's
    /// coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup: Option<String>,
}

impl Annotation {
    /// Every anchor this annotation depends on.
    pub fn labels(&self) -> Vec<&str> {
        self.location.labels()
    }
}
