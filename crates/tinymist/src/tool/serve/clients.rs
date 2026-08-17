//! Who is reading: the callers a server has seen, each given a number.
//!
//! A request says who it is in its headers — the tailnet login `tailscale
//! serve` authenticates and injects, the address it came from, the browser it
//! was made with — and says it again on every request. Repeating that on every
//! line makes the narration long and makes two lines about the same reader hard
//! to see as one.
//!
//! So the identifying headers are read once and turned into a number. The first
//! request from a caller is announced as `client_identified`, with everything
//! that is known about it; every line after that names the number alone.
//!
//! A caller is the triple of login, address and browser. Two tabs in one
//! browser are one caller; the same person on a phone and on a laptop are two,
//! since nothing in the headers ties them together.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Mutex, OnceLock};

/// What is known about a caller, and the number standing for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// The number every other line refers to it by.
    pub id: usize,
    /// The tailnet login, or the local user when there is no proxy in front.
    pub name: String,
    /// The address the request came from.
    pub ip: String,
    /// The browser it was made with, verbatim.
    pub useragent: String,
}

/// The callers seen so far, by the headers that identify them.
fn seen() -> &'static Mutex<HashMap<(String, String, String), Client>> {
    static SEEN: OnceLock<Mutex<HashMap<(String, String, String), Client>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// The number to give the next caller that is new.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The caller a request comes from, first sight or not.
///
/// A caller that has not been seen before is given a number and announced as
/// `client_identified`. One that has is returned as it was, so that the number
/// stays with it for as long as the server runs.
pub fn identify(headers: &hyper::HeaderMap, peer: &SocketAddr) -> Client {
    let key = (name_of(headers), ip_of(headers, peer), agent_of(headers));
    let mut seen = seen().lock().unwrap();
    if let Some(client) = seen.get(&key) {
        return client.clone();
    }
    let client = Client {
        id: NEXT.fetch_add(1, SeqCst),
        name: key.0.clone(),
        ip: key.1.clone(),
        useragent: key.2.clone(),
    };
    seen.insert(key, client.clone());
    // Announced while the registry is held, so that two requests arriving at
    // once cannot both announce the same caller.
    tinymist_project::announce(
        "client_identified",
        &[
            ("client_id", client.id.into()),
            ("name", client.name.clone().into()),
            ("ip", client.ip.clone().into()),
            ("useragent", client.useragent.clone().into()),
        ],
    );
    client
}

/// Who the request says it is.
///
/// From the same header the annotations take their author from: behind
/// `tailscale serve` that is the tailnet login, and on loopback it is whoever
/// is running the server.
fn name_of(headers: &hyper::HeaderMap) -> String {
    super::annotations::author_or_local(super::http::request_author(headers).as_deref())
}

/// The address the request came from: a proxy's own, if one forwarded it, else
/// the connection's far end.
fn ip_of(headers: &hyper::HeaderMap, peer: &SocketAddr) -> String {
    headers
        .get("X-Forwarded-For")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|| peer.ip().to_string())
}

/// The browser the request was made with.
fn agent_of(headers: &hyper::HeaderMap) -> String {
    headers
        .get(hyper::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(agent: &str, forwarded: Option<&str>) -> hyper::HeaderMap {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(hyper::header::USER_AGENT, agent.parse().unwrap());
        if let Some(address) = forwarded {
            headers.insert("X-Forwarded-For", address.parse().unwrap());
        }
        headers
    }

    fn peer() -> SocketAddr {
        "127.0.0.1:5000".parse().unwrap()
    }

    #[test]
    fn the_same_caller_keeps_its_number() {
        let first = identify(&request("Safari", None), &peer());
        let again = identify(&request("Safari", None), &peer());
        assert_eq!(first.id, again.id);
    }

    #[test]
    fn a_different_browser_is_a_different_caller() {
        let safari = identify(&request("Safari/2", None), &peer());
        let chrome = identify(&request("Chrome/2", None), &peer());
        assert_ne!(safari.id, chrome.id);
    }

    #[test]
    fn a_proxy_says_where_the_request_really_came_from() {
        let client = identify(&request("Safari/3", Some("100.64.0.7, 10.0.0.1")), &peer());
        assert_eq!(client.ip, "100.64.0.7");
    }
}
