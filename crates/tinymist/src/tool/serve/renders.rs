//! Keeping the renderings a server has produced.
//!
//! A page refers to places in the rendering it fetched, by the ids the renderer
//! put in the HTML. The map that says what those ids mean is kept here, together
//! with the document's text at the time, so that a reference can still be read
//! after the document has changed or the server has restarted.

use std::path::{Path, PathBuf};

use tinymist_annos::render_map::RenderMap;
use tinymist_annos::store::{Store, StoredRender};

/// Where renderings are kept: beside the registry and the capture store.
pub fn renders_dir() -> PathBuf {
    crate::tool::registry::registry_dir()
        .parent()
        .map(|base| base.join("renders"))
        .unwrap_or_else(|| std::env::temp_dir().join("talimist").join("renders"))
}

/// The store holding one document's renderings.
pub fn store_for(document: &Path) -> Store {
    let canonical = std::fs::canonicalize(document).unwrap_or_else(|_| document.to_path_buf());
    let hash = crate::tool::render::html::hash_of(canonical.as_os_str().as_encoded_bytes());
    Store::for_document(&renders_dir(), &canonical, &hash)
}

/// The map of the most recent rendering, for the requests that need to convert
/// between the document and what a page is showing without reading from disk.
static LATEST: parking_lot::RwLock<Option<std::sync::Arc<RenderMap>>> =
    parking_lot::RwLock::new(None);

/// The map of the most recent rendering.
pub fn latest() -> Option<std::sync::Arc<RenderMap>> {
    LATEST.read().clone()
}

/// Records a rendering of a document.
///
/// `texts` is what each of the document's files said at the time, in the order
/// the map numbers them: a position taken against this rendering is translated
/// against the file it came from, which may be one the document includes.
pub fn record(document: &Path, map: RenderMap, texts: Vec<String>) {
    *LATEST.write() = Some(std::sync::Arc::new(map.clone()));
    let stored = StoredRender {
        map,
        texts,
        stored: tinymist_project::iso_now(),
    };
    if let Err(err) = store_for(document).put(&stored) {
        log::warn!("cannot keep the rendering of {}: {err}", document.display());
    }
}

/// The most recent rendering of a document, by id.
pub fn get(document: &Path, render: &str) -> Option<StoredRender> {
    store_for(document).get(render)
}
