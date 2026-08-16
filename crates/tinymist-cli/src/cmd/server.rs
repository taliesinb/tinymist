//! Serving documents: a file, or a directory of them.
//!
//! A document server used to be one document from top to bottom. A directory
//! is the same thing several times over, and the only honest way to hold
//! several is to hold several: each document gets its own compiler, its own
//! annotations and its own HTML, and the HTTP layer asks the site for whichever
//! one a request is about.
//!
//! Documents are built the first time they are asked for. A directory of thirty
//! papers would otherwise be thirty compilers running to draw a list of names,
//! and a reader opens one of them.
//!
//! HTML only. The paged renderer talks to its page over a websocket that has no
//! room in it for saying *which* document, and a directory of documents is a
//! thing to read rather than a thing to watch an editor drive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tinymist::CompileOnceArgs;

use tinymist::project::ProjectPreviewState;
use tinymist::tool::preview::ProjectPreviewHandler;
use tinymist::tool::webapp::icons;
use tinymist::tool::render::html;
use tinymist::tool::serve::{DocServices, DocumentSite};
use tinymist::tool::project::{ProjectOpts, StartProjectResult, start_project};
use tinymist_preview::{PreviewBuilder, PreviewConfig};
use tinymist_project::WorldProvider;
use tinymist_std::error::prelude::*;

/// What a document server needs to compile the documents it serves.
///
/// Not the previewer's arguments: a served document is always HTML, is never
/// driven by an editor, and has no page renderer to configure. What is left is
/// where the project is and whether annotations are on.
#[derive(Debug, Clone)]
pub struct DocConfig {
    /// Where the project is, what to feed it, and which fonts to use.
    pub compile: CompileOnceArgs,
    /// Whether annotations are served: the endpoints, and the events an agent
    /// hears about.
    pub annotate: bool,
    /// Which origins a browser may reach this server from.
    pub allowed_origins: Vec<String>,
    /// The address the pages are served on.
    pub data_plane_host: String,
    /// What the web app calls itself, and which icon it wears.
    pub identity: tinymist::tool::webapp::WebAppIdentity,
    /// Whether the server goes when the last page does.
    pub shutdown_on_last_client: bool,
    /// Whether agents are answered at `/m/`.
    pub mcp: bool,
}

/// A directory of documents, each built when it is first asked for.
pub struct DirSite {
    /// The directory being served.
    dir: PathBuf,
    /// What every document in it is compiled with; only the input differs.
    cfg: DocConfig,
    /// What has been built so far, by file stem. A failed build is not cached:
    /// a document that does not compile today may compile once it is fixed,
    /// and the reader reloads.
    built: tokio::sync::Mutex<HashMap<String, Arc<DocServices>>>,
    /// Keeps each document's previewer alive: a compile artifact is only kept
    /// for a project that something is watching.
    watchers: parking_lot::Mutex<Vec<PreviewBuilder>>,
}

impl DirSite {
    /// A site for the `.typ` files directly under a directory.
    pub fn new(dir: PathBuf, cfg: DocConfig) -> Self {
        Self {
            dir,
            cfg,
            built: tokio::sync::Mutex::new(HashMap::new()),
            watchers: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// The file a name in the URL refers to, if it is one of ours. Names are
    /// file stems and nothing else: no separators, no `..`, so a URL cannot
    /// reach out of the directory it was answered from.
    fn path_of(&self, slug: &str) -> Option<PathBuf> {
        if slug.is_empty() || slug.contains(['/', '\\']) || slug.starts_with('.') {
            return None;
        }
        let path = self.dir.join(format!("{slug}.typ"));
        path.is_file().then_some(path)
    }
}

impl DocumentSite for DirSite {
    fn is_listing(&self) -> bool {
        true
    }

    fn root(&self) -> &Path {
        &self.dir
    }

    fn listing(&self) -> Vec<tinymist::tool::serve::DocEntry> {
        tinymist::tool::serve::entries_in(&self.dir)
    }

    fn sidecars(&self) -> Vec<PathBuf> {
        tinymist::tool::serve::entries_in(&self.dir)
            .into_iter()
            .map(|entry| self.dir.join(entry.file).with_extension("annos.typ"))
            .collect()
    }

    fn services<'a>(
        &'a self,
        slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>> {
        Box::pin(async move {
            let path = self.path_of(slug)?;
            let mut built = self.built.lock().await;
            if let Some(doc) = built.get(slug) {
                return Some(doc.clone());
            }
            log::info!("building {}", path.display());
            let (doc, previewer) = match build_document(&self.cfg, &path) {
                Ok(doc) => doc,
                Err(err) => {
                    log::error!("cannot serve {}: {err}", path.display());
                    return None;
                }
            };
            self.watchers.lock().push(previewer);
            built.insert(slug.to_owned(), doc.clone());
            Some(doc)
        })
    }
}

/// Builds one document's services: its compiler, its annotations, its HTML.
///
/// Returns the previewer alongside them, which the caller keeps: a compile
/// artifact is only stored for a project something is watching, so the watcher
/// registered here has to outlive this function.
pub fn build_document(cfg: &DocConfig, input: &Path) -> Result<(Arc<DocServices>, PreviewBuilder)> {
    let mut compile = cfg.compile.clone();
    compile.input = Some(input.display().to_string());
    // A served document is HTML: the paged renderer talks to its page over a
    // websocket that has no room in it to say which document, and a directory
    // of documents is a thing to read rather than a thing to watch an editor
    // drive.
    let export_target = tinymist_task::ExportTarget::Html;

    let mut verse = compile.resolve()?;
    // The same shims a single served document gets: what HTML export drops,
    // put back in front of the document rather than in it.
    if let Err(err) = html::install_shims(&mut verse) {
        log::warn!("serving without the HTML export shims: {err}");
    }

    let preview_state = ProjectPreviewState::default();
    let last_art = Arc::new(parking_lot::Mutex::default());
    let opts = ProjectOpts {
        handle: Some(tokio::runtime::Handle::current()),
        preview: preview_state.clone(),
        export_target,
        last_art: last_art.clone(),
        ..ProjectOpts::default()
    };

    let StartProjectResult {
        service,
        intr_tx,
        mut editor_rx,
    } = start_project(verse, Some(opts), |compiler, intr, next| next(compiler, intr));
    tokio::spawn(async move { while editor_rx.recv().await.is_some() {} });

    let id = service.compiler.primary.id.clone();
    // Watched, though nobody is rendering pages: the compile handler keeps an
    // artifact only for a project with a watcher, and the artifact is what the
    // HTML and the annotations are read from.
    let previewer = PreviewBuilder::new(PreviewConfig::default());
    if !preview_state.register(&id, previewer.compile_watcher("serve".to_owned())) {
        bail!("failed to register {}", input.display());
    }

    let (diag_tx, diag_rx) =
        tokio::sync::watch::channel(tinymist::tool::preview::OverlayPayload::default());
    preview_state.register_diag(&id, diag_tx);

    let annot: Arc<dyn tinymist::tool::serve::AnnotationServer> =
        Arc::new(tinymist::tool::serve::DiskAnnotationServer {
            last_art: last_art.clone(),
            watchers: preview_state.clone(),
            project_id: id.clone(),
            emit_events: cfg.annotate,
        });

    let handle: Arc<ProjectPreviewHandler> = Arc::new(ProjectPreviewHandler {
        project_id: id.clone(),
        client: Box::new(intr_tx),
    });

    // The sidecar is not a compile dependency, and neither are the client's own
    // assets; both are polled so that an edit to either reaches the page.
    {
        let watchers = preview_state.clone();
        let poll_id = id.clone();
        let poll_art = last_art.clone();
        tokio::spawn(async move {
            let mut last_mtime = None;
            let mut asset_mtime = Vec::new();
            let mut first = true;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let Some(diag_tx) = watchers.diag_tx(&poll_id) else {
                    break;
                };
                let mut stamps = Vec::new();
                for path in html::asset_paths() {
                    stamps.push(std::fs::metadata(path).and_then(|m| m.modified()).ok());
                }
                if stamps != asset_mtime {
                    asset_mtime = stamps;
                    if !first {
                        diag_tx.send_modify(|state| state.asset_version += 1);
                    }
                }
                first = false;
                let Some(art) = poll_art.lock().clone() else {
                    continue;
                };
                let Some(path) = tinymist::tool::serve::sidecar_path(&art) else {
                    continue;
                };
                let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if mtime == last_mtime {
                    continue;
                }
                last_mtime = mtime;
                diag_tx.send_modify(|state| state.anno_version += 1);
            }
        });
    }

    let body: Arc<dyn html::HtmlBody> = Arc::new(html::ArtifactHtmlServer { last_art });

    tokio::spawn(service.run());

    let title = tinymist::tool::serve::document_title(input)
        .or_else(|| Some(input.file_stem()?.to_string_lossy().into_owned()))
        .unwrap_or_default();

    Ok((
        Arc::new(DocServices {
            title,
            diag_rx,
            annot,
            body,
        }),
        previewer,
    ))
}


/// Serves a whole directory: the listing at the mode's own prefix, and each
/// document under its own name.
/// Serves a whole directory: the listing at the mode's own prefix, and each
/// document under its own name.
pub async fn serve_directory(cfg: DocConfig, dir: PathBuf, ready: impl FnOnce(u16)) -> Result<()> {
    let site = Arc::new(DirSite::new(dir, cfg.clone()));
    serve_site(cfg, site, ready).await
}

/// Serves one document, built before the first request rather than on it: the
/// person who asked for it is waiting at a window.
pub async fn serve_file(cfg: DocConfig, file: PathBuf, ready: impl FnOnce(u16)) -> Result<()> {
    let (doc, previewer) = build_document(&cfg, &file)?;
    // The previewer is what keeps compile artifacts alive; nothing else holds
    // it, so it rides along with the site.
    let site = Arc::new(FileSite {
        doc,
        path: file,
        _previewer: previewer,
    });
    serve_site(cfg, site, ready).await
}

/// One document, served under no name: `/a/` rather than `/a/paper/`.
struct FileSite {
    doc: Arc<DocServices>,
    path: PathBuf,
    _previewer: PreviewBuilder,
}

impl DocumentSite for FileSite {
    fn root(&self) -> &Path {
        &self.path
    }

    fn sidecars(&self) -> Vec<PathBuf> {
        vec![self.path.with_extension("annos.typ")]
    }

    fn services<'a>(
        &'a self,
        _slug: &'a str,
    ) -> futures::future::BoxFuture<'a, Option<Arc<DocServices>>> {
        Box::pin(async move { Some(self.doc.clone()) })
    }
}

/// The HTTP server both shapes share.
async fn serve_site(
    cfg: DocConfig,
    site: Arc<dyn DocumentSite>,
    ready: impl FnOnce(u16),
) -> Result<()> {
    crate::utils::tidy_up_on_signals();
    // Nothing renders pages here, so the websocket channel has no other end;
    // the receiver is held so that an upgrade attempt is dropped rather than
    // failing loudly.
    let (websocket_tx, _websocket_rx) = tokio::sync::mpsc::unbounded_channel();
    let srv = tinymist::tool::serve::make_http_server(
        html::shell_html(),
        cfg.data_plane_host.clone(),
        websocket_tx,
        site,
        cfg.shutdown_on_last_client,
        cfg.mcp,
        cfg.identity.clone(),
        cfg.allowed_origins.clone(),
    )
    .await;
    ready(srv.addr.port());
    srv.join.await.ok();
    Ok(())
}
