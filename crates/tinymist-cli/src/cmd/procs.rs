//! `talimist procs`: what is running, and how to stop it.
//!
//! Every server leaves a note in the register saying what it serves, where it
//! answers and which process it is. That is enough to report on them, and
//! enough to reap them: a preview whose editor died, a document server from a
//! window closed hours ago, a hub nobody is talking to. Rather than reading
//! `lsof` and guessing which of the processes are ours, ask the register.

use std::time::Duration;

use tinymist::tool::registry::{self, ServerNote};
use tinymist_std::error::prelude::*;

/// What to do about the servers that are running.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ProcsCommands {
    /// List every server in the register, live or stale
    List,
    /// Stop every server in the register, and clear their notes
    Shutdown(ShutdownArgs),
}

/// How to stop them.
#[derive(Debug, Clone, clap::Parser)]
pub struct ShutdownArgs {
    /// How long to wait after asking, before insisting.
    ///
    /// A server asked to stop puts its house in order first — the site it
    /// rendered, its note in the register — and that is worth waiting for.
    #[clap(long = "grace", value_name = "SECONDS", default_value_t = 3)]
    pub grace: u64,

    /// Stop shared servers as well. They are left alone by default: a shared
    /// server is somebody else's window on a document, and stopping it takes
    /// the address they are reading it at down with it.
    #[clap(long = "shared")]
    pub shared: bool,
}

/// Runs a `procs` subcommand.
pub fn procs_main(cmd: ProcsCommands) -> Result<()> {
    match cmd {
        ProcsCommands::List => list(),
        ProcsCommands::Shutdown(args) => shutdown(args),
    }
}

/// One note, on one line.
fn describe(note: &ServerNote, live: bool) -> String {
    format!(
        "{state}  {server:<20} {role:<9} pid {pid:<7} parent {ppid:<7} {url}           {path}{mcp}{hosted}{fork}{shared}  since {started}",
        state = if live { "live " } else { "stale" },
        hosted = if note.hosted { "  hosted" } else { "" },
        fork = if note.fork { "  fork" } else { "" },
        server = note.server,
        role = note.role,
        pid = note.pid,
        ppid = note.ppid,
        url = note.url,
        path = note.path,
        mcp = if note.mcp { "  mcp" } else { "" },
        shared = note
            .shared
            .as_deref()
            .map(|url| format!("  shared {url}"))
            .unwrap_or_default(),
        started = note.started,
    )
}

/// Prints the register.
fn list() -> Result<()> {
    let notes = registry::all_notes();
    if notes.is_empty() {
        println!("nothing in the register");
        return Ok(());
    }
    for (note, live) in &notes {
        println!("{}", describe(note, *live));
    }
    Ok(())
}

/// Stops everything in the register.
///
/// Asked first and made to, second: a server that is stopping tidies up after
/// itself, and one that has stopped answering has nothing to tidy. Whichever
/// way it goes, the note goes at the end — a register describing processes that
/// are not there is worse than an empty one.
fn shutdown(args: ShutdownArgs) -> Result<()> {
    let mine = std::process::id();
    let notes = registry::all_notes();
    let (mine_note, others): (Vec<_>, Vec<_>) = notes
        .into_iter()
        .partition(|(note, _)| note.pid == mine);
    // A shared server is a window somebody else has open; stopping it takes
    // their address down with it, so it is left alone unless it is named.
    let (shared, others): (Vec<_>, Vec<_>) = others
        .into_iter()
        .partition(|(note, _)| note.shared.is_some() && !args.shared);
    for (note, _) in &mine_note {
        println!("keeping   {} (this process)", note.server);
    }
    for (note, _) in &shared {
        println!(
            "keeping   {} (shared at {}; --shared stops it too)",
            note.server,
            note.shared.as_deref().unwrap_or("")
        );
    }
    if others.is_empty() {
        println!("nothing to stop");
        return Ok(());
    }

    for (note, live) in &others {
        println!("asking    {}", describe(note, *live));
        // Stopping a hosted server stops the process hosting it — which is the
        // point: the language server is what an editor restarts when it finds
        // it gone, and a stale one is the thing being reaped.
        if note.hosted {
            println!("          (the language server; the editor will start a fresh one)");
        }
        signal(note.pid, "TERM");
    }

    // One wait for all of them, not one each: they were asked at the same time
    // and they are stopping at the same time.
    let deadline = std::time::Instant::now() + Duration::from_secs(args.grace);
    while std::time::Instant::now() < deadline {
        if others.iter().all(|(note, _)| !alive(note.pid)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    for (note, _) in &others {
        if alive(note.pid) {
            println!("killing   {} (pid {})", note.server, note.pid);
            signal(note.pid, "KILL");
        }
        registry::withdraw_server(note.port);
    }
    Ok(())
}

/// Whether a process is still running.
///
/// Asked of `ps` rather than with signal 0, which a process that has already
/// exited still answers to: until its parent reaps it, it is a zombie — dead,
/// but with an entry left in the table. Reading that as alive means waiting out
/// the grace period and then sending a kill to something that has already gone.
fn alive(pid: u32) -> bool {
    let state = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output();
    match state {
        Ok(out) => {
            let state = String::from_utf8_lossy(&out.stdout);
            let state = state.trim();
            !state.is_empty() && !state.starts_with('Z')
        }
        Err(_) => false,
    }
}

/// Sends a signal, by name.
fn signal(pid: u32, name: &str) {
    let result = std::process::Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Err(err) = result {
        log::warn!("cannot signal {pid}: {err}");
    }
}
