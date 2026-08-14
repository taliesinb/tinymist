//! `talimist-serve`: a document server with no editor attached.
//!
//! The language server has to restart when its binary changes and is tied to
//! one editor session; a document you are reading, or sharing with someone,
//! should not be. This serves `.typ` files over HTTP on a port derived from the
//! path it was given, so a document keeps its URL — and therefore its web app
//! and its icon — across restarts.

mod conn;
mod utils;
mod cmd {
    pub mod docsite;
    pub mod preview;
    pub mod serve;
}

use clap::Parser;
use tinymist_std::error::prelude::*;

/// Shared runtimes, as the other binary has.
pub static RUNTIMES: std::sync::LazyLock<Runtimes> = std::sync::LazyLock::new(Runtimes::default);

/// The runtimes this binary needs.
pub struct Runtimes {
    /// The tokio runtime everything async runs on.
    pub tokio_runtime: tokio::runtime::Runtime,
}

impl Default for Runtimes {
    fn default() -> Self {
        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("cannot build a tokio runtime");
        Self { tokio_runtime }
    }
}


fn main() -> Result<()> {
    crate::cmd::serve::serve_main(crate::cmd::serve::ServeArgs::parse())
}
