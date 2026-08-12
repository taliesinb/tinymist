use tinymist::world::system::print_diagnostics;
use tinymist::world::{DiagnosticFormat, SourceWorld};
use tinymist_std::{bail, error::prelude::*};

pub fn exit_on_ctrl_c() {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("Ctrl-C received, exiting");
        std::process::exit(0);
    });
}

/// Exits when the binary this process was started from is replaced.
///
/// A long-running server keeps its own inode after `cargo install` writes a new
/// one, so it would go on serving code that no longer exists anywhere on disk —
/// the failure that makes a rebuild look like it did nothing. Comparing the
/// identity of the file at our own path catches the swap; a caller that wants
/// the old behaviour can simply not call this.
///
/// Two consecutive readings must disagree before exiting, so an installer that
/// unlinks and recreates the file is not mistaken for a replacement mid-write.
pub fn exit_when_binary_replaced() {
    let Ok(path) = std::env::current_exe() else {
        return;
    };
    let identity = |p: &std::path::Path| {
        std::fs::metadata(p).ok().map(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                (m.ino(), m.size(), m.mtime())
            }
            #[cfg(not(unix))]
            {
                (0u64, m.len(), 0i64)
            }
        })
    };
    let Some(original) = identity(&path) else {
        return;
    };
    std::thread::spawn(move || {
        let mut suspicious = false;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let now = identity(&path);
            if now == Some(original) {
                suspicious = false;
                continue;
            }
            if !suspicious {
                suspicious = true;
                continue;
            }
            // Straight to stderr: the log filters only admit a fixed set of
            // modules, and an unexplained exit is worse than a noisy one. Zed
            // keeps a language server's stderr, so this is findable there too.
            eprintln!(
                "{} was replaced since this process started, shutting down",
                path.display()
            );
            std::process::exit(0);
        }
    });
}

pub fn block_on<F: Future>(future: F) -> F::Output {
    crate::RUNTIMES.tokio_runtime.block_on(future)
}

pub fn print_diag_or_error<T>(world: &impl SourceWorld, result: Result<T>) -> Result<T> {
    match result {
        Ok(v) => Ok(v),
        Err(err) => {
            if let Some(diagnostics) = err.diagnostics() {
                print_diagnostics(world, diagnostics.iter(), DiagnosticFormat::Human)
                    .context_ut("print diagnostics")?;
                bail!("");
            }

            Err(err)
        }
    }
}
