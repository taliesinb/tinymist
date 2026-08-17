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

/// Where a server is published: which machine, and under which path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The machine's name on the tailnet, as a reader types it: `tbwork`.
    pub host: String,
    /// The path it is published under, with a leading slash: `/report`.
    pub path: String,
}

impl Mount {
    /// Reads `host` or `host/path`.
    ///
    /// `fallback` is the path to use when only a host was given — the name the
    /// server calls what it is serving.
    pub fn parse(spec: &str, fallback: &str) -> Result<Self, String> {
        let spec = spec.trim().trim_start_matches("http://").trim_matches('/');
        let (host, path) = match spec.split_once('/') {
            Some((host, path)) => (host, path.to_owned()),
            None => (spec, clean(fallback)),
        };
        if host.is_empty() {
            return Err("which machine? --tailscale-host takes a name on your tailnet".into());
        }
        let path = clean(&path);
        if path.is_empty() {
            return Err(
                "which path? give one as --tailscale-host host/path, or name the server \
                 with --root-name"
                    .into(),
            );
        }
        Ok(Self {
            host: host.to_owned(),
            path: format!("/{path}"),
        })
    }

    /// The address to hand to a reader.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.host, self.path)
    }

    /// The same, on the tailnet's own domain, when that can be read from
    /// Tailscale. Some clients need the full name.
    pub fn full_url(&self) -> Option<String> {
        Some(format!("http://{}{}", full_host(&self.host)?, self.path))
    }

    /// Every origin a browser might send when reading this: the short name, the
    /// full one, and both over TLS, since a tailnet with HTTPS enabled serves
    /// the same paths.
    pub fn origins(&self) -> Vec<String> {
        let mut out = vec![
            format!("http://{}", self.host),
            format!("https://{}", self.host),
        ];
        if let Some(full) = full_host(&self.host) {
            out.push(format!("http://{full}"));
            out.push(format!("https://{full}"));
        }
        out
    }
}

/// A path with the punctuation a URL cannot carry taken out.
fn clean(path: &str) -> String {
    path.trim_matches('/')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
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

/// This machine's full name on its tailnet, as MagicDNS knows it.
///
/// Read from `tailscale status --json` by hand rather than with a JSON parser:
/// one field is wanted, and this crate is otherwise dependency-free.
pub fn full_host(host: &str) -> Option<String> {
    let status = run(&["status", "--json"]).ok()?;
    let needle = "\"DNSName\":";
    for (at, _) in status.match_indices(needle) {
        let rest = &status[at + needle.len()..];
        let value = rest.trim_start().strip_prefix('"')?;
        let end = value.find('"')?;
        let name = value[..end].trim_end_matches('.');
        // Every machine on the tailnet is in there; this one is the one whose
        // name matches.
        if name.split('.').next() == Some(host) {
            return Some(name.to_owned());
        }
    }
    None
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
    use super::Mount;

    #[test]
    fn a_host_alone_takes_the_servers_own_name() {
        let mount = Mount::parse("tbwork", "hgxz docs").expect("parses");
        assert_eq!(mount.host, "tbwork");
        assert_eq!(mount.path, "/hgxz-docs");
        assert_eq!(mount.url(), "http://tbwork/hgxz-docs");
    }

    #[test]
    fn a_path_is_taken_as_given() {
        let mount = Mount::parse("tbwork/report", "ignored").expect("parses");
        assert_eq!(mount.path, "/report");
    }

    #[test]
    fn a_url_is_read_as_a_name() {
        let mount = Mount::parse("http://tbwork/report/", "ignored").expect("parses");
        assert_eq!(mount.host, "tbwork");
        assert_eq!(mount.path, "/report");
    }

    #[test]
    fn a_host_with_nothing_to_call_it_is_refused() {
        assert!(Mount::parse("tbwork", "").is_err());
        assert!(Mount::parse("", "name").is_err());
    }
}
