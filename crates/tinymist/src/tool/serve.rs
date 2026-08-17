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

pub mod annotations;
mod cache;
pub mod capture;
pub mod pins;
pub mod renders;
pub mod http;
pub mod sidecar;
pub mod snippet;

pub use crate::tool::registry::{
    announce_server, running_servers, slug_for, withdraw_server, ServerNote,
};

pub use annotations::{
    dev_asset, sidecar_path, AnchorPolicy, AnnotateRequest, AnnotationRecord, AnnotationServer,
    DiskAnnotationServer, SourceBlock,
};
pub use tinymist_annos::ANCHOR_PREFIX;


pub use http::{make_http_server, HttpServer};

/// How long the annotation endpoints wait before doing anything, in
/// milliseconds.
///
/// Zero unless a server was started with a latency. A page holds an annotation
/// on screen itself while the server has it and has not sent it back yet, and
/// on a machine that answers in ten milliseconds that state cannot be looked
/// at; this makes it last as long as it needs to be seen.
static ANNOTATE_LATENCY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Sets the delay the annotation endpoints answer with.
pub fn set_annotate_latency(ms: u64) {
    ANNOTATE_LATENCY.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// The delay the annotation endpoints answer with.
pub fn annotate_latency() -> u64 {
    ANNOTATE_LATENCY.load(std::sync::atomic::Ordering::Relaxed)
}

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
    /// Diagnostics, annotation and asset versions, pushed to the page.
    pub diag_rx: crate::tool::preview::DiagRx,
    /// Reading and writing its annotations.
    pub annot: Arc<dyn annotations::AnnotationServer>,
    /// The document as labelled HTML.
    pub body: Arc<dyn crate::tool::render::html::HtmlBody>,
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

/// What a path under a site names.
///
/// A directory holds documents, other files, and more directories, and a path
/// can be any of them: `report/report` is a document, `report/plot.png` is a
/// file to hand over as it is, `report` is a directory to list. Only the site
/// knows which, since only the site knows what is on disk.
#[derive(Debug, Clone)]
pub enum Located {
    /// A document, and what is being asked of it: `/dev/html/doc`, or nothing
    /// for the page itself.
    Document {
        /// Its name, which is its path under the directory without `.typ`.
        slug: String,
        /// The rest of the request.
        tail: String,
    },
    /// A file to serve as it is: a PDF, an image, a note in Markdown. A
    /// directory of documents is also the place their pictures live.
    File(PathBuf),
    /// A directory, which is a listing.
    Listing(PathBuf),
    /// Nothing of this site's.
    Missing,
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

    /// The annotation sidecars of everything this site serves, for asking
    /// whether anyone is in the middle of something. A read of a few files,
    /// never a compile: it is asked while deciding whether to shut down.
    fn sidecars(&self) -> Vec<PathBuf> {
        vec![]
    }

    /// What a path names: a document, a file, a directory, or nothing.
    ///
    /// A site of one document is asked nothing about paths: everything under
    /// it is that document's.
    fn locate(&self, rest: &str) -> Located {
        Located::Document {
            slug: String::new(),
            tail: rest.to_owned(),
        }
    }

    /// The services for one document, built if this is the first time it has
    /// been asked for. The empty name is the single-document case.
    fn services<'a>(
        &'a self,
        slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>>;
}

/// Something to do on the way out, whatever asks the server to stop.
///
/// A guard on the stack runs when the stack unwinds, and a process killed with
/// a signal does not unwind. This is where anything that must happen anyway
/// goes: taking down a proxy, clearing a cache.
static ON_SHUTDOWN: std::sync::Mutex<Vec<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(Vec::new());

/// Adds something to do on the way out.
pub fn at_shutdown(task: impl FnOnce() + Send + 'static) {
    if let Ok(mut held) = ON_SHUTDOWN.lock() {
        held.push(Box::new(task));
    }
}

/// What a server does on its way out, however it was asked: withdraw its note
/// from the register, take down anything it put up, clear up what it rendered,
/// and go.
pub fn shutdown() -> ! {
    run_shutdown_tasks();
    std::process::exit(0);
}

/// The same, without exiting: for a caller that is on its way out anyway.
pub fn run_shutdown_tasks() {
    if let Some(port) = crate::tool::registry::my_port() {
        crate::tool::registry::withdraw_server(port);
    }
    if let Ok(mut held) = ON_SHUTDOWN.lock() {
        for task in held.drain(..) {
            task();
        }
    }
    cache::drop_site_cache();
}

/// The extensions a directory hands over as they are.
///
/// A document's pictures, the PDF it was exported to, the notes beside it: a
/// directory of documents is where those live, and a reader following a link
/// to one is not asking for anything to be compiled.
pub const ASSET_EXTENSIONS: [&str; 12] = [
    "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg", "md", "txt", "csv", "json", "html",
];

/// Whether a path is one of those.
pub fn is_asset(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ASSET_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Whether a directory holds anything worth listing, at any depth.
///
/// A directory of source, or of build output, is not a directory of documents,
/// and a listing full of them is a listing nobody can read. Bounded: a deep
/// tree is answered by the first document in it.
pub fn holds_documents(dir: &Path, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut dirs = vec![];
    for entry in read.flatten() {
        let path = entry.path();
        if hidden(&path) {
            continue;
        }
        if path.is_dir() {
            dirs.push(path);
        } else if path.extension().is_some_and(|ext| ext == "typ") && !is_sidecar(&path) {
            return true;
        }
    }
    dirs.iter().any(|dir| holds_documents(dir, depth - 1))
}

/// Whether a name is one to leave alone: dotfiles, and the directories that
/// hold what a build left behind.
fn hidden(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "__pycache__")
}

/// The directories under a directory that hold documents somewhere below.
pub fn dirs_in(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<String> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && !hidden(path))
        .filter(|path| holds_documents(path, 6))
        .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
        .collect();
    found.sort();
    found
}

/// One file a directory serves as it is.
pub struct AssetEntry {
    /// Its name, as it sits in the directory.
    pub file: String,
    /// When it was last written.
    pub modified: Option<std::time::SystemTime>,
}

/// The files under a directory that are served as they are.
pub fn assets_in(dir: &Path) -> Vec<AssetEntry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<AssetEntry> = read
        .flatten()
        .map(|entry| entry.path())
        // A sidecar is not a file to read: it is what a document is annotated
        // with, and it belongs to that document.
        .filter(|path| path.is_file() && !hidden(path) && is_asset(path) && !is_sidecar(path))
        .filter_map(|path| {
            Some(AssetEntry {
                file: path.file_name()?.to_string_lossy().into_owned(),
                modified: std::fs::metadata(&path).and_then(|meta| meta.modified()).ok(),
            })
        })
        .collect();
    found.sort_by(|one, two| one.file.cmp(&two.file));
    found
}

/// How long ago something was written, in seconds.
fn age_of(when: Option<std::time::SystemTime>) -> u64 {
    when.and_then(|when| when.elapsed().ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Whether a listing reads the files it lists.
///
/// A document is read to find its title and to tell a document from a file of
/// helpers; a PDF could be read the same way. A few dozen files is a few dozen
/// small reads, which is nothing; a few thousand is a listing that takes a
/// moment, and the titles are not worth it.
static PREPARSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Says whether documents are read for their titles.
pub fn set_preparse(on: bool) {
    PREPARSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether they are.
pub fn preparse() -> bool {
    PREPARSE.load(std::sync::atomic::Ordering::Relaxed)
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
            // Not read at all: every `.typ` file is a document, and its name is
            // what it is called.
            if !preparse() {
                return Some(DocEntry {
                    title: slug.clone(),
                    slug,
                    modified: std::fs::metadata(&path).and_then(|meta| meta.modified()).ok(),
                    annotations: annotation_count(&path),
                    file,
                });
            }
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
    let entry = format!("<{}", tinymist_annos::ANCHOR_PREFIX);
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
    /// Where that document is, when this site is a served file rather than an
    /// editor's preview of one.
    pub path: Option<PathBuf>,
}

impl DocumentSite for SingleSite {
    fn sidecars(&self) -> Vec<PathBuf> {
        self.path
            .as_ref()
            .map(|path| vec![path.with_extension("annos.typ")])
            .unwrap_or_default()
    }

    fn services<'a>(
        &'a self,
        _slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>> {
        Box::pin(async move { Some(self.doc.clone()) })
    }
}

/// Whether any annotation in these sidecars is being worked on.
///
/// Claiming one is how an agent — or a person — says "I am in the middle of
/// this", and it is the one thing a server should outlive a closed window for.
/// Read rather than evaluated: this is asked on a timer, and the answer is a
/// word in a file.
pub fn anyone_working(sidecars: &[PathBuf]) -> bool {
    sidecars.iter().any(|path| {
        std::fs::read_to_string(path).is_ok_and(|text| {
            text.lines().any(|line| {
                // The prelude explains the field; it is not an entry.
                !line.trim_start().starts_with("//") && line.contains(r#"status: "ongoing""#)
            })
        })
    })
}

/// The listing page, read from the source tree when it is there, so it can be
/// edited without a rebuild — as the annotator's own script and stylesheet are.
/// The page holds no documents: it asks for them.
pub fn listing_html() -> String {
    crate::tool::asset::dev_asset_in(
        "serve",
        "listing.html",
        include_str!("../static/serve/listing.html"),
    )
}

/// The directory as the listing page asks for it.
pub fn listing_json(title: &str, dir: &Path, entries: &[DocEntry]) -> String {
    listing_json_of(title, dir, entries, &dirs_in(dir), &assets_in(dir), "")
}

/// The same, for a directory somewhere under the one being served: `under` is
/// the path from the root, which the page needs to make links with.
pub fn listing_json_of(
    title: &str,
    dir: &Path,
    entries: &[DocEntry],
    dirs: &[String],
    assets: &[AssetEntry],
    under: &str,
) -> String {
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
                "age": age_of(entry.modified),
            })
        })
        .collect();
    serde_json::json!({
        "ok": true,
        "title": title,
        "dir": dir.display().to_string(),
        // Where this listing sits under the directory being served, so the page
        // can say where it is and link out of it.
        "under": under,
        "docs": docs,
        "dirs": dirs,
        "files": assets
            .iter()
            .map(|asset| serde_json::json!({ "file": asset.file, "age": age_of(asset.modified) }))
            .collect::<Vec<_>>(),
    })
    .to_string()
}
