//! The file an document's annotations live in, beside the document itself.
//!
//! JSON, and read as data rather than compiled: an annotation is a record with
//! a shape, and the shape is the Rust type. The sidecar it replaces was a Typst
//! file, which meant a compile to read one and template surgery to write one,
//! for the sake of being readable by `typst query` — a price that stopped being
//! worth paying the moment locations became objects with objects inside them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::record::Annotation;

/// The format the file is written in, so that a reader of a later one can say
/// so rather than guessing at fields that are not there.
pub const VERSION: u32 = 1;

/// The first field of every sidecar, saying what the file is.
///
/// A sidecar sits beside the document, in the same directory as build output
/// such as a PDF, and has been mistaken for build output and deleted. This is
/// what anyone opening the file reads first.
pub const NOTE: &str = "Annotations for companion .typ file; written by \
                        talimist; keep in git; do not delete; agents: use \
                        talimist mcp tool to edit.";

/// What a sidecar file holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// What this file is. Not read back: whatever the file on disk says here is
    /// replaced by [`NOTE`] when the file is next written.
    #[serde(rename = "_", default = "note")]
    pub note: String,
    /// The format version.
    pub version: u32,
    /// The annotations, in the order they were written.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

fn note() -> String {
    NOTE.to_owned()
}

impl Default for Sidecar {
    fn default() -> Self {
        Self {
            note: note(),
            version: VERSION,
            annotations: Vec::new(),
        }
    }
}

impl Sidecar {
    /// Reads a sidecar. A file that is not there is an empty one: a document
    /// with no annotations and a document nobody has annotated yet are the same
    /// thing to everyone who asks.
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
        };
        Self::parse(&text)
    }

    /// Reads a sidecar from text.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(text).map_err(|err| format!("cannot read the annotations: {err}"))
    }

    /// How the file is written: pretty, in field order, with a trailing
    /// newline.
    ///
    /// These files sit in a repository next to the documents they belong to, so
    /// what matters is that adding one reply is one hunk of diff. Field order
    /// is the order the types declare, which is why the records hold vectors
    /// rather than maps — a map would reorder itself and rewrite the file.
    pub fn to_json(&self) -> String {
        // Written fresh every time, so a file that lost the note — edited by
        // hand, or written by a build that predates it — gets it back.
        let mut said = self.clone();
        said.note = note();
        let mut text = serde_json::to_string_pretty(&said).unwrap_or_else(|_| "{}".to_owned());
        text.push('\n');
        text
    }

    /// Writes a sidecar, by way of a temporary file in the same directory: a
    /// reader that arrives mid-write sees the old file or the new one, never
    /// half of either.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = self.to_json();
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|err| format!("cannot make {}: {err}", dir.display()))?;
        let temp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|ext| ext.to_str()).unwrap_or("json")
        ));
        std::fs::write(&temp, &text).map_err(|err| format!("cannot write {}: {err}", temp.display()))?;
        std::fs::rename(&temp, path)
            .map_err(|err| format!("cannot put {} in place: {err}", path.display()))
    }

    /// An annotation by uuid.
    pub fn find(&self, uuid: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|anno| anno.uuid == uuid)
    }

    /// An annotation by uuid, to change.
    pub fn find_mut(&mut self, uuid: &str) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|anno| anno.uuid == uuid)
    }

    /// Adds an annotation, or replaces the one that has its uuid.
    pub fn put(&mut self, anno: Annotation) {
        match self.annotations.iter_mut().find(|old| old.uuid == anno.uuid) {
            Some(slot) => *slot = anno,
            None => self.annotations.push(anno),
        }
    }

    /// Removes an annotation, returning whether there was one.
    pub fn remove(&mut self, uuid: &str) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|anno| anno.uuid != uuid);
        self.annotations.len() != before
    }

    /// The letter a new annotation should carry: the first one nothing else is
    /// using.
    ///
    /// Assigned here rather than by whoever asked for the annotation, so that
    /// two clients composing at once cannot both think they are `c`. A letter
    /// stays with its annotation for as long as the annotation exists — it is
    /// how a person refers to one out loud — so this fills gaps rather than
    /// counting up.
    pub fn next_letter(&self) -> String {
        let taken: Vec<&str> = self
            .annotations
            .iter()
            .map(|anno| anno.letter.as_str())
            .collect();
        (0..).map(letter_at).find(|letter| !taken.iter().any(|used| used == letter))
            .unwrap_or_else(|| "a".to_owned())
    }

    /// Every anchor any annotation depends on.
    ///
    /// What garbage collection compares the document against: a label in the
    /// document that appears in no annotation's location is an anchor nothing
    /// needs, and can go.
    pub fn labels_in_use(&self) -> Vec<String> {
        let mut labels: Vec<String> = self
            .annotations
            .iter()
            .flat_map(|anno| anno.labels())
            .map(|label| label.to_owned())
            .collect();
        labels.sort();
        labels.dedup();
        labels
    }
}

/// The nth letter: `a` to `z`, then `aa`, `ab`, and so on. A document with
/// more than twenty-six annotations is unusual but not wrong.
fn letter_at(mut n: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'a' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "a".to_owned())
}

/// Where a document's annotations are kept: beside it, under its own name.
///
/// `docs/math.typ` is annotated in `docs/math.annos.json`, so the two travel
/// together — copied, moved and committed as one thing — and a directory of
/// documents reads as a directory of documents rather than as a store with
/// files scattered through it.
pub fn sidecar_path(document: &Path) -> PathBuf {
    let stem = document
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_owned());
    document.with_file_name(format!("{stem}.annos.json"))
}

/// Whether a path is a sidecar rather than a document.
pub fn is_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".annos.json"))
}

#[cfg(test)]
mod guard_tests {
    use super::Sidecar;

    /// A sidecar that cannot be read is an error, not an empty one. Whatever is
    /// about to write it back would otherwise write nothing over everything.
    #[test]
    fn a_file_that_cannot_be_read_says_so() {
        assert!(Sidecar::parse("{ this is not json").is_err());
        // Empty is empty, which is a file nobody has written to yet.
        assert_eq!(Sidecar::parse("  ").expect("empty").annotations.len(), 0);
    }

    /// A capture written before captures had ids is still one a scribble can
    /// name, and the annotations around it still load.
    #[test]
    fn a_capture_without_an_id_keeps_its_name() {
        let held = concat!(
            r#"{"_": "note", "version": 1, "annotations": [{"#,
            r#""uuid": "u1", "letter": "a","#,
            r#""location": {"type": "svg", "ref": {"type": "node", "ref": "anno.A1"}},"#,
            r#""type": "comment", "color": "white", "author": "tali","#,
            r#""time": "2026-01-01T00:00:00Z", "mtime": "2026-01-01T00:00:00Z","#,
            r#""claimed": false, "resolved": false, "content": "said","#,
            r#""captures": [{"time": "2026-01-01T00:00:00Z", "fmt": "svg","#,
            r#""hash": "abc123", "width": 10, "height": 10}]}]}"#,
        );
        let sidecar = Sidecar::parse(held).expect("an older sidecar still reads");
        let capture = &sidecar.annotations[0].captures[0];
        assert_eq!(capture.name(), "abc123");
    }
}
