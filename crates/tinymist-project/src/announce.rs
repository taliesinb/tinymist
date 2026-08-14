//! Whether this process narrates what it is doing.

/// Whether to narrate file changes and client arrivals on stderr.
///
/// Off by default, and opted into by a standalone server. A language server
/// shares its streams with the editor, so it has no business narrating; a
/// server a person is watching in a terminal is the opposite case, and the one
/// question it must answer is "why did it just rebuild?".
///
/// Lives here rather than beside the watcher that reads it: the question is
/// asked by anything that might narrate — the file watcher, the HTTP server —
/// and those are compiled under different features. A flag that only exists
/// when the watcher does cannot be asked about by code that never watches.
pub static ANNOUNCE_ACTIVITY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether [`ANNOUNCE_ACTIVITY`] is set.
pub fn announcing() -> bool {
    ANNOUNCE_ACTIVITY.load(std::sync::atomic::Ordering::Relaxed)
}

/// The current time as an ISO 8601 UTC string, e.g. "2026-08-11T01:12:40Z".
pub fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Says what just happened, as one line of JSON on stderr.
///
/// One event per line, machine-readable, because the things worth narrating —
/// who arrived, who left, what was rebuilt — are things a person watching a
/// terminal wants to read *and* a program driving this server wants to parse.
/// Prose can only be one of those. Stdout stays clear for the address block,
/// which is the one thing meant to be copied.
///
/// Fields are written in the order given, with `type` first and `ts` last, so
/// the lines read the way they were designed to.
pub fn announce(kind: &str, fields: &[(&str, serde_json::Value)]) {
    if !announcing() {
        return;
    }
    let mut line = format!("{{\"type\":{}", serde_json::Value::from(kind));
    for (key, value) in fields {
        line.push_str(&format!(",{}:{value}", serde_json::Value::from(*key)));
    }
    line.push_str(&format!(",\"ts\":{}}}", serde_json::Value::from(iso_now())));
    eprintln!("{line}");
}

/// When something last changed on disk, as this process saw it.
///
/// A compile is announced with how long it took, and what it took is measured
/// from the change that caused it rather than from when the compiler happened
/// to be asked: the figure worth reading is the wait between saving a file and
/// seeing the document.
static LAST_CHANGE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Notes that a file changed, as the clock a compile is measured against.
pub fn note_change() {
    if let Ok(mut slot) = LAST_CHANGE.lock() {
        *slot = Some(std::time::Instant::now());
    }
}

/// How long ago the last change was seen, if one has been.
pub fn since_change() -> Option<std::time::Duration> {
    LAST_CHANGE
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .map(|at| at.elapsed())
}
