//! Keeping renderings, so that positions taken against them can still be read.
//!
//! A render map is only useful while the rendering it describes can still be
//! referred to. A browser holds ids from the rendering it fetched, and may hold
//! them for a long time: a draft annotation written offline is stored with the
//! render it was composed against and submitted later, possibly after the server
//! has restarted.
//!
//! The store therefore keeps each rendering on disk, together with the text of
//! the document it was made from. The text is what makes a cold start work: with
//! it, the translation from the old positions to the current file can be derived
//! by comparison, without the chain of edits that produced the difference.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::migrate::{Rebase, Shift};
use crate::render_map::RenderMap;

/// How many renderings of one document are kept. Older ones are removed as new
/// ones arrive.
pub const KEEP: usize = 16;

/// A rendering as it is stored: the map, and the document it was made from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRender {
    /// The map of the rendering.
    pub map: RenderMap,
    /// The document's text at the time, used to translate positions taken
    /// against this rendering into the document as it is now.
    pub text: String,
    /// When it was stored, ISO 8601 UTC. Written by the caller, which knows the
    /// clock.
    #[serde(default)]
    pub stored: String,
}

/// Where renderings are kept.
///
/// Keyed by document rather than by server: a port belongs to a process and
/// changes between runs, whereas a rendering belongs to the document it is of.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store under `dir`, holding the renderings of one document.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory a document's renderings are kept in, under `base`.
    ///
    /// Named after the document's path so that two documents with the same file
    /// name do not share a directory.
    pub fn for_document(base: &Path, document: &Path, hash: &str) -> Self {
        let name = document
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_owned());
        Self::new(base.join(format!("{name}-{hash}")))
    }

    /// Where the store keeps its files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads a rendering by its id.
    pub fn get(&self, render: &str) -> Option<StoredRender> {
        let text = std::fs::read_to_string(self.path_of(render)?).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Writes a rendering, and removes the oldest if there are now too many.
    ///
    /// Writing the same rendering twice is not an error and does nothing: a
    /// document that compiles to what it compiled to before has the same render
    /// id, which is why the id is derived from the rendering.
    ///
    /// The id is derived from the HTML alone, though, and the map is not in the
    /// HTML: a renderer that learns to record something new produces the same
    /// id with a different map, and what is stored has to be replaced or the
    /// old map is what everything is answered from.
    pub fn put(&self, render: &StoredRender) -> Result<(), String> {
        let Some(path) = self.path_of(&render.map.render) else {
            return Err("a rendering with no id cannot be stored".into());
        };
        if let Some(known) = self.get(&render.map.render) {
            if known.map == render.map && known.text == render.text {
                return Ok(());
            }
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|err| format!("cannot make {}: {err}", self.dir.display()))?;
        let text = serde_json::to_string(render)
            .map_err(|err| format!("cannot write the rendering: {err}"))?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text)
            .map_err(|err| format!("cannot write {}: {err}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .map_err(|err| format!("cannot put {} in place: {err}", path.display()))?;
        self.prune();
        Ok(())
    }

    /// Every rendering held, oldest first by modification time.
    pub fn held(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                let when = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (when, path)
            })
            .collect();
        files.sort();
        files.into_iter().map(|(_, path)| path).collect()
    }

    /// Removes the oldest renderings beyond [`KEEP`].
    fn prune(&self) {
        let held = self.held();
        let excess = held.len().saturating_sub(KEEP);
        for path in held.into_iter().take(excess) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn path_of(&self, render: &str) -> Option<PathBuf> {
        if render.is_empty() || !render.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return None;
        }
        Some(self.dir.join(format!("{render}.json")))
    }
}

/// A position taken against a rendering, translated into the document as it is
/// now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The position in the current document.
    At(usize),
    /// The rendering is known, but the text this position was in has changed.
    Lost,
    /// The rendering is not held, so the position cannot be read at all.
    Unknown,
}

/// Translates a position from a rendering into the document as it is now.
///
/// `current` is the document's text as it stands. The rendering supplies the
/// text it was made from, which is compared with `current` to derive the
/// translation.
pub fn resolve_offset(store: &Store, render: &str, offset: usize, current: &str) -> Resolved {
    let Some(stored) = store.get(render) else {
        return Resolved::Unknown;
    };
    match Rebase::between(&stored.text, current).at(offset) {
        Shift::At(offset) => Resolved::At(offset),
        Shift::Lost => Resolved::Lost,
    }
}
