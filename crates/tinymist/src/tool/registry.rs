//! Which document servers are running, so that something else can find them.
//!
//! A port is derived from the path a server was given, so a server that knows
//! the path knows the port. Nothing else does: an agent asked to "look at the
//! comments on MATH" has a name, not a path, and no way to turn one into the
//! other. So each server leaves a note saying what it is serving and where it
//! answers, and takes the note away when it stops.
//!
//! Notes outlive servers that are killed. A reader treats a note whose port
//! does not answer as what it is — a note from a server that is gone — and
//! removes it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One running server, as its note describes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerNote {
    /// A short name for the server, from the path it serves: `hgxz-docs`.
    /// What a person says when they mean this server, and what an agent
    /// matches against.
    pub server: String,
    /// The file or directory being served.
    pub path: String,
    /// Where it answers.
    pub url: String,
    /// The port, which is the note's own name.
    pub port: u16,
    /// Which face it wears: `serve` or `annotate`.
    pub role: String,
    /// Whether it answers agents at `/m/`.
    pub mcp: bool,
    /// Whether it is a directory of documents rather than one.
    pub directory: bool,
    /// The process serving it.
    pub pid: u32,
    /// The process that started that one, as it was at the time.
    ///
    /// Says who owns a server, which a pid on its own does not: a preview whose
    /// parent is the editor was started by the editor, and one whose parent is
    /// a shell was started by hand. Recorded when the note is written, since a
    /// process that is reparented later — its starter having gone — reads as a
    /// child of `init` and no longer says where it came from.
    #[serde(default)]
    pub ppid: u32,
    /// Whether that process serves this among other things.
    ///
    /// A document server is its own process and stopping it stops exactly what
    /// it serves. An editor's preview is served by the language server, which
    /// is also answering completions and diagnostics for a person who is
    /// typing: the note points at a process that is not ours to kill.
    #[serde(default)]
    pub hosted: bool,
    /// Whether what it serves is a copy made for this server alone, named after
    /// its process. The copy is nobody else's: it has its own port, its own
    /// sidecar, and no other server will ever be asked to share it.
    #[serde(default)]
    pub fork: bool,
    /// When it started, ISO 8601 UTC.
    pub started: String,
}

/// Where the notes are kept: one file per port, so two servers never write to
/// the same one and a crash leaves at most its own behind.
pub fn registry_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::state_dir())
        .or_else(|| dirs::data_local_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join("talimist").join("servers")
}

/// A short name for a path: the last directory, and the one above it when
/// that would otherwise be something as common as `docs`.
pub fn slug_for(path: &Path) -> String {
    let stem = |part: Option<&std::ffi::OsStr>| {
        part.map(|part| part.to_string_lossy().trim_end_matches(".typ").to_owned())
    };
    let last = stem(path.file_name()).unwrap_or_else(|| "documents".into());
    let parent = stem(path.parent().and_then(Path::file_name));
    let name = match parent {
        // `docs` alone says nothing when three projects have one.
        Some(parent) if !parent.is_empty() => format!("{parent}-{last}"),
        _ => last,
    };
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase()
}

/// The port this process left a note under, if it left one.
static MY_PORT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The port this server announced itself on.
pub fn my_port() -> Option<u16> {
    match MY_PORT.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        port => Some(port as u16),
    }
}

/// Leaves a note that this server is running.
pub fn announce_server(note: &ServerNote) -> std::io::Result<PathBuf> {
    let dir = registry_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", note.port));
    std::fs::write(&path, serde_json::to_string_pretty(note).unwrap_or_default())?;
    MY_PORT.store(u32::from(note.port), std::sync::atomic::Ordering::SeqCst);
    Ok(path)
}

/// The process that started this one, where that can be asked.
pub fn parent_pid() -> u32 {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Takes the note away.
pub fn withdraw_server(port: u16) {
    let _ = std::fs::remove_file(registry_dir().join(format!("{port}.json")));
}

/// How long a server has to answer for before its note is believed stale. A
/// document server compiles what it serves before it listens, and a long
/// document takes a moment.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Every server that is running, with the notes of those that are gone removed
/// on the way past.
pub fn running_servers() -> Vec<ServerNote> {
    let Ok(entries) = std::fs::read_dir(registry_dir()) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(note) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<ServerNote>(&text).ok())
        else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if answers(note.port) {
            out.push(note);
            continue;
        }
        // Not answering yet is not the same as gone: a server writes its note
        // as it starts and binds its port a moment later, and a reader that
        // tidied up in between would delete the note of a server on its way in.
        let young = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|when| when.elapsed().ok())
            .is_some_and(|age| age < STARTUP_GRACE);
        if !young {
            let _ = std::fs::remove_file(&path);
        }
    }
    out.sort_by(|a, b| a.server.cmp(&b.server));
    out
}

/// Every note in the register, said or unsaid, with nothing removed.
///
/// `running_servers` tidies as it reads, which is right for a caller that wants
/// to talk to something; a caller that wants to *report* on the register needs
/// to see the notes that are not answering, because those are the interesting
/// ones. Each is paired with whether its port answers.
pub fn all_notes() -> Vec<(ServerNote, bool)> {
    let Ok(entries) = std::fs::read_dir(registry_dir()) else {
        return vec![];
    };
    let mut out: Vec<(ServerNote, bool)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let note: ServerNote = serde_json::from_str(&text).ok()?;
            let live = answers(note.port);
            Some((note, live))
        })
        .collect();
    out.sort_by(|a, b| a.0.server.cmp(&b.0.server));
    out
}

/// Whether anything is listening on a port and answering as one of ours.
pub fn answers(port: u16) -> bool {
    get(port, "/dev/build").is_some()
}

/// A plain GET against a loopback port, for probing and for the hub's own
/// questions. Small on purpose: this is one request to one process on this
/// machine, and an HTTP client would be a dependency for it.
pub fn get(port: u16, path: &str) -> Option<String> {
    request(port, "GET", path, None)
}

/// A POST with a JSON body, which is how the hub forwards a call.
pub fn post_json(port: u16, path: &str, body: &str) -> Option<String> {
    request(port, "POST", path, Some(body))
}

fn request(port: u16, method: &str, path: &str, body: Option<&str>) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};

    // Long enough for a tool call that waits — `wait_for_annotations` is meant
    // to sit there — and the caller's own patience bounds the rest.
    let connect = std::time::Duration::from_millis(500);
    let read = std::time::Duration::from_secs(330);
    for addr in ("127.0.0.1", port).to_socket_addrs().ok()? {
        let Ok(mut sock) = TcpStream::connect_timeout(&addr, connect) else {
            continue;
        };
        let _ = sock.set_read_timeout(Some(read));
        let _ = sock.set_write_timeout(Some(connect));
        let mut request = format!(
            "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n"
        );
        if let Some(body) = body {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        if let Some(body) = body {
            request.push_str(body);
        }
        if sock.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if sock.read_to_end(&mut buf).is_err() && buf.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let (head, body) = text.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.") || !head.contains(" 200") {
            continue;
        }
        return Some(body.to_owned());
    }
    None
}
