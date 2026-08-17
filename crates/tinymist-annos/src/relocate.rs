//! A location taken against one rendering, expressed against another.
//!
//! A page holds annotations the server has not got yet: one being written, one
//! sent and not answered for. They point at the rendering the page was looking
//! at when they were made, by ids that mean nothing in any other rendering —
//! and a document that is edited while somebody is writing about it produces
//! another rendering, with every id reassigned. Drawing them where they last
//! were is what the page does when it has nothing better, and it is wrong as
//! soon as the text moves.
//!
//! This is the something better. A position in a rendering is a position in the
//! document — that is what the render map says — and a position in the document
//! survives an edit somewhere else in it, which is what [`crate::migrate`]
//! works out. So: down to the source through the old map, across the edit,
//! and back up through the new one.

use crate::location::{
    HtmlLocation, HtmlNodeCursorRef, HtmlNodeRef, HtmlTextCursorRef, HtmlWordRef, Location,
};
use crate::migrate::{Rebase, Shift};
use crate::render_map::{NodeKind, RenderMap};

/// The two renderings a location is being carried between, and the document as
/// it was and is.
pub struct Between<'a> {
    /// The rendering the location was taken against.
    pub was: &'a RenderMap,
    /// The document text that rendering was made from.
    pub was_text: &'a str,
    /// The rendering it is being expressed against.
    pub now: &'a RenderMap,
    /// The document text now.
    pub now_text: &'a str,
}

impl Between<'_> {
    /// Where a position in the old document is in the new one.
    fn moved(&self, offset: usize) -> Option<usize> {
        match Rebase::between(self.was_text, self.now_text).at(offset) {
            Shift::At(offset) => Some(offset),
            Shift::Lost => None,
        }
    }

    /// The source position a character in a run stood for.
    fn source_of(&self, node: &str, at: usize) -> Option<usize> {
        let entry = self.was.node(node)?;
        let (file, offset) = entry.source_of(at)?;
        // Only the document itself: a position in an included file is a
        // position in a text this cannot compare.
        (file == 0).then_some(offset)
    }

    /// The run and character offset a source position falls in now.
    fn text_at(&self, offset: usize) -> Option<(String, usize)> {
        let (node, at) = self.now.text_at(0, offset)?;
        Some((node.to_owned(), at))
    }
}

/// A location against the old rendering, expressed against the new one.
///
/// `None` when the place it named is no longer in the document, or when the two
/// renderings cannot be compared. The caller decides what that means: for a
/// draft it means the mark stays where it was drawn and the words are kept.
pub fn relocate(ctx: &Between, location: &HtmlLocation) -> Option<HtmlLocation> {
    let word = |reference: &HtmlWordRef| -> Option<HtmlWordRef> {
        let beg = ctx.moved(ctx.source_of(&reference.node, reference.beg)?)?;
        let end = ctx.moved(ctx.source_of(&reference.node, reference.end)?)?;
        let (node, at) = ctx.text_at(beg)?;
        // The end is measured in the run the start landed in: the two ends of
        // one word are one run's business, and a word split across runs by an
        // edit is a word this cannot claim to have found.
        let (end_node, end_at) = ctx.text_at(end)?;
        let end_at = if end_node == node {
            end_at
        } else {
            at + reference.end.saturating_sub(reference.beg)
        };
        (end_at > at).then(|| HtmlWordRef {
            node,
            beg: at,
            end: end_at,
            w: reference.w.clone(),
        })
    };
    let cursor = |reference: &HtmlTextCursorRef| -> Option<HtmlTextCursorRef> {
        let offset = ctx.moved(ctx.source_of(&reference.node, reference.pos)?)?;
        let (node, pos) = ctx.text_at(offset)?;
        Some(HtmlTextCursorRef {
            node,
            pos,
            l: reference.l.clone(),
            r: reference.r.clone(),
        })
    };
    // A region is found again by what it covers rather than by where it ends:
    // an edit inside it moves its end, and the smallest thing of the same kind
    // containing its first character is the same thing.
    let node = |reference: &HtmlNodeRef| -> Option<HtmlNodeRef> {
        let was = ctx.was.node(&reference.node)?;
        let range = was.range?;
        if range.file != 0 {
            return None;
        }
        let start = ctx.moved(range.start)?;
        let kinds = [was.kind];
        let found = ctx
            .now
            .node_containing(0, start, &kinds)
            .or_else(|| ctx.now.node_ending_at(0, ctx.moved(range.end)?, &kinds))?;
        Some(HtmlNodeRef {
            node: found.to_owned(),
        })
    };
    let edge = |reference: &HtmlNodeCursorRef| -> Option<HtmlNodeCursorRef> {
        let found = node(&HtmlNodeRef {
            node: reference.node.clone(),
        })?;
        Some(HtmlNodeCursorRef {
            node: found.node,
            side: reference.side,
        })
    };

    Some(match location {
        Location::Word { reference } => Location::Word {
            reference: word(reference)?,
        },
        Location::Line { reference } => Location::Line {
            reference: word(reference)?,
        },
        Location::Sentence { reference } => Location::Sentence {
            reference: word(reference)?,
        },
        Location::PosH { reference } => Location::PosH {
            reference: cursor(reference)?,
        },
        Location::SpanH { begin, end } => Location::SpanH {
            begin: cursor(begin)?,
            end: cursor(end)?,
        },
        Location::PosV { reference } => Location::PosV {
            reference: edge(reference)?,
        },
        Location::SpanV { begin, end } => Location::SpanV {
            begin: edge(begin)?,
            end: edge(end)?,
        },
        Location::Raw { reference } => Location::Raw {
            reference: node(reference)?,
        },
        Location::Para { reference } => Location::Para {
            reference: node(reference)?,
        },
        Location::Item { reference } => Location::Item {
            reference: node(reference)?,
        },
        Location::Block { reference } => Location::Block {
            reference: node(reference)?,
        },
        Location::Opaque { reference } => Location::Opaque {
            reference: node(reference)?,
        },
        Location::Math { reference } => Location::Math {
            reference: node(reference)?,
        },
        Location::MathBlock { reference } => Location::MathBlock {
            reference: node(reference)?,
        },
        Location::Link { reference } => Location::Link {
            reference: node(reference)?,
        },
        Location::Svg { reference } => Location::Svg {
            reference: node(reference)?,
        },
        // It names nothing in either rendering.
        Location::Document => Location::Document,
    })
}

/// Whether a kind is one a region can be found again by.
///
/// Unused today — every kind is tried — and kept as the place to say so if a
/// kind turns out not to survive being looked for.
pub fn is_findable(kind: NodeKind) -> bool {
    !matches!(kind, NodeKind::Group)
}
