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

/// An anchor already at a position, if there is one.
///
/// Two annotations about the same word share an anchor rather than writing one
/// each, since a Typst element carries at most one label.
pub fn at_position(source: &Source, offset: usize) -> Option<Anchor> {
    anchors_in(source)
        .into_iter()
        .find(|anchor| anchor.at() == offset)
}

/// Why a position cannot take an anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The position is inside a string, a comment or a raw block, where a label
    /// would be read as text.
    NotMarkup,
    /// The position is inside code, where a label is not valid syntax. The
    /// enclosing expression can be annotated instead; its range is given.
    InsideCode(Range<usize>),
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

    match leaf.kind() {
        // Markup text: snap to a word boundary so the label does not split a
        // word.
        SyntaxKind::Text | SyntaxKind::MathText => Ok(word_end(text, offset)),
        // Whitespace and paragraph breaks are markup, and a label may go
        // directly there.
        SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::SmartQuote => Ok(offset),
        SyntaxKind::Str | SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Raw => {
            Err(Refusal::NotMarkup)
        }
        // Anything else came from code. The expression containing it is what
        // can be annotated.
        _ => Err(Refusal::InsideCode(expression_around(text, &leaf))),
    }
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
    /// Where the text goes.
    pub at: usize,
    /// What to insert.
    pub text: String,
}

/// The edit that writes an anchor at a position.
pub fn insert(id: &str, at: usize) -> Edit {
    Edit {
        at,
        text: format!("<{ANCHOR_PREFIX}{id}>"),
    }
}

/// The edit that removes an anchor.
pub fn remove(anchor: &Anchor) -> (Range<usize>, String) {
    (anchor.label.clone(), String::new())
}

/// Applies edits to a text, last first so that earlier offsets stay valid.
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut edits = edits.to_vec();
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
