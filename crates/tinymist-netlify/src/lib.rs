//! Serving patched pages and annotations from a static site on Netlify.
//!
//! A site on Netlify is a set of files that were uploaded together, and it
//! cannot be changed a file at a time: every change is a whole new deploy, so
//! republishing one page of twenty thousand costs a deploy like any other. That
//! is fine for a build and wrong for a page somebody is reading and correcting
//! now.
//!
//! So a page that has been rebuilt since the last deploy is kept in a blob
//! instead, and the site is told about it in a list — the patchset — that a
//! page reads for itself when it has nothing better to do. There are three
//! pieces:
//!
//! - the patchset, one blob for the whole site, listing what has changed since
//!   the deploy and where the new page is. It is served by an edge function
//!   whose answer is the same for everybody, and so is cached at the edge and
//!   asked of a blob about once a minute rather than once a page.
//! - the pages themselves, a blob each, named by the hash of what is in them,
//!   and served by a function whose answer can therefore be cached forever.
//! - a script on the page, which asks for the patchset when the browser is
//!   idle and swaps the page's content if what it is showing is out of date.
//!
//! Annotations live in the same store, a blob per page, and are read and
//! written by a function of their own.
//!
//! Nothing here talks to Netlify: this crate holds what a site needs and knows
//! how to name and lay it out. What talks to Netlify is the deploy, which
//! uploads these files, and whatever publishes a patch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The blob store everything below lives in.
pub const STORE: &str = "talimist";

/// Where a page asks what has changed since the deploy.
pub const MANIFEST_ROUTE: &str = "/_talimist/patchset.json";

/// Where a page asks for one of those changed pages, by its hash.
pub const PAGE_ROUTE: &str = "/_talimist/page";

/// Where a page reads and writes its annotations.
pub const ANNOS_ROUTE: &str = "/_talimist/annotations";

/// What has changed since the site was last deployed.
///
/// One object for the whole site, and a small one: a deploy publishes
/// everything it knows and empties this, so it holds the work of the hours
/// since rather than the history of the wiki.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patchset {
    /// The deploy these patches are against. A page from an older deploy takes
    /// them; a page from a newer one is already ahead of them.
    pub build: String,
    /// The pages, by the path they are served at.
    #[serde(default)]
    pub pages: BTreeMap<String, Patch>,
}

/// One page that has been rebuilt since the deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    /// The hash of the page's content, which is also where it is stored and
    /// what the page it replaces compares itself against.
    pub hash: String,
    /// When it was published, ISO 8601 UTC.
    pub time: String,
}

impl Patchset {
    /// Records a page, replacing whatever was there for that path.
    pub fn put(&mut self, path: &str, hash: &str, time: &str) {
        self.pages.insert(
            path.to_owned(),
            Patch {
                hash: hash.to_owned(),
                time: time.to_owned(),
            },
        );
    }

    /// What the page at this path should be showing, if it is not what it is.
    pub fn wanted(&self, path: &str, showing: &str) -> Option<&Patch> {
        self.pages.get(path).filter(|patch| patch.hash != showing)
    }
}

/// Where a page's content is kept.
pub fn page_key(hash: &str) -> String {
    format!("page/{}", clean(hash))
}

/// Where a page's annotations are kept.
///
/// Keyed by the path the page is served at, since that is what both the reader
/// and the writer know. A key may be 600 bytes and a path may be longer than
/// that once it is escaped, so a long one is named by its hash instead — which
/// is the same for everybody who computes it, which is all that is asked of a
/// name.
pub fn annos_key(path: &str) -> String {
    let named = clean(path.trim_start_matches('/'));
    if named.len() <= 500 {
        format!("annos/{named}")
    } else {
        format!("annos/long/{}", short_hash(path))
    }
}

/// What a blob key may contain, so that a path from a page cannot name a key
/// that was not meant.
fn clean(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// A short name for a long one: FNV-1a, which is enough to tell twenty
/// thousand pages apart and short enough to read in a log.
fn short_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A file the site needs, and where it goes in it.
pub struct Asset {
    /// Where it goes, relative to the site's root.
    pub path: &'static str,
    /// What is in it.
    pub text: &'static str,
}

/// Everything a site needs to serve patches and annotations.
pub fn assets() -> Vec<Asset> {
    vec![
        Asset {
            path: "netlify/edge-functions/talimist-patchset.ts",
            text: include_str!("../assets/edge/talimist-patchset.ts"),
        },
        Asset {
            path: "netlify/functions/talimist-page.mts",
            text: include_str!("../assets/functions/talimist-page.mts"),
        },
        Asset {
            path: "netlify/functions/talimist-annotations.mts",
            text: include_str!("../assets/functions/talimist-annotations.mts"),
        },
        Asset {
            path: "_talimist/patch.js",
            text: include_str!("../assets/patch.js"),
        },
    ]
}

/// Writes those files into a site.
///
/// Returns what was written. Overwrites: these files are this crate's to say
/// what is in, and a site that has edited them will lose the edit, which is
/// better than a site running last month's idea of the protocol.
pub fn install(site: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for asset in assets() {
        let path = site.join(asset.path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, asset.text)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_takes_a_patch_only_when_it_is_behind() {
        let mut set = Patchset::default();
        set.put("/anafunctor/", "abc123", "2026-08-19T09:00:00Z");
        assert!(set.wanted("/anafunctor/", "old").is_some());
        // Already showing it: nothing to do, which is the answer for almost
        // every page that ever asks.
        assert!(set.wanted("/anafunctor/", "abc123").is_none());
        assert!(set.wanted("/elsewhere/", "old").is_none());
    }

    #[test]
    fn a_key_is_safe_and_short_enough_to_be_a_key() {
        assert_eq!(annos_key("/anafunctor/"), "annos/anafunctor/");
        // Netlify allows 600 bytes and no leading slash; a path longer than
        // that is named by its hash, which is as good a name and is short.
        let long = format!("/{}/", "a".repeat(700));
        let key = annos_key(&long);
        assert!(key.starts_with("annos/long/"), "{key}");
        assert!(key.len() < 600);
        assert_eq!(annos_key(&long), key);
        // What a path may not smuggle into a key.
        assert_eq!(annos_key("/a:b/c d/"), "annos/a_b/c_d/");
    }
}
