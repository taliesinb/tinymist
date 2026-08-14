//! Static assets: the scripts, stylesheets, pages and templates the tools
//! serve, kept as files under `src/static` rather than as strings in Rust.
//!
//! Each is embedded at build time and read from the source tree when there is
//! one, so an edit shows up on the next request rather than on the next build.
//! A shipped binary has no source tree beside it and falls back to what it was
//! built with.
//!
//! Neither tool's own business, so neither tool owns it: the previewer's
//! overlay script and the document server's annotator both come from here.

/// Loads an asset from `src/static/<dir>/<rel>`, or the embedded copy.
pub fn dev_asset_in(dir: &str, rel: &str, embedded: &'static str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/static")
        .join(dir)
        .join(rel);
    std::fs::read_to_string(path).unwrap_or_else(|_| embedded.to_owned())
}

/// Where an asset lives in the source tree, for watching it.
pub fn dev_asset_path(dir: &str, rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/static")
        .join(dir)
        .join(rel)
}
