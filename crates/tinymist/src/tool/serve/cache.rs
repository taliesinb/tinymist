//! The rendered site, written to disk as it is compiled.
//!
//! A served document is compiled once and read by everyone who asks for it, so
//! the rendering belongs on disk rather than in the answer to each request: one
//! file per document, rewritten when that document compiles, served with the
//! version it was written at. A browser that already has the current version
//! asks and is told nothing changed; several readers share one render; and what
//! the server thinks it produced can be looked at with `ls`.
//!
//! It is a cache, not a site: the pages are the annotator's fragments, which
//! need the server they came from. Publishing is a different output, and a
//! later one.
//!
//! One directory per process, under the system's temp directory, removed when
//! the server exits normally. A server that is killed leaves one behind, which
//! the next one with that process id will overwrite.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One document's rendered body, and the version it was rendered at.
#[derive(Debug, Clone)]
pub struct CachedBody {
    /// Where the body was written.
    pub path: PathBuf,
    /// Bumped on every write: the browser asks with it and is answered with
    /// nothing when it has not moved.
    pub version: u64,
}

/// The rendered site of one server.
#[derive(Debug)]
pub struct SiteCache {
    /// The directory everything is written under.
    dir: PathBuf,
    /// What has been written, by the document it was rendered from.
    written: parking_lot::Mutex<HashMap<PathBuf, CachedBody>>,
}

impl SiteCache {
    /// A cache under a directory of its own, made now.
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            written: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    /// Where the site is.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file a document's body is written to. Named after the document, and
    /// after enough of its path to tell two documents of the same name apart.
    fn body_path(&self, doc: &Path) -> PathBuf {
        let stem = doc
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".into());
        let stem: String = stem
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        // Two documents can share a name across directories; the path decides
        // which file is which, and a short digest of it is enough to say so
        // without rebuilding the directory tree here.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in doc.as_os_str().as_encoded_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        self.dir.join(format!("{stem}-{hash:016x}.html"))
    }

    /// Writes a document's rendered body, returning what it is now.
    ///
    /// A body that has not changed is not rewritten: a compile that produced
    /// the same page is not a reason for every reader to fetch it again.
    pub fn write_body(&self, doc: &Path, body: &str) -> Option<CachedBody> {
        let path = self.body_path(doc);
        let mut written = self.written.lock();
        if let Some(cached) = written.get(doc) {
            if std::fs::read_to_string(&cached.path).is_ok_and(|old| old == body) {
                return Some(cached.clone());
            }
        }
        std::fs::write(&path, body).ok()?;
        let version = written.get(doc).map(|it| it.version + 1).unwrap_or(1);
        let entry = CachedBody { path, version };
        written.insert(doc.to_path_buf(), entry.clone());
        Some(entry)
    }

    /// What was last written for a document, if anything was.
    pub fn body(&self, doc: &Path) -> Option<CachedBody> {
        self.written.lock().get(doc).cloned()
    }
}

impl Drop for SiteCache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The cache this process renders into, if it renders to disk at all. The
/// language server does not: it has one document, held by an editor, and
/// nothing to share it with.
static SITE: std::sync::RwLock<Option<Arc<SiteCache>>> = std::sync::RwLock::new(None);

/// Renders into `dir` from now on. The directory is emptied first: what is in
/// it is a previous run's, and stale.
pub fn use_site_cache(dir: PathBuf) -> std::io::Result<Arc<SiteCache>> {
    let _ = std::fs::remove_dir_all(&dir);
    let cache = Arc::new(SiteCache::new(dir)?);
    if let Ok(mut slot) = SITE.write() {
        *slot = Some(cache.clone());
    }
    Ok(cache)
}

/// The cache this process renders into.
pub fn site_cache() -> Option<Arc<SiteCache>> {
    SITE.read().ok().and_then(|slot| slot.clone())
}

/// Stops rendering to disk, and takes the site with it once the last holder of
/// it lets go. A global is never dropped on the way out, so the server says
/// when it is done rather than leaving that to the end of the process.
pub fn drop_site_cache() {
    if let Ok(mut slot) = SITE.write() {
        *slot = None;
    }
}

/// Where a server's rendered site goes: one directory per process, so two
/// servers of the same document do not write over each other.
pub fn temp_site_dir() -> PathBuf {
    std::env::temp_dir().join(temp_site_dir_name())
}

/// What a rendered site's directory is called, before the process id.
const SITE_PREFIX: &str = "talimist-site-";

/// Removes rendered sites left behind by servers that are gone.
///
/// A server takes its own with it when it exits; one that is killed cannot, so
/// the next server to start clears up. Age is the test rather than whether the
/// process that wrote it is alive: asking that portably costs a dependency or a
/// spawned process, and a site nothing has written to in a day belongs to
/// nobody. Being wrong only costs a rendering, which is remade on request.
pub fn sweep_stale_sites() {
    const A_DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(SITE_PREFIX) || name == temp_site_dir_name() {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|when| when.elapsed().ok())
            .is_some_and(|age| age > A_DAY);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// This process's own site directory name.
fn temp_site_dir_name() -> String {
    format!("{SITE_PREFIX}{}", std::process::id())
}
