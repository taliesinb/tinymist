//! Serving documents over HTTP: one document, or a directory of them.
//!
//! This is the CLI's side of the web server, and not the editor's: an LSP
//! preview is one document, opened by an editor, and has no use for a listing.
//! The HTTP layer itself is shared with it — one server, one set of endpoints —
//! so what a request is *about* is asked of a site, and the editor's preview is
//! a site of exactly one nameless document.
//!
//!
//! A server used to be one document from top to bottom — one compile pipeline,
//! one set of endpoints, one page. A directory is the same thing several times
//! over, so the HTTP layer stops holding a document's services directly and
//! asks a site for them by name instead. One document is then a site with one
//! nameless entry, and nothing about it is a special case.
//!
//! Documents are built when they are first asked for. A directory of thirty
//! papers is thirty compilers if they all start at once, and a reader opens
//! one.

mod cache;
pub mod hub;
pub mod mcp;
pub mod registry;

pub use registry::{
    announce_server, running_servers, slug_for, withdraw_server, ServerNote,
};

pub use cache::{
    drop_site_cache, site_cache, sweep_stale_sites, temp_site_dir, use_site_cache, CachedBody,
    SiteCache,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The services one document answers with: its annotations, its HTML, and the
/// stream that tells a page when either changed.
pub struct DocServices {
    /// The document's own name, for the window and the web app.
    pub title: String,
    /// Diagnostics, annotations and asset versions, pushed to the page.
    pub diag_rx: Option<crate::tool::preview::DiagRx>,
    /// Reading and writing annotations.
    pub annot: Option<Arc<dyn crate::tool::preview::AnnotationServer>>,
    /// The document as labelled HTML, in HTML mode.
    pub html: Option<Arc<dyn crate::tool::preview::html_annotations::HtmlAnnotationServer>>,
}

/// One document as a listing shows it: what it is called, where it is, and how
/// much has been said about it.
#[derive(Debug, Clone)]
pub struct DocEntry {
    /// The name in the URL: the file's stem.
    pub slug: String,
    /// The title the document gives itself, or its stem.
    pub title: String,
    /// The file name, as it sits in the directory.
    pub file: String,
    /// When it was last written.
    pub modified: Option<std::time::SystemTime>,
    /// How many annotations its sidecar holds.
    pub annotations: usize,
}

/// A document server's contents, addressed by name.
pub trait DocumentSite: Send + Sync + 'static {
    /// Whether this site is a directory, and so has a listing to show at its
    /// front page. Cheap: it is asked on every request.
    fn is_listing(&self) -> bool {
        false
    }

    /// The directory being served, for a listing to say where it is looking.
    fn root(&self) -> &Path {
        Path::new("")
    }

    /// The listing, read fresh: a document added to the directory while the
    /// server runs is a document the next reader sees.
    fn listing(&self) -> Vec<DocEntry> {
        vec![]
    }

    /// The services for one document, built if this is the first time it has
    /// been asked for. The empty name is the single-document case.
    fn services<'a>(
        &'a self,
        slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>>;
}

/// What a server does on its way out, however it was asked: withdraw its note
/// from the register, clear up what it rendered, and go.
pub fn shutdown() -> ! {
    if let Some(port) = registry::my_port() {
        registry::withdraw_server(port);
    }
    cache::drop_site_cache();
    std::process::exit(0);
}

/// The `.typ` files directly under a directory, sorted, as listing entries.
pub fn entries_in(dir: &Path) -> Vec<DocEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<PathBuf> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "typ"))
        // A sidecar is not a document: it is what one of them is annotated
        // with, and it has no reader of its own.
        .filter(|path| !is_sidecar(path))
        .collect();
    found.sort();
    found
        .into_iter()
        .filter_map(|path| {
            let file = path.file_name()?.to_string_lossy().into_owned();
            let slug = path.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).ok()?;
            // A directory of documents usually holds a few files that are not
            // documents: a style file, a library of helpers, a list of
            // definitions something else imports. They render nothing, and a
            // listing that offers them offers blank pages.
            //
            // A heading, a figure or a block equation is the cheap sign of a
            // document meant to be read. Not a proof — a document can open with
            // a paragraph — but reading the file is all it costs, and the
            // alternative is compiling every file in the directory to draw a
            // list of names.
            if !has_heading(&text) {
                return None;
            }
            let title = title_in(&text).unwrap_or_else(|| slug.clone());
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            Some(DocEntry {
                annotations: annotation_count(&path),
                slug,
                title,
                file,
                modified,
            })
        })
        .collect()
}

/// Whether the source looks like something to read: a heading, a figure, or a
/// block equation, each recognised at the start of a line where it is markup
/// rather than part of an expression.
pub fn has_heading(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        if line.starts_with("#figure(") || line.starts_with('$') {
            return true;
        }
        let rest = line.trim_start_matches('=');
        rest.len() < line.len() && rest.starts_with(' ') && !rest.trim().is_empty()
    })
}

/// Whether a path is an annotation sidecar, e.g. `paper.annos.typ`.
pub fn is_sidecar(path: &Path) -> bool {
    path.file_stem()
        .map(|stem| Path::new(stem).extension().is_some_and(|ext| ext == "annos"))
        .unwrap_or(false)
}

/// How many annotations a document's sidecar holds, counted rather than
/// evaluated: a listing is a listing, and compiling every sidecar to draw one
/// would be absurd.
fn annotation_count(doc: &Path) -> usize {
    let sidecar = doc.with_extension("annos.typ");
    let Ok(text) = std::fs::read_to_string(sidecar) else {
        return 0;
    };
    let entry = format!("<{}", crate::tool::preview::ANCHOR_PREFIX);
    text.match_indices(&entry)
        // The prelude shows what an entry looks like; the example in it is not
        // an annotation.
        .filter(|(at, _)| {
            let line_start = text[..*at].rfind('\n').map(|i| i + 1).unwrap_or(0);
            !text[line_start..*at].contains("//")
        })
        .count()
}

/// The title a document announces, taken from `#set document(title:)` or from
/// its first heading. A parse rather than a compile, for the same reason.
pub fn document_title(path: &Path) -> Option<String> {
    title_in(&std::fs::read_to_string(path).ok()?)
}

/// The same, from source already read.
pub fn title_in(text: &str) -> Option<String> {
    if let Some(at) = text.find("#set document(") {
        let rest = &text[at..];
        if let Some(key) = rest.find("title:") {
            let after = &rest[key + "title:".len()..];
            if let Some(open) = after.find('"') {
                if let Some(close) = after[open + 1..].find('"') {
                    let title = after[open + 1..open + 1 + close].trim();
                    if !title.is_empty() {
                        return Some(title.to_owned());
                    }
                }
            }
        }
    }
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("= ") else {
            continue;
        };
        let title: String = rest
            .chars()
            .take_while(|c| !matches!(c, '[' | '$' | '<' | '#' | '@'))
            .filter(|c| c.is_ascii() && !c.is_control())
            .collect();
        let title = title.trim();
        if !title.is_empty() {
            return Some(title.to_owned());
        }
    }
    None
}

/// A site of exactly one document, which is what a served file is.
pub struct SingleSite {
    /// The one document's services.
    pub doc: Arc<DocServices>,
}

impl DocumentSite for SingleSite {
    fn services<'a>(
        &'a self,
        _slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>> {
        Box::pin(async move { Some(self.doc.clone()) })
    }
}

/// The listing page, read from the source tree when it is there, so it can be
/// edited without a rebuild — as the annotator's own script and stylesheet are.
/// The page holds no documents: it asks for them.
pub fn listing_html() -> String {
    crate::tool::preview::dev_asset_in(
        "serve",
        "listing.html",
        include_str!("serve/listing.html"),
    )
}

/// The directory as the listing page asks for it.
pub fn listing_json(title: &str, dir: &Path, entries: &[DocEntry]) -> String {
    let docs: Vec<_> = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "slug": entry.slug,
                "title": entry.title,
                "file": entry.file,
                "annotations": entry.annotations,
                // Age rather than a stamp: the page says "3 h", and a clock
                // that disagrees with the server's would say it wrongly.
                "age": entry
                    .modified
                    .and_then(|when| when.elapsed().ok())
                    .map(|since| since.as_secs())
                    .unwrap_or(0),
            })
        })
        .collect();
    serde_json::json!({
        "ok": true,
        "title": title,
        "dir": dir.display().to_string(),
        "docs": docs,
    })
    .to_string()
}
