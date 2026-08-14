//! Serving a directory: one server, one compiler per document in it.
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

use tinymist::project::ProjectPreviewState;
use tinymist::tool::preview::{PreviewCliArgs, ProjectPreviewHandler, html_annotations, icons};
use tinymist::tool::serve::{DocServices, DocumentSite};
use tinymist::tool::project::{ProjectOpts, StartProjectResult, start_project};
use tinymist_preview::{PreviewBuilder, PreviewConfig};
use tinymist_project::WorldProvider;
use tinymist_std::error::prelude::*;

/// A directory of documents, each built when it is first asked for.
pub struct DirSite {
    /// The directory being served.
    dir: PathBuf,
    /// The arguments every document in it is compiled with; only the input
    /// file differs.
    args: PreviewCliArgs,
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
    pub fn new(dir: PathBuf, args: PreviewCliArgs) -> Self {
        Self {
            dir,
            args,
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
            let (doc, previewer) = match build_document(&self.args, &path) {
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
pub fn build_document(
    args: &PreviewCliArgs,
    input: &Path,
) -> Result<(Arc<DocServices>, PreviewBuilder)> {
    let mut args = args.clone();
    args.compile.input = Some(input.display().to_string());
    let config = args.preview.config(&PreviewConfig::default());
    let export_target = args.preview.export_target();

    let mut verse = args.compile.resolve()?;
    // The same shims a single served document gets: what HTML export drops,
    // put back in front of the document rather than in it.
    let shims = match html_annotations::install_shims(&mut verse) {
        Ok(entry) => Some(entry),
        Err(err) => {
            log::warn!("serving without the HTML export shims: {err}");
            None
        }
    };

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
    let previewer = PreviewBuilder::new(config);
    if !preview_state.register(&id, previewer.compile_watcher(args.task_id.clone())) {
        bail!("failed to register {}", input.display());
    }

    let (diag_tx, diag_rx) =
        tokio::sync::watch::channel(tinymist::tool::preview::OverlayPayload::default());
    preview_state.register_diag(&id, diag_tx);

    let annot: Arc<dyn tinymist::tool::preview::AnnotationServer> =
        Arc::new(tinymist::tool::preview::DiskAnnotationServer {
            last_art: last_art.clone(),
            watchers: preview_state.clone(),
            project_id: id.clone(),
            emit_events: args.annotate,
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
                for path in html_annotations::asset_paths() {
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
                let Some(path) = tinymist::tool::preview::sidecar_path(&art) else {
                    continue;
                };
                let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if mtime == last_mtime {
                    continue;
                }
                last_mtime = mtime;
                let pins = tinymist::tool::preview::annotation_pins(&art);
                diag_tx.send_modify(|state| state.annotations = pins);
            }
        });
    }

    // A shim edit is not a compile dependency either — the wrapper holds a copy
    // of the text — so the wrapper is rewritten when the file changes, which is
    // what the compiler notices.
    if let Some(shims) = shims {
        let handle = handle.clone();
        tokio::spawn(async move {
            use tinymist_preview::EditorServer;
            let mut last = None;
            loop {
                let stamp = std::fs::metadata(html_annotations::shims_path())
                    .and_then(|meta| meta.modified())
                    .ok();
                if last.is_some() && stamp != last {
                    let files = tinymist_preview::MemoryFiles {
                        files: HashMap::from([(
                            shims.path.clone(),
                            html_annotations::wrapper_source(&shims.main_name),
                        )]),
                    };
                    let _ = handle.update_memory_files(files, false).await;
                }
                last = stamp;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }

    let html: Arc<dyn html_annotations::HtmlAnnotationServer> =
        Arc::new(html_annotations::ArtifactHtmlServer { last_art });

    tokio::spawn(service.run());

    let title = tinymist::tool::serve::document_title(input)
        .or_else(|| Some(input.file_stem()?.to_string_lossy().into_owned()))
        .unwrap_or_default();

    Ok((
        Arc::new(DocServices {
            title,
            diag_rx: Some(diag_rx),
            annot: Some(annot),
            html: Some(html),
        }),
        previewer,
    ))
}

/// The name a directory server calls itself, and the icon it wears.
pub fn identity_for(dir: &Path, role: icons::IconRole, color: Option<[u8; 3]>) -> tinymist::tool::preview::WebAppIdentity {
    tinymist::tool::preview::WebAppIdentity {
        role,
        color,
        name: dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    }
}

/// Serves a whole directory: the listing at the mode's own prefix, and each
/// document under its own name.
pub async fn serve_directory(
    args: PreviewCliArgs,
    dir: PathBuf,
    host: String,
    role: icons::IconRole,
    shutdown_on_last_client: bool,
    mcp: bool,
    ready: impl FnOnce(u16),
) -> Result<()> {
    if !matches!(args.preview.export_target(), tinymist_task::ExportTarget::Html) {
        bail!(
            "a directory is served as HTML: the paged renderer speaks to its page over a \
             websocket with no room in it to say which document. Name a file to serve it as pages."
        );
    }
    let identity = identity_for(&dir, role, args.icon_color.as_deref().and_then(icons::parse_hex));
    let allowed_origins = args.allowed_origins.clone();
    let addr = args.data_plane_host.clone();
    let site = Arc::new(DirSite::new(dir, args));

    // Nothing renders pages here, so the websocket channel has no other end;
    // the receiver is held so that an upgrade attempt is dropped rather than
    // failing loudly.
    let (websocket_tx, _websocket_rx) = tokio::sync::mpsc::unbounded_channel();
    let srv = tinymist::tool::preview::make_http_server(
        html_annotations::shell_html(),
        addr,
        websocket_tx,
        site,
        shutdown_on_last_client,
        mcp,
        identity,
        allowed_origins,
    )
    .await;
    let _ = host;
    ready(srv.addr.port());
    srv.join.await.ok();
    Ok(())
}
