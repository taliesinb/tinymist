//! Anchors in a document: finding them, placing them, removing them.
//!
//! An anchor is a Typst label of the form `<anno.7C42>`, written into the
//! document at the place an annotation refers to. It carries no meaning of its
//! own; the sidecar says what each one is for. Several annotations may refer to
//! the same anchor, which is why anchors are not named after annotations.
//!
//! A label attaches to the element that precedes it, so an anchor placed after a
//! word marks that word. Labels may not be written inside a string, a comment or
//! a call's arguments, so an insertion point is snapped to the nearest position
//! in markup where a label is valid.

use std::ops::Range;

use typst_syntax::{LinkedNode, Source, SyntaxKind};

use crate::ANCHOR_PREFIX;

/// An anchor found in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The id, without the `anno.` prefix and the angle brackets.
    pub id: String,
    /// Where the label itself is written, including the brackets.
    pub label: Range<usize>,
}

impl Anchor {
    /// The position the anchor marks: the end of the element it attaches to,
    /// which is where the label begins.
    pub fn at(&self) -> usize {
        self.label.start
    }

    /// The label as a location refers to it, prefix included: `anno.7C42`.
    pub fn name(&self) -> String {
        crate::anchor_label(&self.id)
    }
}

/// Every anchor in a document, in the order they appear.
///
/// Read from the syntax tree rather than by searching the text, so that
/// `<anno.A100>` written inside raw text or a comment is not mistaken for an
/// anchor. A document that documents this format contains several such
/// mentions.
pub fn anchors_in(source: &Source) -> Vec<Anchor> {
    let open = format!("<{ANCHOR_PREFIX}");
    let text = source.text();
    let mut found = Vec::new();
    collect(&LinkedNode::new(source.root()), text, &open, &mut found);
    found.sort_by_key(|anchor| anchor.label.start);
    found
}

fn collect(node: &LinkedNode, text: &str, open: &str, found: &mut Vec<Anchor>) {
    if node.kind() == SyntaxKind::Label {
        let range = node.range();
        let written = &text[range.clone()];
        if let Some(id) = written
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix('>'))
        {
            if !id.is_empty() && id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                found.push(Anchor {
                    id: id.to_owned(),
                    label: range,
                });
            }
        }
    }
    for child in node.children() {
        collect(&child, text, open, found);
    }
}

/// Every anchor in a text that has not been parsed yet.
pub fn anchors_in_text(text: &str) -> Vec<Anchor> {
    anchors_in(&Source::detached(text.to_owned()))
}

/// The anchor with a given id.
pub fn find(source: &Source, id: &str) -> Option<Anchor> {
    anchors_in(source).into_iter().find(|anchor| anchor.id == id)
}

/// The anchor a label written at a position would collide with.
///
/// A label attaches to the element before it, and whitespace between the two
/// does not separate them. Two labels written next to each other therefore
/// attach to the same element, and Typst keeps only the last: "only the last
/// label is used, the rest are ignored". The annotation using the other one
/// then has no element in the rendering and shows as an orphan.
///
/// So a position that only whitespace separates from an anchor is that
/// anchor's position, and the annotation shares it. This looks both ways: a
/// label written before an existing one collides with it just as one written
/// after does.
pub fn colliding(source: &Source, offset: usize) -> Option<Anchor> {
    let text = source.text();
    let blank = |range: Range<usize>| text[range].chars().all(char::is_whitespace);
    anchors_in(source).into_iter().find(|anchor| {
        (anchor.label.end <= offset && blank(anchor.label.end..offset))
            || (offset <= anchor.label.start && blank(offset..anchor.label.start))
    })
}

/// Why a position cannot take an anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The position is inside a string, a comment or a raw block, where a label
    /// would be read as text.
    NotMarkup,
    /// A label at the position would not mean what it says: inside code it is
    /// not valid syntax, and inside a heading it ends the heading. The
    /// enclosing element can be annotated instead; its range is given.
    Coarsen(Range<usize>),
}

/// Where an anchor for a position would be written.
///
/// Returns the offset the label should be inserted at. The position is snapped
/// forwards to the end of the word it falls in, so that the label attaches to a
/// whole word rather than splitting one, which would divide the word into two
/// text runs in the rendering.
pub fn place(source: &Source, offset: usize) -> Result<usize, Refusal> {
    let text = source.text();
    let offset = offset.min(text.len());
    let root = LinkedNode::new(source.root());
    // Asked at a boundary the tree answers with either side, so the question is
    // asked from just inside the position.
    let probe = offset.saturating_sub(1);
    let leaf: LinkedNode = match root.leaf_at(probe, typst_syntax::Side::After) {
        Some(leaf) => leaf,
        // Past the last node: the end of the document is markup.
        None => return Ok(offset),
    };

    // A label inside a heading ends it: `== What<anno.X> are annotations?` is a
    // heading that says "What" followed by a paragraph. Only the end of the
    // heading can carry one, and that names the whole heading.
    if let Some(heading) = enclosing_heading(text, &leaf) {
        if heading.end > offset {
            return Err(Refusal::Coarsen(heading));
        }
    }

    match leaf.kind() {
        // Markup text: snap to a word boundary so the label does not split a
        // word.
        SyntaxKind::Text | SyntaxKind::MathText => Ok(word_end(text, offset)),
        // Whitespace and paragraph breaks are markup, and a label may go
        // directly there — unless nothing precedes it, in which case the label
        // would attach to whatever is outside: written as `== <anno.X>Title`
        // the heading loses its text. The next word takes the anchor instead,
        // and the position is recorded as being to that word's left.
        SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::SmartQuote => {
            if opens_content(text, &root, offset) {
                let mut at = offset;
                while text[at..].starts_with(char::is_whitespace) {
                    at += text[at..].chars().next().map_or(0, char::len_utf8);
                }
                Ok(word_end(text, at))
            } else {
                Ok(offset)
            }
        }
        SyntaxKind::Str | SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Raw => {
            Err(Refusal::NotMarkup)
        }
        // Anything else came from code. The expression containing it is what
        // can be annotated.
        _ => Err(Refusal::Coarsen(expression_around(text, &leaf))),
    }
}

/// The content of the heading a node is in, if it is in one.
///
/// The range ends where the heading's text does, so an anchor for it goes
/// there: written after the text and before the line ends, which is where a
/// heading's label belongs.
fn enclosing_heading(text: &str, node: &LinkedNode) -> Option<Range<usize>> {
    let mut cursor = node.clone();
    loop {
        if cursor.kind() == SyntaxKind::Heading {
            let mut range = cursor.range();
            // Trailing whitespace is not part of the heading's text.
            let written = &text[range.clone()];
            range.end -= written.len() - written.trim_end().len();
            return Some(range);
        }
        cursor = cursor.parent()?.clone();
    }
}

/// Whether a position has nothing before it that a label could attach to: the
/// start of a heading, of a list item, of a bracketed block, of a paragraph.
///
/// A label attaches to what precedes it, so one written at the start of a
/// paragraph names the paragraph before, which may already carry a label of its
/// own. The first word of the paragraph takes it instead.
fn opens_content(text: &str, root: &LinkedNode, offset: usize) -> bool {
    let mut at = offset;
    while let Some(prev) = text[..at].chars().next_back() {
        if !prev.is_whitespace() {
            break;
        }
        at -= prev.len_utf8();
    }
    if at == 0 {
        return true;
    }
    // A blank line between: what precedes is another block.
    if text[at..offset].matches('\n').count() >= 2 {
        return true;
    }
    let Some(leaf) = root.leaf_at(at - 1, typst_syntax::Side::After) else {
        return true;
    };
    matches!(
        leaf.kind(),
        SyntaxKind::HeadingMarker
            | SyntaxKind::ListMarker
            | SyntaxKind::EnumMarker
            | SyntaxKind::TermMarker
            | SyntaxKind::LeftBracket
            | SyntaxKind::Colon
    )
}

/// The end of the word containing a position.
///
/// A word runs to the next whitespace, so trailing punctuation belongs to it:
/// the client uses the same rule when it says which word was clicked, and the
/// two must agree or the label will not be found again. Markup that closes an
/// enclosing expression ends it as well, so that a word inside `#emph[..]`
/// keeps its label inside the block rather than after the whole call; so does a
/// label already written there, which would otherwise be what the new one
/// names.
pub fn word_end(text: &str, offset: usize) -> usize {
    let mut end = offset;
    for (at, ch) in text[offset..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | ']' | ')' | '}') {
            break;
        }
        end = offset + at + ch.len_utf8();
    }
    end
}

/// The start of the word ending at a position, by the same rule.
pub fn word_start(text: &str, offset: usize) -> usize {
    let mut start = offset;
    for (at, ch) in text[..offset].char_indices().rev() {
        if ch.is_whitespace() || matches!(ch, '>' | '[' | '(' | '{') {
            break;
        }
        start = at;
    }
    start
}

/// The whole expression a node belongs to: the outermost one that markup
/// contains, such as a complete `#figure(..)` call.
///
/// The range of a call in markup starts at its name, so a leading `#` is added
/// back: the expression a reader sees includes it, and an edit that replaced the
/// range without it would leave the `#` behind.
fn expression_around(text: &str, node: &LinkedNode) -> Range<usize> {
    let mut cursor = node.clone();
    let mut range = cursor.range();
    while let Some(parent) = cursor.parent() {
        if parent.kind() == SyntaxKind::Markup {
            range = cursor.range();
            break;
        }
        cursor = parent.clone();
        range = cursor.range();
    }
    if range.start > 0 && text.as_bytes()[range.start - 1] == b'#' {
        range.start -= 1;
    }
    range
}

/// A change to make to a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Which of the document's files it goes in, as the rendering numbers
    /// them: the document itself is 0, and the rest are what it includes and
    /// imports.
    pub file: usize,
    /// Where the text goes in that file.
    pub at: usize,
    /// What to insert.
    pub text: String,
}

/// The edit that writes an anchor at a position in one of the document's files.
pub fn insert(id: &str, file: usize, at: usize) -> Edit {
    Edit {
        file,
        at,
        text: format!("<{ANCHOR_PREFIX}{id}>"),
    }
}

/// The edit that removes an anchor.
pub fn remove(anchor: &Anchor) -> (Range<usize>, String) {
    (anchor.label.clone(), String::new())
}

/// Applies edits to a text, last first so that earlier offsets stay valid.
pub fn apply(file: usize, text: &str, edits: &[Edit]) -> String {
    let mut edits: Vec<Edit> = edits.iter().filter(|edit| edit.file == file).cloned().collect();
    edits.sort_by_key(|edit| edit.at);
    let mut out = String::with_capacity(text.len() + edits.iter().map(|e| e.text.len()).sum::<usize>());
    let mut at = 0;
    for edit in edits {
        let cut = edit.at.min(text.len());
        out.push_str(&text[at..cut]);
        out.push_str(&edit.text);
        at = cut;
    }
    out.push_str(&text[at..]);
    out
}

/// An id nothing in the document is using.
///
/// Four hexadecimal characters, checked against the document rather than
/// assumed unique: a few hundred anchors give a noticeable chance of collision,
/// and the document is available to check.
pub fn fresh_id(source: &Source, seed: u64) -> String {
    let taken: Vec<String> = anchors_in(source).into_iter().map(|a| a.id).collect();
    let mut hash = seed | 1;
    for _ in 0..64 {
        hash = hash.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let id = format!("{:04X}", (hash >> 32) as u16);
        if !taken.contains(&id) {
            return id;
        }
    }
    format!("{:04X}", taken.len() as u16)
}

#[cfg(test)]
mod collision_tests {
    use super::{colliding, Anchor};
    use typst_syntax::Source;

    fn found(text: &str, offset: usize) -> Option<Anchor> {
        colliding(&Source::detached(text.to_owned()), offset)
    }

    #[test]
    fn a_position_a_space_after_an_anchor_is_that_anchors_position() {
        let text = "Here.<anno.174F> Scored under it.";
        assert_eq!(found(text, 17).map(|a| a.id), Some("174F".into()));
    }

    #[test]
    fn a_position_just_before_an_anchor_is_too() {
        let text = "The monoid<anno.A1B2> is here.";
        assert_eq!(found(text, 10).map(|a| a.id), Some("A1B2".into()));
    }

    #[test]
    fn a_position_with_a_word_in_between_is_not() {
        let text = "The monoid<anno.A1B2> is here.";
        // After "is", which the anchor does not reach across.
        assert_eq!(found(text, 24), None);
    }

    #[test]
    fn a_line_break_does_not_separate_two_labels_either() {
        let text = "A paragraph.<anno.C0DE>\nMore of it.";
        assert_eq!(found(text, 24).map(|a| a.id), Some("C0DE".into()));
    }
}
