//! The web app a served page presents itself as: its name, its icon, its
//! manifest, and the prefix its URLs live under.
//!
//! A page opened from a document server and a page opened from an editor's
//! previewer are both installed by a browser as apps, and both want a name in
//! the dock and an icon that says which of several they are. Neither owns this,
//! so it sits beside them.

pub mod icons;

use icons::IconRole;

/// What a server calls itself in the Dock: its role, and the two things an
/// operator can override — the colour and the name.
#[derive(Debug, Clone)]
pub struct WebAppIdentity {
    /// Which glyph the icon wears.
    pub role: IconRole,
    /// The tile colour, if one was chosen; otherwise derived from the port.
    pub color: Option<[u8; 3]>,
    /// What the server serves, as a person would name it.
    pub name: Option<String>,
}

impl WebAppIdentity {
    /// An identity for a role, with nothing overridden.
    pub fn new(role: IconRole) -> Self {
        Self { role, color: None, name: None }
    }

    /// The identity of a page, which may be the annotating face of a server
    /// that is otherwise a plain one.
    pub fn with_role(&self, role: IconRole) -> Self {
        Self { role, ..self.clone() }
    }

    /// The identity of one document out of several: a directory's server is
    /// named after the directory, and each page in it after its own document.
    pub fn with_name(&self, name: String) -> Self {
        Self { name: Some(name), ..self.clone() }
    }

    /// What is being served, as a person would name it.
    fn subject(&self, port: u16) -> String {
        self.name.clone().unwrap_or_else(|| port.to_string())
    }

    /// The long name, for the browser's tab and the manifest's `name`.
    pub fn title(&self, port: u16) -> String {
        format!("{}: {}", self.role.title(), self.subject(port))
    }

    /// The name a Dock app takes, which is the manifest's `short_name`.
    ///
    /// Subject first, because that is what distinguishes one app from the next
    /// once there are several — and a plain document server needs no suffix at
    /// all, being the ordinary way to look at a document.
    pub fn short_title(&self, port: u16) -> String {
        let subject = self.subject(port);
        match self.role {
            IconRole::Serve => subject,
            IconRole::Lsp => format!("{subject} (LSP)"),
            IconRole::Annotate => format!("{subject} (Annotator)"),
        }
    }
}

/// A stamp for the running binary: when it was last written. Two servers with
/// the same stamp are the same build; a different one means the binary has
/// been replaced since, and the older server is on its way out.
/// Reads it now, while the binary on disk is still the one running: asked for
/// the first time after a rebuild, the answer would be the *new* binary's
/// stamp, and a server on its way out would claim to be the one taking over.
pub fn note_build_stamp() {
    let _ = build_stamp();
}

pub fn build_stamp() -> String {
    use std::time::UNIX_EPOCH;
    static STAMP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STAMP
        .get_or_init(|| {
            std::env::current_exe()
                .and_then(std::fs::metadata)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_nanos().to_string())
                .unwrap_or_else(|| "unknown".into())
        })
        .clone()
}

/// Where each mode lives, as the first segment of every URL it serves: `/p/`
/// for the editor's preview, `/v/` for a document served to be read, `/a/` for
/// one served to be annotated.
///
/// A prefix rather than a page, because an installed web app matches links
/// against its manifest's scope: everything under `/a/` belongs to the
/// annotator, whatever document it names, so a document opened from a listing
/// lands in the same app as one opened directly. The document is the rest of
/// the path, which is how one server comes to serve a whole directory.
pub fn role_prefix(role: IconRole) -> &'static str {
    match role {
        IconRole::Lsp => "/p/",
        IconRole::Serve => "/v/",
        IconRole::Annotate => "/a/",
    }
}

/// The mode a path asks for, and the document under it, if it names one at
/// all. `/a` and `/a/` both mean the annotator's own front page.
pub fn role_of_path(path: &str) -> Option<(IconRole, &str)> {
    for role in [
        IconRole::Lsp,
        IconRole::Serve,
        IconRole::Annotate,
    ] {
        let prefix = role_prefix(role);
        if path == prefix.trim_end_matches('/') {
            return Some((role, ""));
        }
        if let Some(rest) = path.strip_prefix(prefix) {
            return Some((role, rest));
        }
    }
    None
}

/// The web-app furniture: each mode is a page of its own, with its own name,
/// icon and manifest, so both can live in the Dock side by side. Safari takes
/// the name, start URL, icons and scope from the manifest when there is one,
/// and treats in-scope links as belonging to that app — which is what the
/// per-mode prefix is for.
pub fn mode_head(html: &str, identity: &WebAppIdentity, port: u16) -> String {
    let prefix = role_prefix(identity.role);
    let manifest = format!("{prefix}manifest.webmanifest");
    let (manifest, icon) = match identity.role {
        IconRole::Lsp => (manifest.as_str(), "/icon/lsp-192.png"),
        IconRole::Serve => (manifest.as_str(), "/icon/serve-192.png"),
        IconRole::Annotate => (manifest.as_str(), "/icon/anno-192.png"),
    };
    let title = identity.title(port);
    let head = format!(
        "<title>{title}</title>\
         <link rel=\"manifest\" href=\"{manifest}\">\
         <link rel=\"apple-touch-icon\" href=\"{icon}\">\
         <link rel=\"icon\" type=\"image/png\" href=\"{icon}\">"
    );

    // The bundled frontend ships its own title and icon; a browser takes the
    // last icon it is offered, so ours has to both replace theirs and come
    // last. Strip, then append at the end of the head.
    let html = strip_tags(html, &["<title>"], &["</title>"]);
    let html = strip_icon_links(&html);
    match html.find("</head>") {
        Some(at) => {
            let mut out = String::with_capacity(html.len() + head.len());
            out.push_str(&html[..at]);
            out.push_str(&head);
            out.push_str(&html[at..]);
            out
        }
        None => format!("{head}{html}"),
    }
}

/// Removes every `open..close` span from `html`.
fn strip_tags(html: &str, open: &[&str], close: &[&str]) -> String {
    let mut out = html.to_string();
    for (open, close) in open.iter().zip(close) {
        while let Some(start) = out.find(open) {
            let Some(end) = out[start..].find(close) else {
                break;
            };
            out.replace_range(start..start + end + close.len(), "");
        }
    }
    out
}

/// Removes the `<link rel="icon">` and friends a page declares for itself.
fn strip_icon_links(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<link") {
        let Some(len) = rest[at..].find('>') else {
            break;
        };
        let tag = &rest[at..at + len + 1];
        let is_icon = tag.contains("rel=\"icon\"")
            || tag.contains("rel=\"shortcut icon\"")
            || tag.contains("rel=\"apple-touch-icon\"");
        out.push_str(&rest[..at]);
        if !is_icon {
            out.push_str(tag);
        }
        rest = &rest[at + len + 1..];
    }
    out.push_str(rest);
    out
}

/// The PNG behind an `/icon/...` path, if it names one of ours.
///
/// Icons are synthesised rather than stored: the glyph comes from the role in
/// the path and the colour from the port this server is bound to, so two
/// servers never wear the same icon in the Dock.
pub fn icon_asset(path: &str, port: u16, identity: &WebAppIdentity) -> Option<Vec<u8>> {
    // Browsers ask for /favicon.ico whatever the page says, and Safari caches
    // what it gets per origin — so leaving this to fall through to the page
    // meant a stale icon stuck to the port.
    if path == "/favicon.ico" || path == "/favicon.png" {
        return icons::icon_png(identity.role, port, 192, identity.color).ok();
    }
    let name = path.strip_prefix("/icon/")?.strip_suffix(".png")?;
    let (role, size) = name.rsplit_once('-')?;
    let role = match role {
        "lsp" => IconRole::Lsp,
        "serve" => IconRole::Serve,
        "anno" => IconRole::Annotate,
        _ => return None,
    };
    let size = match size {
        "192" => 192,
        "512" => 512,
        _ => return None,
    };
    icons::icon_png(role, port, size, identity.color).ok()
}

/// The web app manifest for a mode's path, if it names one.
pub fn web_manifest(path: &str, port: u16, identity: &WebAppIdentity) -> Option<String> {
    // One manifest per mode, at that mode's own prefix, so an installed app's
    // scope is the prefix and every document under it belongs to that app.
    let (identity, start, scope) = match role_of_path(path) {
        Some((role, "manifest.webmanifest")) => {
            let prefix = role_prefix(role);
            (identity.with_role(role), prefix, prefix)
        }
        // The bare one names whatever this server is, for a browser that asks
        // before being redirected into a prefix.
        _ if path == "/manifest.webmanifest" => {
            let prefix = role_prefix(identity.role);
            (identity.clone(), prefix, prefix)
        }
        _ => return None,
    };
    let icon = match identity.role {
        IconRole::Lsp => "lsp",
        IconRole::Serve => "serve",
        IconRole::Annotate => "anno",
    };
    let (bg, _) = icons::colors_for(port, identity.color);
    let background = format!("#{:02x}{:02x}{:02x}", bg[0], bg[1], bg[2]);
    let name = identity.title(port);
    let short = identity.short_title(port);
    Some(format!(
        r##"{{
  "name": "{name}",
  "short_name": "{short}",
  "start_url": "{start}",
  "scope": "{scope}",
  "display": "standalone",
  "background_color": "{background}",
  "icons": [
    {{ "src": "/icon/{icon}-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any maskable" }},
    {{ "src": "/icon/{icon}-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable" }}
  ]
}}
"##
    ))
}
