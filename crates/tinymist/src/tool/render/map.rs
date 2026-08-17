//! Building the map that says what a rendering was made of.
//!
//! The rendering carries an id on every node that can be referred to. This
//! records, per id, the stretch of source each came from, so that a position
//! expressed against the rendering can be converted to a position in a file.
//!
//! Separate from the labelling walk: the walk determines the HTML, this records
//! what is known about it.

use std::collections::BTreeMap;

use tinymist_annos::render_map::{FileEntry, NodeEntry, NodeKind, RenderMap, Segment, SrcRange};
use crate::world::SourceWorld;
use typst::syntax::{FileId, Span};

/// The attribute an element carries its id in.
///
/// Not `id`, which Typst uses for link destinations. Frames are the exception:
/// an SVG can only carry the `id` that `HtmlFrame` provides.
pub const UID_ATTR: &str = "data-uid";

/// Collects the map as the rendering is walked.
pub struct Mapper<'a> {
    world: &'a dyn SourceWorld,
    /// Files that are part of the machinery rather than part of the document:
    /// the shim module. Spans from these are left unmapped, since the reader
    /// has no copy of them and cannot annotate them.
    hidden: Vec<FileId>,
    /// The files the rendering has drawn on, the document first.
    files: Vec<(FileId, FileEntry)>,
    /// What is known about each id.
    nodes: BTreeMap<String, NodeEntry>,
    /// How many ids have been given out.
    given: usize,
}

impl<'a> Mapper<'a> {
    /// Starts a map for a rendering of `main`.
    pub fn new(world: &'a dyn SourceWorld, main: FileId, hidden: Vec<FileId>) -> Self {
        let mut mapper = Self {
            world,
            hidden: hidden.into_iter().filter(|id| *id != main).collect(),
            files: Vec::new(),
            nodes: BTreeMap::new(),
            given: 0,
        };
        // The document being annotated is always index 0.
        mapper.file_index(main);
        mapper
    }

    /// The next id. Kept short: it appears once per element, and a document may
    /// have tens of thousands.
    pub fn uid(&mut self) -> String {
        self.given += 1;
        format!("n{}", self.given)
    }

    /// Where a file sits in the map, adding it if this is the first time it has
    /// come up.
    fn file_index(&mut self, id: FileId) -> u32 {
        if let Some(at) = self.files.iter().position(|(known, _)| *known == id) {
            return at as u32;
        }
        let path = self
            .world
            .path_for_id(id)
            .ok()
            .and_then(|path| path.to_err().ok())
            .map(|path: std::path::PathBuf| path.display().to_string())
            .unwrap_or_else(|| id.vpath().get_with_slash().to_string());
        // The file's contents at the time of this rendering, so that a stale
        // position can be detected later.
        let hash = self
            .world
            .source(id)
            .map(|source| super::html::hash_of(source.text().as_bytes()))
            .unwrap_or_default();
        self.files.push((id, FileEntry { path, hash }));
        (self.files.len() - 1) as u32
    }

    /// Where a span came from, in whichever file it came from.
    ///
    /// Unlike the attributes on the rendering, this accepts spans from any
    /// file, so content from an included file is addressable.
    pub fn range_of(&mut self, span: Span) -> Option<SrcRange> {
        let id = span.id()?;
        if self.hidden.contains(&id) {
            return None;
        }
        let source = self.world.source(id).ok()?;
        let range = typst_shim::syntax::source_range(&source, span)?;
        let file = self.file_index(id);
        Some(SrcRange {
            file,
            start: range.start,
            end: range.end,
        })
    }

    /// The range recorded for a node, if any.
    pub fn range_at(&self, uid: &str) -> Option<SrcRange> {
        self.nodes.get(uid).and_then(|node| node.range)
    }

    /// Gives a node the range its contents cover.
    ///
    /// A container built by the exporter — the body of a term, a list item —
    /// carries no position of its own, but what is inside it does. A block that
    /// does carry one carries the span of the first thing in it, which is not
    /// where the block ends: a paragraph of several runs would otherwise end
    /// where its first run does, and a label written after the paragraph would
    /// not be found to belong to it. Anything smaller than a block keeps the
    /// range it has, which is the text it is made of.
    pub fn cover(&mut self, uid: &str, range: SrcRange) {
        let Some(node) = self.nodes.get_mut(uid) else {
            return;
        };
        let Some(known) = node.range else {
            node.range = Some(range);
            return;
        };
        let holds = node.kind.is_block() || node.kind == NodeKind::Group;
        if holds && known.file == range.file {
            node.range = Some(SrcRange {
                file: known.file,
                start: known.start.min(range.start),
                end: known.end.max(range.end),
            });
        }
    }

    /// Records a whole thing whose place in the source is already known.
    pub fn node_at(&mut self, uid: &str, kind: NodeKind, range: Option<SrcRange>) {
        self.nodes.insert(
            uid.to_owned(),
            NodeEntry {
                kind,
                range,
                segments: Vec::new(),
            },
        );
    }

    /// The index the document itself has, for a caller that resolved a range
    /// against it by hand.
    pub fn document_index(&self) -> u32 {
        0
    }

    /// Records a whole thing: an equation, a drawing, a paragraph.
    pub fn node(&mut self, uid: &str, kind: NodeKind, span: Span) {
        let range = self.range_of(span);
        self.nodes.insert(
            uid.to_owned(),
            NodeEntry {
                kind,
                range,
                segments: Vec::new(),
            },
        );
    }

    /// Records a run of text: what it says, and where those characters came
    /// from.
    pub fn text(&mut self, uid: &str, kind: NodeKind, text: &str, span: Span) {
        let range = self.range_of(span);
        // One segment: a run interrupted by something that renders to nothing
        // (a label, a comment) currently arrives as several separate runs. When
        // those are merged, this becomes several segments of one node.
        let segments = range
            .map(|range| {
                vec![Segment {
                    at: 0,
                    len: text.chars().count(),
                    file: range.file,
                    offset: range.start,
                }]
            })
            .unwrap_or_default();
        self.nodes.insert(
            uid.to_owned(),
            NodeEntry {
                kind,
                range,
                segments,
            },
        );
    }

    /// Records that a node is a run of text, for an element that holds one and
    /// carries it directly rather than in a wrapper.
    ///
    /// The kind is set as well as the segments: an element that is only
    /// carrying a style — a bold run, the text of a heading — is a run of text
    /// with characters to point between, whatever its tag suggests.
    pub fn set_text(&mut self, uid: &str, text: &str, span: Span) {
        if let Some(entry) = self.nodes.get_mut(uid) {
            entry.kind = NodeKind::Text;
        }
        self.add_text(uid, text, span);
    }

    /// Adds text to a node that already has an entry, for the case where an
    /// element holds a single run and carries it directly rather than in a
    /// wrapper.
    pub fn add_text(&mut self, uid: &str, text: &str, span: Span) {
        let Some(range) = self.range_of(span) else {
            return;
        };
        if let Some(entry) = self.nodes.get_mut(uid) {
            entry.segments.push(Segment {
                at: entry
                    .segments
                    .last()
                    .map(|last| last.at + last.len)
                    .unwrap_or(0),
                len: text.chars().count(),
                file: range.file,
                offset: range.start,
            });
        }
    }

    /// The finished map, under the given render id.
    ///
    /// Elements whose own span says nothing — content built in code carries no
    /// position — are given the stretch of source between their neighbours,
    /// which is the expression that produced them. Without this an annotation
    /// on such an element has nothing in the rendering to attach to.
    pub fn finish(mut self, render: String, text: &str) -> RenderMap {
        self.infer_ranges(text);
        self.demote_duplicates();
        RenderMap {
            render,
            files: self.files.into_iter().map(|(_, entry)| entry).collect(),
            nodes: self.nodes,
        }
    }

    /// Turns a region that covers exactly what another region covers into a
    /// container.
    ///
    /// The exporter wraps a heading in a spacing `div`, so the two say the same
    /// thing about the same stretch of source. Offering both means two frames
    /// around one heading, and resolving an annotation may pick either. The
    /// outer one is the wrapper, since ids are given out from the outside in.
    fn demote_duplicates(&mut self) {
        let mut seen: std::collections::HashMap<(u32, usize, usize), Vec<String>> =
            std::collections::HashMap::new();
        for (uid, node) in &self.nodes {
            if !node.kind.is_block() {
                continue;
            }
            let Some(range) = node.range else { continue };
            seen.entry((range.file, range.start, range.end))
                .or_default()
                .push(uid.clone());
        }
        for (_, mut uids) in seen {
            if uids.len() < 2 {
                continue;
            }
            // In the order they were given out, which is the order they nest.
            uids.sort_by_key(|uid| uid[1..].parse::<usize>().unwrap_or(0));
            uids.pop();
            for uid in uids {
                if let Some(node) = self.nodes.get_mut(&uid) {
                    node.kind = NodeKind::Group;
                }
            }
        }
    }

    /// Fills in the ranges that could not be read from a span.
    fn infer_ranges(&mut self, text: &str) {
        // In document order, which is the order ids were given out.
        let order: Vec<String> = self.nodes.keys().cloned().collect();
        let known: Vec<Option<SrcRange>> = order
            .iter()
            .map(|uid| self.nodes.get(uid).and_then(|node| node.range))
            .collect();
        let mut inferred: Vec<(String, SrcRange)> = Vec::new();
        for (at, uid) in order.iter().enumerate() {
            if known[at].is_some() {
                continue;
            }
            let before = known[..at]
                .iter()
                .rev()
                .flatten()
                .find(|range| range.file == 0)
                .map(|range| range.end);
            let after = known[at + 1..]
                .iter()
                .flatten()
                .find(|range| range.file == 0)
                .map(|range| range.start);
            let (Some(start), Some(end)) = (before, after) else {
                continue;
            };
            let Some(range) = between(text, start, end) else {
                continue;
            };
            inferred.push((uid.clone(), range));
        }
        for (uid, range) in inferred {
            if let Some(node) = self.nodes.get_mut(&uid) {
                node.range = Some(range);
            }
        }
    }
}

/// The stretch of source between two positions, without the whitespace and
/// anchors at its edges.
///
/// A label written after an expression is not part of it: `#shout("x")<anno.A1>`
/// is the call, and the range must end where the call does or a label will
/// never be found to attach to it.
fn between(text: &str, start: usize, end: usize) -> Option<SrcRange> {
    if start >= end || end > text.len() {
        return None;
    }
    let mut range = start..end;
    while text[range.clone()].starts_with(char::is_whitespace) {
        range.start += text[range.clone()].chars().next()?.len_utf8();
    }
    loop {
        let slice = &text[range.clone()];
        let trimmed = slice.trim_end();
        if trimmed.len() != slice.len() {
            range.end -= slice.len() - trimmed.len();
            continue;
        }
        if slice.ends_with('>') {
            if let Some(open) = slice.rfind('<') {
                if slice[open..].starts_with(&format!("<{}", tinymist_annos::ANCHOR_PREFIX)) {
                    range.end = range.start + open;
                    continue;
                }
            }
        }
        break;
    }
    (range.start < range.end).then_some(SrcRange {
        file: 0,
        start: range.start,
        end: range.end,
    })
}

/// What kind of node an element counts as, by its tag.
///
/// Anything not listed is inline content, which is annotated whole. A
/// block-level element left off the list would be offered as one thing to
/// annotate — a caption underlined across the width of its figure — so the list
/// covers every block-level tag the exporter emits.
pub fn kind_of(tag: &str, display_block: bool) -> NodeKind {
    match tag {
        "math" if display_block => NodeKind::MathBlock,
        "math" => NodeKind::Math,
        "a" => NodeKind::Link,
        "code" | "pre" => NodeKind::Raw,
        "img" => NodeKind::Image,
        "p" => NodeKind::Para,
        // A term and its definition are the two halves of one item.
        "li" | "dt" | "dd" => NodeKind::Item,
        // Containers: a mark around all their children would mean "these
        // several things", which an anchor cannot say.
        "ul" | "ol" | "dl" | "thead" | "tbody" | "tfoot" | "tr" => NodeKind::Group,
        "div" | "section" | "figure" | "figcaption" | "caption" | "blockquote" | "td" | "th"
        | "table" | "main" | "header" | "footer" | "nav" | "article" | "aside" | "h1" | "h2"
        | "h3" | "h4" | "h5" | "h6" => NodeKind::Block,
        _ => NodeKind::Inline,
    }
}
