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

/// The events this process has narrated, so that something arriving later can
/// still hear them.
///
/// Narration on a stream is for whoever was watching at the time; an agent
/// asking "what happened while I was thinking?" needs the same events with
/// numbers on them. The last few hundred are kept — enough for any client that
/// is not asleep, and bounded so a long-running server does not grow a diary.
const KEPT_EVENTS: usize = 1024;

struct EventLog {
    /// The events, oldest first, each with the id it was given.
    events: std::sync::Mutex<std::collections::VecDeque<(u64, serde_json::Value)>>,
    /// The id of the last event, which waiters watch for a change in.
    latest: tokio::sync::watch::Sender<u64>,
}

static EVENTS: std::sync::OnceLock<EventLog> = std::sync::OnceLock::new();

fn log() -> &'static EventLog {
    EVENTS.get_or_init(|| EventLog {
        events: std::sync::Mutex::new(std::collections::VecDeque::new()),
        latest: tokio::sync::watch::channel(0).0,
    })
}

/// Records an event, whatever else is done with it. Called by both narrators:
/// the one that writes lines to stderr for a person, and the one that writes
/// them to stdout for an agent driving the server.
pub fn record_event(event: serde_json::Value) {
    let log = log();
    let id = {
        let Ok(mut events) = log.events.lock() else {
            return;
        };
        let id = events.back().map(|(id, _)| id + 1).unwrap_or(1);
        events.push_back((id, event));
        while events.len() > KEPT_EVENTS {
            events.pop_front();
        }
        id
    };
    let _ = log.latest.send(id);
}

/// The events after `since`, and the id to ask with next time.
///
/// A `since` older than what is kept returns what there is: an event that has
/// fallen off the end is one nobody can be told about, and saying so by
/// skipping is better than pretending the gap is not there — the ids show it.
pub fn events_since(since: u64) -> (Vec<serde_json::Value>, u64) {
    let log = log();
    let Ok(events) = log.events.lock() else {
        return (vec![], since);
    };
    let mut out = vec![];
    let mut cursor = since;
    for (id, event) in events.iter() {
        if *id > since {
            out.push(event.clone());
            cursor = *id;
        }
    }
    if out.is_empty() {
        cursor = events.back().map(|(id, _)| *id).unwrap_or(since);
    }
    (out, cursor)
}

/// A handle that says when an event has been recorded, for waiting on one.
pub fn event_signal() -> tokio::sync::watch::Receiver<u64> {
    log().latest.subscribe()
}

/// Says what just happened, as one line of JSON on stderr.
///
/// One event per line, machine-readable, because the things worth narrating —
/// who arrived, who left, what was rebuilt — are things a person watching a
/// terminal wants to read *and* a program driving this server wants to parse.
/// Prose can only be one of those. Stdout stays clear for the address block,
/// which is the one thing meant to be copied.
///
/// Fields are written in the order given, after `ts` and `type`. The time comes
/// first and is a fixed width, so a column of lines is a column of times.
pub fn announce(kind: &str, fields: &[(&str, serde_json::Value)]) {
    if !announcing() {
        return;
    }
    let line = event_line(kind, fields);
    record_event(serde_json::from_str(&line).unwrap_or_default());
    eprintln!("{line}");
}

/// One event as one line of JSON, with `ts` first and `type` after it.
pub fn event_line(kind: &str, fields: &[(&str, serde_json::Value)]) -> String {
    let mut line = format!(
        "{{\"ts\":{},\"type\":{}",
        serde_json::Value::from(iso_now()),
        serde_json::Value::from(kind)
    );
    for (key, value) in fields {
        line.push_str(&format!(",{}:{value}", serde_json::Value::from(*key)));
    }
    line.push('}');
    line
}

/// When something last changed on disk, as this process saw it.
///
/// A compile is announced with how long it took, and what it took is measured
/// from the change that caused it rather than from when the compiler happened
/// to be asked: the figure worth reading is the wait between saving a file and
/// seeing the document.
/// The path is kept as well as the time, because a compile says why it ran and
/// the answer is which file moved: the document itself, its annotations, or
/// something it reads.
static LAST_CHANGE: std::sync::Mutex<Option<(std::time::Instant, String)>> =
    std::sync::Mutex::new(None);

/// Notes that a file changed, as the clock a compile is measured against.
pub fn note_change(path: &str) {
    if let Ok(mut slot) = LAST_CHANGE.lock() {
        *slot = Some((std::time::Instant::now(), path.to_owned()));
    }
}

/// How long ago the last change was seen, if one has been.
pub fn since_change() -> Option<std::time::Duration> {
    LAST_CHANGE
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|(at, _)| at.elapsed()))
}

/// Which file the last change was to.
pub fn changed_path() -> Option<String> {
    LAST_CHANGE
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|(_, path)| path.clone()))
}
