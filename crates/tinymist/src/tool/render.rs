//! Turning a compiled document into something a browser can show.
//!
//! Two renderings, one document: pages drawn as SVG, which is what a paper
//! looks like, and HTML, which is faster and reflows. Neither is tied to who
//! asked for it — an editor's previewer and a standalone document server both
//! want either — so they live here rather than inside one of them.
//!
//! Paged rendering is `tinymist-preview`'s, driven over a websocket; this
//! crate's part of it is the compile view in `tool/preview`. HTML is here.

pub mod html;
