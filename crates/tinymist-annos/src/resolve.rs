//! Converting a location expressed against a rendering into one expressed
//! against the document.
//!
//! A browser names places in the rendering it fetched: a node by the id the
//! renderer gave it, a position by a character offset in a run. The sidecar
//! names places in the document: a Typst label, and which side of it a position
//! lies on. This module performs the conversion.
//!
//! The conversion has three steps. The reference is turned into a source offset
//! using the render map. The offset is translated into the document as it stands
//! now, since the document may have changed since the rendering was made. An
//! anchor at that offset is then found or created.
//!
//! The result is not always what was asked for. A position inside text produced
//! by a call cannot take a label, so the whole call is annotated instead, and
//! the location comes back as `opaque`. Callers must therefore use the returned
//! location rather than assume it matches the request.

use typst_syntax::Source;

use crate::anchor::{self, Edit, Refusal};
use crate::location::{
    HSide, HtmlLocation, Location, TypstLocation, TypstNodeCursorRef, TypstNodeRef,
    TypstTextCursorRef, TypstWordRef,
};
use crate::migrate::{Rebase, Shift};
use crate::render_map::RenderMap;

/// What a conversion produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The location as recorded, which may differ from the one requested.
    pub location: TypstLocation,
    /// Anchors to write into the document. Empty when every anchor the location
    /// needs was already there.
    pub edits: Vec<Edit>,
    /// Whether the request was answered with something coarser than it asked
    /// for, such as a whole call in place of a position inside it.
    pub coarsened: bool,
}

/// Why a conversion failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The rendering does not have a node with that id.
    NoSuchNode(String),
    /// The node is not of a kind the location asked for: a vertical position
    /// against something that is not a block, for instance.
    WrongKind(String),
    /// The node has no source position. Generated content and content from a
    /// file that is not being annotated are addressable in the rendering but
    /// not in the document.
    NoSource(String),
    /// The text the position referred to has changed since the rendering was
    /// made.
    Lost,
    /// The position is in a place where a label cannot be written and there is
    /// no enclosing expression to annotate instead.
    Unwritable,
}

/// What a conversion needs: the rendering it was taken against, and the
/// document as it stands.
pub struct Context<'a> {
    /// The map of the rendering the location refers to.
    pub map: &'a RenderMap,
    /// The document's text at the time of that rendering.
    pub was: &'a str,
    /// The document as it is now.
    pub source: &'a Source,
    /// A value used to derive ids for new anchors.
    pub seed: u64,
}

impl Context<'_> {
    /// The offset in the current document that a source offset in the rendering
    /// corresponds to.
    fn migrate(&self, offset: usize) -> Result<usize, Failure> {
        if self.was == self.source.text() {
            return Ok(offset);
        }
        match Rebase::between(self.was, self.source.text()).at(offset) {
            Shift::At(offset) => Ok(offset),
            Shift::Lost => Err(Failure::Lost),
        }
    }

    /// Where a node's text position sits in the current document.
    fn offset_of_char(&self, uid: &str, char_at: usize) -> Result<usize, Failure> {
        let node = self
            .map
            .node(uid)
            .ok_or_else(|| Failure::NoSuchNode(uid.to_owned()))?;
        let (file, offset) = node
            .source_of(char_at)
            .ok_or_else(|| Failure::NoSource(uid.to_owned()))?;
        if file != 0 {
            // Only the document being annotated can take anchors; content from
            // an included file is addressable but not yet writable.
            return Err(Failure::NoSource(uid.to_owned()));
        }
        self.migrate(offset)
    }

    /// Where a node ends in the current document, which is where a label
    /// attaching to it goes.
    fn offset_after_node(&self, uid: &str) -> Result<usize, Failure> {
        let node = self
            .map
            .node(uid)
            .ok_or_else(|| Failure::NoSuchNode(uid.to_owned()))?;
        let range = node
            .range
            .ok_or_else(|| Failure::NoSource(uid.to_owned()))?;
        if range.file != 0 {
            return Err(Failure::NoSource(uid.to_owned()));
        }
        self.migrate(range.end)
    }

    /// Whether a node is a block, which a vertical position requires.
    fn is_block(&self, uid: &str) -> Result<bool, Failure> {
        let node = self
            .map
            .node(uid)
            .ok_or_else(|| Failure::NoSuchNode(uid.to_owned()))?;
        Ok(node.kind.is_block())
    }
}

/// An anchor at an offset: the one already there, or a new one to write.
struct Anchored {
    /// The label as a location refers to it, prefix included.
    label: String,
    /// Set when the anchor could not go where it was asked for and marks an
    /// enclosing expression instead.
    coarsened: bool,
}

fn anchor_at(ctx: &Context, offset: usize, used: &mut Vec<Edit>) -> Result<Anchored, Failure> {
    // Snap to a position where a label is valid.
    let (at, coarsened) = match anchor::place(ctx.source, offset) {
        Ok(at) => (at, false),
        Err(Refusal::InsideCode(range)) => (range.end, true),
        Err(Refusal::NotMarkup) => return Err(Failure::Unwritable),
    };

    // An anchor already at this position is shared rather than duplicated: a
    // Typst element carries at most one label.
    if let Some(anchor) = anchor::at_position(ctx.source, at) {
        return Ok(Anchored {
            label: anchor.name(),
            coarsened,
        });
    }
    // An anchor written earlier in this same conversion, for a span whose ends
    // resolve to the same place.
    if let Some(edit) = used.iter().find(|edit| edit.at == at) {
        let label = edit
            .text
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_owned();
        return Ok(Anchored {
            label,
            coarsened,
        });
    }

    let id = anchor::fresh_id(ctx.source, ctx.seed.wrapping_add(at as u64));
    let edit = anchor::insert(&id, at);
    used.push(edit.clone());
    Ok(Anchored {
        label: crate::anchor_label(&id),
        coarsened,
    })
}

/// Converts a location.
pub fn resolve(ctx: &Context, location: &HtmlLocation) -> Result<Resolution, Failure> {
    let mut edits = Vec::new();
    let mut coarsened = false;

    // A word or a horizontal position resolves to an anchor at an offset.
    let mut text_anchor = |uid: &str, char_at: usize| -> Result<String, Failure> {
        let offset = ctx.offset_of_char(uid, char_at)?;
        let anchored = anchor_at(ctx, offset, &mut edits)?;
        coarsened |= anchored.coarsened;
        Ok(anchored.label)
    };

    let location = match location {
        Location::Word { reference } => {
            // The label goes after the word, so the word is what it attaches to.
            let label = text_anchor(&reference.node, reference.end)?;
            Location::Word {
                reference: TypstWordRef { label },
            }
        }
        Location::Line { reference } => Location::Line {
            reference: TypstWordRef {
                label: text_anchor(&reference.node, reference.end)?,
            },
        },
        Location::Sentence { reference } => Location::Sentence {
            reference: TypstWordRef {
                label: text_anchor(&reference.node, reference.end)?,
            },
        },
        Location::PosH { reference } => {
            let label = text_anchor(&reference.node, reference.pos)?;
            // The label sits at the position, so the position is to its right.
            Location::PosH {
                reference: TypstTextCursorRef {
                    label,
                    side: HSide::Right,
                },
            }
        }
        Location::SpanH { begin, end } => {
            let from = text_anchor(&begin.node, begin.pos)?;
            let to = text_anchor(&end.node, end.pos)?;
            Location::SpanH {
                begin: TypstTextCursorRef {
                    label: from,
                    side: HSide::Right,
                },
                end: TypstTextCursorRef {
                    label: to,
                    side: HSide::Left,
                },
            }
        }
        Location::PosV { reference } => {
            if !ctx.is_block(&reference.node)? {
                return Err(Failure::WrongKind(reference.node.clone()));
            }
            let offset = ctx.offset_after_node(&reference.node)?;
            let anchored = anchor_at(ctx, offset, &mut edits)?;
            coarsened |= anchored.coarsened;
            Location::PosV {
                reference: TypstNodeCursorRef {
                    label: anchored.label,
                    side: reference.side,
                },
            }
        }
        Location::SpanV { begin, end } => {
            for uid in [&begin.node, &end.node] {
                if !ctx.is_block(uid)? {
                    return Err(Failure::WrongKind(uid.clone()));
                }
            }
            let from = ctx.offset_after_node(&begin.node)?;
            let to = ctx.offset_after_node(&end.node)?;
            let from = anchor_at(ctx, from, &mut edits)?;
            let to = anchor_at(ctx, to, &mut edits)?;
            coarsened |= from.coarsened || to.coarsened;
            Location::SpanV {
                begin: TypstNodeCursorRef {
                    label: from.label,
                    side: begin.side,
                },
                end: TypstNodeCursorRef {
                    label: to.label,
                    side: end.side,
                },
            }
        }
        // Whole things: the label goes after the element.
        other => {
            let (uid, rebuild): (&str, fn(TypstNodeRef) -> TypstLocation) = match other {
                Location::Raw { reference } => (&reference.node, |r| Location::Raw { reference: r }),
                Location::Para { reference } => {
                    (&reference.node, |r| Location::Para { reference: r })
                }
                Location::Item { reference } => {
                    (&reference.node, |r| Location::Item { reference: r })
                }
                Location::Block { reference } => {
                    (&reference.node, |r| Location::Block { reference: r })
                }
                Location::Opaque { reference } => {
                    (&reference.node, |r| Location::Opaque { reference: r })
                }
                Location::Math { reference } => {
                    (&reference.node, |r| Location::Math { reference: r })
                }
                Location::MathBlock { reference } => {
                    (&reference.node, |r| Location::MathBlock { reference: r })
                }
                Location::Link { reference } => {
                    (&reference.node, |r| Location::Link { reference: r })
                }
                Location::Svg { reference } => (&reference.node, |r| Location::Svg { reference: r }),
                _ => unreachable!("every location kind is handled"),
            };
            let offset = ctx.offset_after_node(uid)?;
            let anchored = anchor_at(ctx, offset, &mut edits)?;
            coarsened |= anchored.coarsened;
            rebuild(TypstNodeRef {
                label: anchored.label,
            })
        }
    };

    Ok(Resolution {
        location,
        edits,
        coarsened,
    })
}

/// Converts a location in the document into one against a rendering.
///
/// The reverse of [`resolve`], used to draw annotations: the sidecar says where
/// in the document an annotation points, and the page needs that expressed in
/// what it is showing. No anchors are created, since every reference names one
/// already.
pub fn project(ctx: &Context, location: &TypstLocation) -> Result<HtmlLocation, Failure> {
    use crate::render_map::NodeKind;
    // Which kinds of element each location may name. A paragraph and the word
    // that ends it both end where the label begins, so the kind decides.
    const BLOCKS: [NodeKind; 6] = [
        NodeKind::Block,
        NodeKind::Para,
        NodeKind::Item,
        NodeKind::MathBlock,
        NodeKind::Svg,
        NodeKind::Image,
    ];
    let word_ref = |label: &str| -> Result<crate::location::HtmlWordRef, Failure> {
        let (uid, at) = ctx.rendered_word(label)?;
        // The label follows the word, so the word ends where the label begins.
        let (beg, w) = ctx.word_before(uid, at);
        Ok(crate::location::HtmlWordRef {
            node: uid.to_owned(),
            beg,
            end: at - (at - beg - w.as_ref().map_or(0, |word| word.chars().count())),
            w,
        })
    };
    let cursor_ref = |cursor: &TypstTextCursorRef| -> Result<crate::location::HtmlTextCursorRef, Failure> {
        let (uid, at) = ctx.rendered_position(&cursor.label)?;
        Ok(crate::location::HtmlTextCursorRef {
            node: uid.to_owned(),
            pos: at,
            l: None,
            r: None,
        })
    };
    let node_ref = |label: &str,
                    kinds: &[crate::render_map::NodeKind]|
     -> Result<crate::location::HtmlNodeRef, Failure> {
        Ok(crate::location::HtmlNodeRef {
            node: ctx.rendered_node(label, kinds)?.to_owned(),
        })
    };
    // A region's label sits inside it; everything else follows the thing it
    // names. The kind is tried first and any region second, so an item
    // annotation lands on the list item rather than on the paragraph inside it,
    // and still lands somewhere if the document has changed shape.
    let region_ref = |label: &str,
                      want: NodeKind|
     -> Result<crate::location::HtmlNodeRef, Failure> {
        // The kind asked for, then any region: an annotation on an item lands
        // on the item rather than on the paragraph inside it, and still lands
        // somewhere if the document has changed shape.
        let node = ctx
            .rendered_region(label, &[want])
            .or_else(|_| ctx.rendered_region(label, &BLOCKS))?;
        Ok(crate::location::HtmlNodeRef {
            node: node.to_owned(),
        })
    };
    let edge_ref = |edge: &TypstNodeCursorRef| -> Result<crate::location::HtmlNodeCursorRef, Failure> {
        Ok(crate::location::HtmlNodeCursorRef {
            node: ctx.rendered_node(&edge.label, &BLOCKS)?.to_owned(),
            side: edge.side,
        })
    };

    Ok(match location {
        Location::Word { reference } => Location::Word {
            reference: word_ref(&reference.label)?,
        },
        Location::Line { reference } => Location::Line {
            reference: word_ref(&reference.label)?,
        },
        Location::Sentence { reference } => Location::Sentence {
            reference: word_ref(&reference.label)?,
        },
        Location::PosH { reference } => Location::PosH {
            reference: cursor_ref(reference)?,
        },
        Location::PosV { reference } => Location::PosV {
            reference: edge_ref(reference)?,
        },
        Location::SpanH { begin, end } => Location::SpanH {
            begin: cursor_ref(begin)?,
            end: cursor_ref(end)?,
        },
        Location::SpanV { begin, end } => Location::SpanV {
            begin: edge_ref(begin)?,
            end: edge_ref(end)?,
        },
        Location::Raw { reference } => Location::Raw {
            reference: node_ref(&reference.label, &[NodeKind::Raw])?,
        },
        Location::Para { reference } => Location::Para {
            reference: region_ref(&reference.label, NodeKind::Para)?,
        },
        Location::Item { reference } => Location::Item {
            reference: region_ref(&reference.label, NodeKind::Item)?,
        },
        Location::Block { reference } => Location::Block {
            reference: region_ref(&reference.label, NodeKind::Block)?,
        },
        Location::Opaque { reference } => Location::Opaque {
            reference: node_ref(&reference.label, &[NodeKind::Inline])?,
        },
        Location::Math { reference } => Location::Math {
            reference: node_ref(&reference.label, &[NodeKind::Math])?,
        },
        Location::MathBlock { reference } => Location::MathBlock {
            reference: node_ref(&reference.label, &[NodeKind::MathBlock])?,
        },
        Location::Link { reference } => Location::Link {
            reference: node_ref(&reference.label, &[NodeKind::Link])?,
        },
        Location::Svg { reference } => Location::Svg {
            reference: node_ref(&reference.label, &[NodeKind::Svg])?,
        },
    })
}

impl Context<'_> {
    /// Where an anchor sits in the document.
    fn anchor_offset(&self, label: &str) -> Result<usize, Failure> {
        let id = crate::anchor_id(label).unwrap_or(label);
        anchor::find(self.source, id)
            .map(|anchor| anchor.at())
            .ok_or_else(|| Failure::NoSuchNode(label.to_owned()))
    }

    /// Where an anchor sits in the rendering: the run it is in, and how many
    /// characters into that run.
    fn rendered_position(&self, label: &str) -> Result<(&str, usize), Failure> {
        let offset = self.anchor_offset(label)?;
        self.map
            .text_at(0, offset)
            .ok_or_else(|| Failure::NoSource(label.to_owned()))
    }

    /// The run holding the word an anchor names.
    ///
    /// A label may be written with a space before it — `annotated <anno.X001>`
    /// — and that space is a run of its own, one character long. The word is in
    /// the run before it, so the whitespace is stepped back over before the run
    /// is looked up.
    fn rendered_word(&self, label: &str) -> Result<(&str, usize), Failure> {
        let mut at = self.anchor_offset(label)?;
        let text = self.source.text();
        while let Some(prev) = text[..at].chars().next_back() {
            if !prev.is_whitespace() {
                break;
            }
            at -= prev.len_utf8();
        }
        self.map
            .text_at(0, at)
            .ok_or_else(|| Failure::NoSource(label.to_owned()))
    }

    /// The region an anchor sits in, in the rendering.
    ///
    /// A region's label may be written anywhere inside it, so the region is
    /// found by containment first; a label written just past the end of one —
    /// which is where a paragraph's goes, since the text ends there — is
    /// matched by what it follows.
    fn rendered_region(
        &self,
        label: &str,
        kinds: &[crate::render_map::NodeKind],
    ) -> Result<&str, Failure> {
        let offset = self.anchor_offset(label)?;
        self.map
            .node_containing(0, offset, kinds)
            .ok_or(())
            .or_else(|()| self.rendered_node(label, kinds).map_err(|_| ()))
            .map_err(|()| Failure::NoSource(label.to_owned()))
    }

    /// The element an anchor attaches to, in the rendering.
    ///
    /// A label may be written with whitespace between it and the thing it
    /// names — `== A heading <anno.H001>` — which Typst allows and which leaves
    /// the element ending short of the label. Whitespace is stepped back over;
    /// anything else is a different element.
    fn rendered_node(
        &self,
        label: &str,
        kinds: &[crate::render_map::NodeKind],
    ) -> Result<&str, Failure> {
        let offset = self.anchor_offset(label)?;
        let text = self.source.text();
        let mut at = offset;
        loop {
            if let Some(uid) = self.map.node_ending_at(0, at, kinds) {
                return Ok(uid);
            }
            let Some(prev) = text[..at].chars().next_back() else {
                break;
            };
            if !prev.is_whitespace() {
                break;
            }
            at -= prev.len_utf8();
        }
        Err(Failure::NoSource(label.to_owned()))
    }

    /// The word ending at a position in a run: where it starts, and what it
    /// says.
    fn word_before(&self, uid: &str, at: usize) -> (usize, Option<String>) {
        let Some(node) = self.map.node(uid) else {
            return (at, None);
        };
        // The run's text is not held in the map, so the word is read from the
        // source instead, through the segment the position falls in.
        let Some((_, offset)) = node.source_of(at) else {
            return (at, None);
        };
        let text = self.source.text();
        // A label may be written with a space before it, which is not part of
        // the word it names.
        let mut end = offset;
        while end > 0 && text[..end].ends_with(char::is_whitespace) {
            end -= text[..end].chars().next_back().map_or(0, char::len_utf8);
        }
        let start = anchor::word_start(text, end);
        let word = text[start..end].to_owned();
        let skipped = text[end..offset].chars().count();
        let back = word.chars().count() + skipped;
        (
            at.saturating_sub(back),
            (!word.is_empty()).then_some(word),
        )
    }
}
