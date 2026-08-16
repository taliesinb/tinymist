//! Where a drawing came from.
//!
//! A drawing reaches HTML as a frame: content laid out by the paged engine and
//! embedded as SVG. Typst attributes a frame to the call that framed it, which
//! for these is a single line of the shim module, so every drawing in a document
//! reports the same source position.
//!
//! The frame's contents carry better information. Each glyph, shape and image
//! records the span it was laid out from, and for a diagram written in the
//! document those spans are in the document. The source position of a drawing is
//! therefore derived from its contents rather than from the frame.

use typst::layout::{Frame, FrameItem};
use typst::syntax::{FileId, LinkedNode, Source, Span, SyntaxKind};

/// How far apart the pieces of a drawing may be before their common ancestor
/// stops being believable.
///
/// A diagram whose labels are written at the diagram gives a narrow range. One
/// drawing content defined far away gives a range covering everything between
/// the two, whose smallest enclosing node may be most of the document. Such a
/// result is rejected rather than reported.
const MAX_SPREAD: usize = 4000;

/// The place in the document a drawing was made from, if it can be told.
///
/// The smallest expression containing every span the frame's contents record,
/// which for a diagram written in the document is the call that drew it.
pub fn source_of(frame: &Frame, source: &Source, main: FileId) -> Option<std::ops::Range<usize>> {
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    collect(frame, source, main, &mut lo, &mut hi);
    if lo > hi {
        return None;
    }
    if hi - lo > MAX_SPREAD {
        return None;
    }
    enclosing(source, lo, hi)
}

/// Every span in a frame that belongs to the document, as a range.
fn collect(frame: &Frame, source: &Source, main: FileId, lo: &mut usize, hi: &mut usize) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect(&group.frame, source, main, lo, hi),
            FrameItem::Text(text) => {
                for glyph in &text.glyphs {
                    let (span, _) = glyph.span;
                    note(span, source, main, lo, hi);
                }
            }
            FrameItem::Shape(_, span) => note(*span, source, main, lo, hi),
            FrameItem::Image(_, _, span) => note(*span, source, main, lo, hi),
            // A link carries no span, and a tag is introspection rather than
            // drawn content; neither indicates a source position.
            FrameItem::Link(..) | FrameItem::Tag(_) => {}
        }
    }
}

/// Widens the range to include a span, if the span is from the document.
fn note(span: Span, source: &Source, main: FileId, lo: &mut usize, hi: &mut usize) {
    if span.id() != Some(main) {
        return;
    }
    let Some(range) = typst_shim::syntax::source_range(source, span) else {
        return;
    };
    *lo = (*lo).min(range.start);
    *hi = (*hi).max(range.end);
}

/// The call that drew a range's worth of ink.
///
/// Descends from the root, taking at each step the child that contains the whole
/// range. The deepest `FuncCall` so reached is the drawing call: a picture's
/// content comes from several sub-calls (`place`, `circle`, one `node` per
/// corner), so the smallest call containing all of them is the one that drew
/// them.
///
/// Markup, code and content blocks do not count as enclosing expressions, since
/// they identify the document rather than a call in it. A range no expression
/// contains yields `None`.
fn enclosing(source: &Source, lo: usize, hi: usize) -> Option<std::ops::Range<usize>> {
    let mut node = LinkedNode::new(source.root());
    let mut call: Option<std::ops::Range<usize>> = None;
    let mut best: Option<std::ops::Range<usize>> = None;
    loop {
        let next = node
            .children()
            .find(|child| child.range().start <= lo && hi <= child.range().end);
        let Some(next) = next else { break };
        if matches!(next.kind(), SyntaxKind::FuncCall) {
            call = Some(next.range());
        } else if !matches!(
            next.kind(),
            SyntaxKind::Markup | SyntaxKind::Code | SyntaxKind::CodeBlock | SyntaxKind::ContentBlock
        ) {
            best = Some(next.range());
        }
        node = next;
    }
    call.or(best)
}
