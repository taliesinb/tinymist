//! Where an annotation points.
//!
//! The same shapes are spoken by both ends of the system, and only the way a
//! place is *named* differs. A browser knows the rendering: it can name a node
//! the renderer gave an id to, and a position among the characters inside one.
//! The server knows the source: it can name a Typst label, and which side of it
//! a position lies on. Everything else — that a span has two ends, that a word
//! is a word — is the same on both sides.
//!
//! So the location types are written once, over a [`Flavor`] that says what a
//! reference looks like. Resolving a location is then a matter of rewriting its
//! references and copying the rest, and a new kind of location has one
//! definition rather than two that must be kept in step.

use serde::{Deserialize, Serialize};

/// What a reference looks like, on one side of the wire.
///
/// The four kinds are not interchangeable: a position between characters is a
/// different thing from a whole element, and a location says which it wants.
pub trait Flavor {
    /// A whole element: an equation, a drawing, a paragraph.
    type Node: Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug + PartialEq;
    /// A vertical position, at the top or bottom edge of an element.
    type NodeCursor: Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug + PartialEq;
    /// A word, which is a stretch of text short enough to be named by it.
    type Word: Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug + PartialEq;
    /// A horizontal position, between two characters.
    type TextCursor: Serialize + serde::de::DeserializeOwned + Clone + std::fmt::Debug + PartialEq;
}

/// References as a browser can make them, against one rendering of a document.
///
/// A rendering is a passing thing — the next compile makes another — so these
/// are only meaningful together with the render they were taken from, and the
/// server keeps the map that turns them back into places in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Html;

/// References as the document itself carries them: Typst labels, which move
/// with the text they are written beside and so outlive every rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typst;

impl Flavor for Html {
    type Node = HtmlNodeRef;
    type NodeCursor = HtmlNodeCursorRef;
    type Word = HtmlWordRef;
    type TextCursor = HtmlTextCursorRef;
}

impl Flavor for Typst {
    type Node = TypstNodeRef;
    type NodeCursor = TypstNodeCursorRef;
    type Word = TypstWordRef;
    type TextCursor = TypstTextCursorRef;
}

/// Which end of a thing a horizontal position sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HSide {
    /// Before it.
    Left,
    /// After it.
    Right,
}

/// Which edge of a block a vertical position sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VSide {
    /// Above it.
    Top,
    /// Below it.
    Bottom,
}

/// An element in a rendering, by the id the renderer gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "node")]
pub struct HtmlNodeRef {
    /// The id, as it appears in the rendered HTML.
    #[serde(rename = "ref")]
    pub node: String,
}

/// The top or bottom edge of an element in a rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "node_cursor")]
pub struct HtmlNodeCursorRef {
    /// The id, as it appears in the rendered HTML.
    #[serde(rename = "ref")]
    pub node: String,
    /// Which edge.
    pub side: VSide,
}

/// A word in a rendering: a stretch of one text run, with the word itself kept
/// alongside so a stale reference can be recognised as stale rather than
/// silently resolving to whatever now occupies those characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "word")]
pub struct HtmlWordRef {
    /// The id of the run the word is in.
    #[serde(rename = "ref")]
    pub node: String,
    /// Where the word starts, in characters from the start of the run.
    pub beg: usize,
    /// Where it ends.
    pub end: usize,
    /// The word, for checking against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<String>,
}

/// A position between two characters of a rendering.
///
/// The text on either side rides along for the same reason a word carries
/// itself: an edit that moved this position can be recognised, and a small
/// misalignment repaired, by looking for the text that used to be here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "text_cursor")]
pub struct HtmlTextCursorRef {
    /// The id of the run the position is in.
    #[serde(rename = "ref")]
    pub node: String,
    /// How many characters into the run it sits.
    pub pos: usize,
    /// A little of the text to the left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub l: Option<String>,
    /// A little of the text to the right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<String>,
}

/// How much text is kept on each side of a cursor, and around a word, for
/// recognising the place again after the document has moved on.
pub const CONTEXT_CHARS: usize = 8;

/// An element in the document, by the label written beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "node")]
pub struct TypstNodeRef {
    /// The label, without its angle brackets: `anno.7C42`.
    #[serde(rename = "ref")]
    pub label: String,
}

/// The top or bottom edge of a labelled element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "node_cursor")]
pub struct TypstNodeCursorRef {
    /// The label, without its angle brackets.
    #[serde(rename = "ref")]
    pub label: String,
    /// Which edge.
    pub side: VSide,
}

/// A labelled word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "word")]
pub struct TypstWordRef {
    /// The label, without its angle brackets.
    #[serde(rename = "ref")]
    pub label: String,
}

/// A position beside a labelled thing.
///
/// A label cannot stand between two characters — it attaches to the thing
/// before it — so a position in the source is a label and the side of it the
/// position lies on. This is where the browser's exactness is spent: an
/// arbitrary point becomes a point beside something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "text_cursor")]
pub struct TypstTextCursorRef {
    /// The label, without its angle brackets.
    #[serde(rename = "ref")]
    pub label: String,
    /// Which side of it.
    pub side: HSide,
}

/// What an annotation is about.
///
/// Some of these say *what* is annotated — a word, an equation, a drawing —
/// and some say *where* an annotation goes without anything being annotated at
/// all: a position between words is a place to put something, and a span is a
/// stretch with two ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", bound = "")]
pub enum Location<F: Flavor> {
    /// The word itself.
    #[serde(rename = "word")]
    Word {
        /// The word.
        #[serde(rename = "ref")]
        reference: F::Word,
    },
    /// The line the word is on. The same reference as a word, shown
    /// differently: a line is a fact about the rendering rather than about the
    /// document, so it is never anchored to as one.
    #[serde(rename = "line")]
    Line {
        /// The word the line is found from.
        #[serde(rename = "ref")]
        reference: F::Word,
    },
    /// The sentence containing the word, likewise.
    #[serde(rename = "sentence")]
    Sentence {
        /// The word the sentence is found from.
        #[serde(rename = "ref")]
        reference: F::Word,
    },
    /// A horizontal position: between two words, for something to be put.
    #[serde(rename = "pos.h")]
    PosH {
        /// Where.
        #[serde(rename = "ref")]
        reference: F::TextCursor,
    },
    /// A vertical position: between two blocks.
    #[serde(rename = "pos.v")]
    PosV {
        /// Where.
        #[serde(rename = "ref")]
        reference: F::NodeCursor,
    },
    /// A stretch of text, from one position to another.
    #[serde(rename = "span.h")]
    SpanH {
        /// Where it starts.
        begin: F::TextCursor,
        /// Where it ends.
        end: F::TextCursor,
    },
    /// A stretch of blocks, from one to another.
    #[serde(rename = "span.v")]
    SpanV {
        /// Where it starts.
        begin: F::NodeCursor,
        /// Where it ends.
        end: F::NodeCursor,
    },
    /// A fragment of raw text, annotated whole: it is one name, not a phrase.
    #[serde(rename = "raw")]
    Raw {
        /// The fragment.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// A paragraph.
    #[serde(rename = "para")]
    Para {
        /// The paragraph.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// One item of a list: a bullet, a numbered entry, a term.
    #[serde(rename = "item")]
    Item {
        /// The item.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// A block: a figure, a table, whatever a call laid out on its own.
    #[serde(rename = "block")]
    Block {
        /// The block.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// Something a label cannot be written inside — text a call produced from
    /// its arguments — annotated as the call.
    ///
    /// Produced by resolving rather than asked for: a browser says where it
    /// clicked, and this is what the answer sometimes has to be.
    #[serde(rename = "opaque")]
    Opaque {
        /// The call.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// An inline equation, annotated whole.
    #[serde(rename = "math")]
    Math {
        /// The equation.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// A block equation, which is a region of the document rather than a
    /// phrase in one.
    #[serde(rename = "math.block")]
    MathBlock {
        /// The equation.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// A link, annotated whole: its text is one destination.
    #[serde(rename = "link")]
    Link {
        /// The link.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// A drawing — a cetz canvas, a fletcher diagram — which reaches HTML as
    /// one picture and is annotated as one.
    #[serde(rename = "svg")]
    Svg {
        /// The drawing.
        #[serde(rename = "ref")]
        reference: F::Node,
    },
    /// The document itself, rather than anywhere in it.
    ///
    /// It names no anchor, so nothing about the document can make it stale:
    /// this is where an annotation goes when the place it was about is gone.
    #[serde(rename = "document")]
    Document,
}

/// A location in a rendering, as a browser makes them.
pub type HtmlLocation = Location<Html>;

/// A location in the document, as the sidecar keeps them.
pub type TypstLocation = Location<Typst>;

impl<F: Flavor> Location<F> {
    /// What kind of location this is, by the name it goes under.
    ///
    /// The same string the `type` field carries, for the places that want to
    /// group or report on locations without matching every variant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Word { .. } => "word",
            Self::Line { .. } => "line",
            Self::Sentence { .. } => "sentence",
            Self::PosH { .. } => "pos.h",
            Self::PosV { .. } => "pos.v",
            Self::SpanH { .. } => "span.h",
            Self::SpanV { .. } => "span.v",
            Self::Raw { .. } => "raw",
            Self::Para { .. } => "para",
            Self::Item { .. } => "item",
            Self::Block { .. } => "block",
            Self::Opaque { .. } => "opaque",
            Self::Math { .. } => "math",
            Self::MathBlock { .. } => "math.block",
            Self::Link { .. } => "link",
            Self::Svg { .. } => "svg",
            Self::Document => "document",
        }
    }

    /// Whether this location marks a stretch of the document rather than a
    /// point in it: a span has two ends, and is drawn between them.
    pub fn is_span(&self) -> bool {
        matches!(self, Self::SpanH { .. } | Self::SpanV { .. })
    }

    /// Whether this location is a place for something to go rather than
    /// something that is there: a caret, not a subject.
    pub fn is_position(&self) -> bool {
        matches!(self, Self::PosH { .. } | Self::PosV { .. })
    }
}

impl TypstLocation {
    /// Every label this location names, in the order it names them.
    ///
    /// What garbage collection counts: an anchor with no location naming it is
    /// an anchor nothing needs.
    pub fn labels(&self) -> Vec<&str> {
        match self {
            Self::Word { reference }
            | Self::Line { reference }
            | Self::Sentence { reference } => vec![reference.label.as_str()],
            Self::PosH { reference } => vec![reference.label.as_str()],
            Self::PosV { reference } => vec![reference.label.as_str()],
            Self::SpanH { begin, end } => vec![begin.label.as_str(), end.label.as_str()],
            Self::SpanV { begin, end } => vec![begin.label.as_str(), end.label.as_str()],
            Self::Raw { reference }
            | Self::Para { reference }
            | Self::Item { reference }
            | Self::Block { reference }
            | Self::Opaque { reference }
            | Self::Math { reference }
            | Self::MathBlock { reference }
            | Self::Link { reference }
            | Self::Svg { reference } => vec![reference.label.as_str()],
            Self::Document => vec![],
        }
    }
}
