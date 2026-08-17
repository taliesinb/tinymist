//! What a rendering was made of: which piece of which file each part of it came
//! from.
//!
//! A browser can say where it clicked — this node, this many characters in —
//! but not what that means in a `.typ` file. This is the table that turns one
//! into the other, made by the renderer as it renders and kept by the server
//! afterwards, so that a position taken from a rendering can still be
//! understood once the document has moved on.
//!
//! It is carried beside the document rather than inside it. Attributes on every
//! element were how this worked before, which meant the mapping could only be as
//! rich as an attribute value, and content from an included file — whose ranges
//! belong to another file entirely — had nowhere to put its provenance and was
//! silently dropped. A table has room to say "this run is three pieces, and the
//! middle one comes from another file".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A file a rendering drew on, by the index the map refers to it by.
pub type FileIndex = u32;

/// Where a rendering's parts came from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderMap {
    /// What this rendering is. Derived from the rendering itself, so that a
    /// document which compiles to the same thing keeps the same name for it and
    /// a page that reconnects after a restart is still talking about something
    /// the server knows.
    pub render: String,
    /// The files it drew on. The document being annotated is always the first.
    pub files: Vec<FileEntry>,
    /// Every part of the rendering that can be pointed at, by the id it carries
    /// in the HTML.
    pub nodes: BTreeMap<String, NodeEntry>,
}

/// One file a rendering drew on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Where it is, as the server addresses it.
    pub path: String,
    /// A hash of its text as the rendering was made from it, so that a position
    /// taken against this rendering can be recognised as being against an older
    /// version of the file.
    pub hash: String,
}

/// What kind of thing a node is.
///
/// The client needs this to know what it may offer — a drawing is annotated
/// whole, a run of text can be pointed into — and the resolver needs it to
/// check that a vertical position was asked for against something with a top
/// and a bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A run of text, which has characters to point between.
    Text,
    /// A figure, a table, a callout: something with a top and a bottom that is
    /// neither a paragraph nor a list item.
    Block,
    /// A paragraph.
    Para,
    /// One item of a list: a bullet, a numbered entry, a term.
    Item,
    /// An inline element: emphasis, a span the exporter made.
    Inline,
    /// An inline equation.
    Math,
    /// A block equation.
    MathBlock,
    /// A link.
    Link,
    /// A fragment of raw text.
    Raw,
    /// A drawing, embedded as SVG.
    Svg,
    /// Something that holds other things and is not itself a place: a list, a
    /// table's row group. An annotation on "these three items" cannot be said
    /// in the document, so these are never offered and never resolved to.
    Group,
    /// An image.
    Image,
}

impl NodeKind {
    /// Whether this is a thing with a top and a bottom, which a vertical
    /// position can sit at the edge of.
    pub fn is_block(self) -> bool {
        matches!(
            self,
            Self::Block | Self::Para | Self::Item | Self::MathBlock | Self::Svg | Self::Image
        )
    }

    /// Whether characters inside it can be pointed between.
    pub fn has_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// One addressable part of a rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEntry {
    /// What it is.
    pub kind: NodeKind,
    /// Where the whole of it came from, for anything that is one thing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<SrcRange>,
    /// Where each stretch of its text came from, for runs. A run is in pieces
    /// whenever something that renders to nothing — a label, a comment — sits
    /// in the middle of it, so the mapping from characters to source is a list
    /// rather than a sum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<Segment>,
}

impl NodeEntry {
    /// The source position a character offset in this node's text stands for.
    ///
    /// A position at the very end of a segment belongs to that segment, so that
    /// the end of a run is a place and not a failure.
    pub fn source_of(&self, at: usize) -> Option<(FileIndex, usize)> {
        let mut last = None;
        for segment in &self.segments {
            if at < segment.at {
                break;
            }
            let within = at - segment.at;
            if within < segment.len {
                return Some((segment.file, segment.offset + within));
            }
            if within == segment.len {
                last = Some((segment.file, segment.offset + segment.len));
            }
        }
        last.or_else(|| {
            let range = self.range.as_ref()?;
            Some((range.file, range.start))
        })
    }

    /// How many characters of text this node has, by its segments.
    pub fn text_len(&self) -> usize {
        self.segments
            .last()
            .map(|segment| segment.at + segment.len)
            .unwrap_or(0)
    }
}

/// A stretch of one node's text, and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Where this stretch starts, in characters from the start of the node's
    /// text.
    pub at: usize,
    /// How long it is, in characters.
    pub len: usize,
    /// Which file it came from.
    pub file: FileIndex,
    /// Where in that file, as a byte offset.
    pub offset: usize,
}

/// A stretch of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrcRange {
    /// Which file.
    pub file: FileIndex,
    /// Where it starts, as a byte offset.
    pub start: usize,
    /// Where it ends.
    pub end: usize,
}

impl RenderMap {
    /// A node by the id it carries in the HTML.
    pub fn node(&self, uid: &str) -> Option<&NodeEntry> {
        self.nodes.get(uid)
    }

    /// The run of text covering a source position, and how far into it the
    /// position is.
    ///
    /// The reverse of [`NodeEntry::source_of`], used to draw an annotation: the
    /// sidecar says where in the document it points, and the page needs to know
    /// where that is in what it is showing.
    ///
    /// A position at the end of one run is also the position at the start of
    /// the next, and an anchor is written after what it names, so the run that
    /// ends there is the one meant. A label between a word and the space after
    /// it would otherwise name the space.
    pub fn text_at(&self, file: FileIndex, offset: usize) -> Option<(&str, usize)> {
        let mut ends: Option<(&str, usize)> = None;
        let mut inside: Option<(&str, usize)> = None;
        let mut begins: Option<(&str, usize)> = None;
        for (uid, node) in &self.nodes {
            for segment in &node.segments {
                if segment.file != file {
                    continue;
                }
                let Some(within) = offset.checked_sub(segment.offset) else {
                    continue;
                };
                if within > segment.len {
                    continue;
                }
                let candidate = (uid.as_str(), segment.at + within);
                if within == segment.len && segment.len > 0 {
                    ends = ends.or(Some(candidate));
                } else if within == 0 {
                    begins = begins.or(Some(candidate));
                } else {
                    inside = inside.or(Some(candidate));
                }
            }
        }
        inside.or(ends).or(begins)
    }

    /// The innermost node of the given kinds whose source range contains a
    /// position.
    ///
    /// A label for a region is written wherever it suits inside the region — in
    /// the middle of a list item, at the end of a paragraph — rather than after
    /// it, so the region is the one the label sits in.
    pub fn node_containing(
        &self,
        file: FileIndex,
        offset: usize,
        kinds: &[NodeKind],
    ) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (uid, node) in &self.nodes {
            if !kinds.is_empty() && !kinds.contains(&node.kind) {
                continue;
            }
            let Some(range) = node.range else { continue };
            if range.file != file || offset < range.start || offset > range.end {
                continue;
            }
            let width = range.end.saturating_sub(range.start);
            if best.is_none_or(|(_, known)| width < known) {
                best = Some((uid.as_str(), width));
            }
        }
        best.map(|(uid, _)| uid)
    }

    /// The node a label at this position attaches to: the one whose source
    /// range ends where the label begins.
    ///
    /// Only nodes of the kinds asked for are considered. A paragraph and the
    /// last word in it end in the same place, and which of them a label names
    /// is decided by the kind of annotation, not by the position.
    pub fn node_ending_at(
        &self,
        file: FileIndex,
        offset: usize,
        kinds: &[NodeKind],
    ) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (uid, node) in &self.nodes {
            if !kinds.is_empty() && !kinds.contains(&node.kind) {
                continue;
            }
            let Some(range) = node.range else { continue };
            if range.file != file || range.end != offset {
                continue;
            }
            let width = range.end.saturating_sub(range.start);
            // The narrowest element ending here is the one the label names: a
            // paragraph and the word at its end both end in the same place.
            if best.is_none_or(|(_, known)| width < known) {
                best = Some((uid.as_str(), width));
            }
        }
        best.map(|(uid, _)| uid)
    }

    /// A node of one of the given kinds lying inside a stretch of a file.
    ///
    /// For a picture whose anchor could not be written beside it. A label may
    /// not go inside a call's arguments, so an image written as
    /// `#figure(image(..), caption: ..)` takes its anchor after the whole call,
    /// and nothing of the image's own kind ends there. What the label names is
    /// the figure; the picture is the one inside it.
    pub fn node_within(
        &self,
        file: FileIndex,
        range: SrcRange,
        kinds: &[NodeKind],
    ) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (uid, node) in &self.nodes {
            if !kinds.is_empty() && !kinds.contains(&node.kind) {
                continue;
            }
            let Some(own) = node.range else { continue };
            if own.file != file || own.start < range.start || own.end > range.end {
                continue;
            }
            // The widest, since a picture is the largest thing of its kind in
            // whatever holds it.
            let width = own.end.saturating_sub(own.start);
            if best.is_none_or(|(_, known)| width > known) {
                best = Some((uid.as_str(), width));
            }
        }
        best.map(|(uid, _)| uid)
    }

    /// The range recorded for a node.
    pub fn range_of(&self, uid: &str) -> Option<SrcRange> {
        self.node(uid)?.range
    }

    /// The file a rendering is *of* — the document being annotated, which is
    /// always the first one.
    pub fn document(&self) -> Option<&FileEntry> {
        self.files.first()
    }

    /// Reads a map.
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|err| format!("cannot read the render map: {err}"))
    }

    /// Writes a map, the way the sidecar is written: readable, and the same
    /// bytes for the same content.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
        text.push('\n');
        text
    }
}
