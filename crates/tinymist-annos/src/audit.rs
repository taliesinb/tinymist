//! What the document and the sidecar say about each other.
//!
//! The two can disagree in two directions, and they are not the same problem.
//! An anchor in the document that no annotation points at is litter: nothing is
//! lost by removing it, and something is gained, since anchors are text in a
//! file somebody has to read. An annotation pointing at an anchor the document
//! no longer has is the opposite — nothing is wrong with the annotation, its
//! subject has gone — and removing it silently would throw away the only record
//! that somebody once had something to say.
//!
//! So one is collected and the other is reported.

use crate::record::Annotation;
use crate::sidecar::Sidecar;

/// How a document and its sidecar stand with one another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audit {
    /// Anchors in the document that no annotation points at. Safe to remove.
    pub unused: Vec<String>,
    /// Anchors annotations point at that the document does not have. The
    /// annotations naming them have lost their subject.
    pub missing: Vec<String>,
}

impl Audit {
    /// Whether the two agree.
    pub fn is_clean(&self) -> bool {
        self.unused.is_empty() && self.missing.is_empty()
    }
}

/// Compares the anchors a document carries with the ones its annotations name.
///
/// The document's anchors are passed in rather than read here: finding them
/// means parsing Typst, which is the caller's business, and this crate holds
/// the model rather than the compiler.
pub fn audit<'a>(
    in_document: impl IntoIterator<Item = &'a str>,
    sidecar: &Sidecar,
) -> Audit {
    let mut present: Vec<String> = in_document.into_iter().map(str::to_owned).collect();
    present.sort();
    present.dedup();

    let used = sidecar.labels_in_use();

    Audit {
        unused: present
            .iter()
            .filter(|label| !used.contains(label))
            .cloned()
            .collect(),
        missing: used
            .iter()
            .filter(|label| !present.contains(label))
            .cloned()
            .collect(),
    }
}

/// The annotations that have lost their subject, with the anchor each is
/// missing.
///
/// For showing rather than for fixing: what to do about a dangling annotation
/// is a decision for whoever wrote it, and the useful thing a tool can do is
/// say which one it is and what it used to be about — which is what the
/// snapshot on each record is for.
pub fn dangling<'a>(sidecar: &'a Sidecar, audit: &Audit) -> Vec<(&'a Annotation, Vec<String>)> {
    if audit.missing.is_empty() {
        return vec![];
    }
    sidecar
        .annotations
        .iter()
        .filter_map(|anno| {
            let lost: Vec<String> = anno
                .labels()
                .into_iter()
                .filter(|label| audit.missing.iter().any(|gone| gone == label))
                .map(str::to_owned)
                .collect();
            (!lost.is_empty()).then_some((anno, lost))
        })
        .collect()
}
