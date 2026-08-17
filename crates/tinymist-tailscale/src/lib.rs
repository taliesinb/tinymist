//! Publishing a local server on a tailnet, by driving the `tailscale` command.
//!
//! A document server binds loopback. To let somebody else read it, something
//! has to stand in front of it that knows who they are, and `tailscale serve`
//! is that: it terminates the connection on the tailnet, authenticates the
//! caller, and proxies to the loopback port.
//!
//! This runs the two commands that set that up and take it down again, and
//! reads the machine's name on the tailnet so a server can say where it is.
//! There is no Tailscale library here — the command is the interface, and it
//! is the one that is installed on a machine already using Tailscale.

use std::process::{Command, Stdio};

/// Where a server is published: this machine's name on the tailnet, and the
/// path it is published under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The machine's name as a reader types it: `tbwork`.
    pub host: String,
    /// The same on the tailnet's own domain, when it can be read.
    pub full: Option<String>,
    /// The path it is published under, with a leading slash: `/report`.
    pub path: String,
}

impl Mount {
    /// A mount on this machine, under the given path.
    ///
    /// The machine is always this one — `tailscale serve` publishes what is
    /// running here — so the only choice is the path. `called` is what to use
    /// when none was given: the name the server calls what it is serving.
    pub fn here(path: Option<&str>, called: &str) -> Result<Self, String> {
        let (host, full) = self_host()
            .ok_or("cannot read this machine's name from tailscale; is it running?")?;
        let path = clean(path.unwrap_or(called));
        if path.is_empty() {
            return Err(
                "which path? give one as --tailscale=path, or name the server with --root-name"
                    .into(),
            );
        }
        Ok(Self {
            host,
            full,
            path: format!("/{path}"),
        })
    }

    /// The address to hand to a reader.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.host, self.path)
    }

    /// The same, on the tailnet's own domain, when that is known. Some clients
    /// need the full name.
    pub fn full_url(&self) -> Option<String> {
        Some(format!("http://{}{}", self.full.as_ref()?, self.path))
    }

    /// Every origin a browser might send when reading this: the short name, the
    /// full one, and both over TLS, since a tailnet with HTTPS enabled serves
    /// the same paths.
    pub fn origins(&self) -> Vec<String> {
        let mut out = vec![
            format!("http://{}", self.host),
            format!("https://{}", self.host),
        ];
        if let Some(full) = &self.full {
            out.push(format!("http://{full}"));
            out.push(format!("https://{full}"));
        }
        out
    }
}

/// A path with the punctuation a URL cannot carry taken out, and no run of
/// dashes where a run of punctuation was.
fn clean(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.trim_matches('/').chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/') {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Whether the `tailscale` command is there to be run.
pub fn available() -> bool {
    run(&["version"]).is_ok()
}

/// Publishes a loopback port at a path on this machine's tailnet name.
///
/// Idempotent as far as Tailscale is concerned: setting the same path twice is
/// setting it once.
pub fn publish(mount: &Mount, port: u16) -> Result<(), String> {
    run(&[
        "serve",
        "--bg",
        "--http=80",
        &format!("--set-path={}", mount.path),
        &port.to_string(),
    ])
    .map(|_| ())
}

/// Takes it down again.
pub fn withdraw(mount: &Mount) -> Result<(), String> {
    run(&[
        "serve",
        "--http=80",
        &format!("--set-path={}", mount.path),
        "off",
    ])
    .map(|_| ())
}

/// This machine's name on its tailnet: the short one a reader types, and the
/// full one MagicDNS knows.
///
/// Read from `tailscale status --json` by hand rather than with a JSON parser:
/// one field is wanted, and this crate is otherwise dependency-free. The
/// machine's own entry is under `Self`, which comes before the peers.
pub fn self_host() -> Option<(String, Option<String>)> {
    let status = run(&["status", "--json"]).ok()?;
    let at = status.find("\"Self\":")?;
    let needle = "\"DNSName\":";
    let at = status[at..].find(needle)? + at + needle.len();
    let value = status[at..].trim_start().strip_prefix('"')?;
    let end = value.find('"')?;
    let full = value[..end].trim_end_matches('.').to_ascii_lowercase();
    let short = full.split('.').next()?.to_owned();
    if short.is_empty() {
        return None;
    }
    let full = (full != short).then_some(full);
    Some((short, full))
}

/// Runs `tailscale` with the given arguments and returns what it said.
fn run(args: &[&str]) -> Result<String, String> {
    let out = Command::new("tailscale")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("cannot run tailscale: {err}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if said.is_empty() {
            format!("tailscale {} failed", args.join(" "))
        } else {
            said
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{clean, Mount};

    /// A mount without asking Tailscale for the machine's name.
    fn mount(path: &str) -> Mount {
        Mount {
            host: "machine".into(),
            full: Some("machine.tailnet.ts.net".into()),
            path: format!("/{}", clean(path)),
        }
    }

    #[test]
    fn a_name_becomes_a_path() {
        assert_eq!(clean("hgxz docs"), "hgxz-docs");
        assert_eq!(clean("/report/"), "report");
        assert_eq!(clean("A Paper: Draft 2"), "A-Paper-Draft-2");
    }

    #[test]
    fn a_mount_says_where_it_is() {
        let mount = mount("report");
        assert_eq!(mount.url(), "http://machine/report");
        assert_eq!(
            mount.full_url().as_deref(),
            Some("http://machine.tailnet.ts.net/report")
        );
    }

    #[test]
    fn every_name_the_browser_might_send_is_accepted() {
        let origins = mount("report").origins();
        assert!(origins.contains(&"http://machine".to_owned()));
        assert!(origins.contains(&"https://machine.tailnet.ts.net".to_owned()));
    }
}
