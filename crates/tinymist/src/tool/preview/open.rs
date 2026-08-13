//! Opening a served document in a browser.
//!
//! Three things vary, and they are independent: which browser, whether the
//! document gets a window of its own, and whether that window can be driven
//! from outside. A dock app installed from this URL is preferred when isolation
//! is asked for, because it is the window the person already chose for this
//! document; the fallbacks reproduce as much of that as each browser allows.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which browser to open with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Browser {
    /// Whatever the system opens URLs with.
    Default,
    Safari,
    Chrome,
    Firefox,
    /// An application named literally, as it appears in /Applications.
    Named(String),
}

impl Browser {
    /// Reads a `--open-in` value. The known names are lowercase; anything else
    /// is taken literally, so a specific build ("Google Chrome Canary") or a
    /// full path still works.
    pub fn parse(spec: &str) -> Self {
        match spec {
            "browser" | "default" => Browser::Default,
            "safari" => Browser::Safari,
            "chrome" => Browser::Chrome,
            "firefox" => Browser::Firefox,
            other => Browser::Named(other.to_owned()),
        }
    }

    /// The application bundle, at its usual place on this machine.
    fn app(&self) -> Option<PathBuf> {
        let name = match self {
            Browser::Default => return None,
            Browser::Safari => "Safari",
            Browser::Chrome => "Google Chrome",
            Browser::Firefox => "Firefox",
            Browser::Named(name) => name,
        };
        if name.contains('/') {
            return Some(PathBuf::from(name));
        }
        let file = format!("{name}.app");
        let home = dirs::home_dir().unwrap_or_default().join("Applications").join(&file);
        let system = PathBuf::from("/Applications").join(&file);
        [home, system].into_iter().find(|path| path.exists())
    }

    /// The executable inside the bundle, for the cases that need flags —
    /// `open -a` cannot pass them.
    fn binary(&self) -> Option<PathBuf> {
        let app = self.app()?;
        let stem = app.file_stem()?.to_string_lossy().into_owned();
        let exe = app.join("Contents/MacOS").join(&stem);
        exe.exists().then_some(exe)
    }
}

/// How to open a URL.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// The browser to use.
    pub browser: Browser,
    /// Give the document a window of its own, rather than a tab among others.
    pub isolated: bool,
    /// Expose a debugging port on that window. Chrome only.
    pub cdp_port: Option<u16>,
    /// The name a dock app installed from this URL would carry, which is what
    /// isolation looks for first.
    pub app_title: String,
    /// The server's port, which keys the profile an isolated Chrome uses so
    /// that one document's window is not another's.
    pub key: u16,
}

/// Opens `url` as the options ask, falling back to the plain browser and then
/// to the system default rather than failing.
pub fn open(url: &str, opts: &OpenOptions) {
    if opts.isolated {
        // A dock app installed from this URL is the window the person already
        // chose for this document; nothing synthesised beats it.
        if let Some(app) = installed_app(&opts.app_title) {
            if open_with(&app, url) {
                return;
            }
        }
    }

    match &opts.browser {
        Browser::Chrome if opts.isolated || opts.cdp_port.is_some() => {
            if open_chrome(url, opts) {
                return;
            }
        }
        _ if opts.cdp_port.is_some() => {
            log::warn!("--open-cdp only applies to chrome; opening without it");
        }
        _ => {}
    }

    if let Some(app) = opts.browser.app() {
        if open_with(&app, url) {
            return;
        }
    }
    let _ = Command::new("open").arg(url).spawn();
}

/// A web app installed from this URL, by the name its manifest gives.
///
/// Safari's "Add to Dock" writes to ~/Applications; Chrome's "Install page as
/// app" writes a shim to ~/Applications/Chrome Apps. Both carry the manifest's
/// short name and icon, which is the only way an isolated window gets a name
/// of its own — a plain `--app=` window is still Chrome, and macOS names a
/// window after the process that owns it.
fn installed_app(title: &str) -> Option<PathBuf> {
    let file = format!("{title}.app");
    let home = dirs::home_dir().unwrap_or_default();
    [
        home.join("Applications").join(&file),
        home.join("Applications/Chrome Apps").join(&file),
        PathBuf::from("/Applications").join(&file),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn open_with(app: &Path, url: &str) -> bool {
    Command::new("open")
        .arg("-a")
        .arg(app)
        .arg(url)
        .spawn()
        .is_ok()
}

/// Chrome with flags: its own window, its own profile, and a debugging port if
/// one was asked for. Chrome refuses a debugging port for a profile that is
/// already running, so the profile is keyed to this server.
fn open_chrome(url: &str, opts: &OpenOptions) -> bool {
    let Some(binary) = Browser::Chrome.binary() else {
        return false;
    };
    let mut cmd = Command::new(binary);
    if opts.isolated {
        let profile = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("talimist")
            .join(format!("chrome-{}", opts.key));
        let _ = std::fs::create_dir_all(&profile);
        cmd.arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--app={url}"));
    } else {
        cmd.arg(url);
    }
    if let Some(port) = opts.cdp_port {
        cmd.arg(format!("--remote-debugging-port={port}"));
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Reads a `--open-cdp` value: a port, or `auto` for one derived from the
/// server's own port.
pub fn parse_cdp_port(spec: &str, server_port: u16) -> Option<u16> {
    match spec {
        "auto" => server_port.checked_add(1),
        "none" | "" => None,
        other => other.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_browser_names() {
        assert_eq!(Browser::parse("chrome"), Browser::Chrome);
        assert_eq!(Browser::parse("safari"), Browser::Safari);
        assert_eq!(Browser::parse("firefox"), Browser::Firefox);
        assert_eq!(Browser::parse("browser"), Browser::Default);
        // Anything else is an application name, so a specific build still works
        assert_eq!(
            Browser::parse("Google Chrome Canary"),
            Browser::Named("Google Chrome Canary".into())
        );
        // ...including one that is capitalised like the app it names
        assert_eq!(Browser::parse("Safari"), Browser::Named("Safari".into()));
    }

    #[test]
    fn reads_cdp_ports() {
        assert_eq!(parse_cdp_port("auto", 24123), Some(24124));
        assert_eq!(parse_cdp_port("9222", 24123), Some(9222));
        assert_eq!(parse_cdp_port("none", 24123), None);
        assert_eq!(parse_cdp_port("nonsense", 24123), None);
    }
}
