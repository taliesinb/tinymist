//! Annotations on Typst documents: what they say, and where they point.
//!
//! An annotation is a remark about a place in a document — a question about a
//! sentence, a request to redraw a diagram — kept beside the document rather
//! than inside it. The document carries only anchors: bare labels that move
//! with the text they are written next to. Everything else lives here, in a
//! sidecar file that says which anchor each annotation points at and what kind
//! of place that is.
//!
//! The split is what makes several annotations on one word possible. A Typst
//! element carries at most one label, so a scheme where each annotation writes
//! its own label into the document can only ever have one of them per place —
//! the second silently replaces the first. An anchor that belongs to nobody in
//! particular can be pointed at by as many annotations as like.
//!
//! This crate deliberately knows nothing about compiling, serving or drawing.
//! It is the model and the file format, so that the tools which do those things
//! agree about what an annotation is.

#![deny(missing_docs)]

pub mod anchor;
pub mod audit;
pub mod location;
pub mod migrate;
pub mod record;
pub mod render_map;
pub mod resolve;
pub mod store;
pub mod sidecar;

pub use anchor::{anchors_in, anchors_in_text, Anchor, Edit, Refusal};
pub use audit::{audit, dangling, Audit};
pub use location::{
    HSide, Html, HtmlLocation, HtmlNodeCursorRef, HtmlNodeRef, HtmlTextCursorRef, HtmlWordRef,
    Location, Typst, TypstLocation, TypstNodeCursorRef, TypstNodeRef, TypstTextCursorRef,
    TypstWordRef, VSide, CONTEXT_CHARS,
};
pub use migrate::{Rebase, Shift};
pub use record::{Annotation, Capture, Reply};
pub use render_map::{FileEntry, NodeEntry, NodeKind, RenderMap, Segment, SrcRange};
pub use resolve::{project, resolve, Context, Failure, Resolution};
pub use sidecar::{is_sidecar, sidecar_path, Sidecar, VERSION};
pub use store::{resolve_offset, Resolved, StoredRender, Store};

/// What every anchor is named after, so that one plain search — no regular
/// expression — finds every anchor in a document: `<anno.`.
///
/// A leading `.` would have been shorter, but Typst does not accept it in a
/// label: `<.7C42>` is not an anchor, it is text, and it renders as text.
pub const ANCHOR_PREFIX: &str = "anno.";

/// The label an anchor id goes under in the document: `anno.7C42`.
pub fn anchor_label(id: &str) -> String {
    format!("{ANCHOR_PREFIX}{id}")
}

/// The anchor id inside a label, if the label is one of ours.
pub fn anchor_id(label: &str) -> Option<&str> {
    label.strip_prefix(ANCHOR_PREFIX)
}

#[cfg(test)]
mod tests;
