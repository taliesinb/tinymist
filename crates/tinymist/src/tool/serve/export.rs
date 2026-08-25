//! The document itself, rather than a page for reading it.
//!
//! A served document has a URL, and what that URL answers with is a page: the
//! viewer or the annotator, which fetch the rendering and draw it. `?fmt=`
//! answers with the document instead — its source, a PDF of it, or a standalone
//! HTML file — so that the same address serves both the reader and whoever
//! wants the thing itself.
//!
//! Nothing here is cached. Each of these is asked for rarely, and a stale
//! answer to "give me the document" is worse than a slow one.

use std::path::Path;

/// What a URL can ask a document for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The Typst source, as it is on disk.
    Typ,
    /// A standalone HTML file: the rendering, with nothing fetched from
    /// anywhere and no annotator in it.
    Html,
    /// The document laid out in pages.
    Pdf,
    /// The annotation sidecar, as it is on disk.
    Annos,
}

impl Format {
    /// The format a `fmt` value names.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "typ" | "typst" | "source" => Ok(Self::Typ),
            "html" => Ok(Self::Html),
            "pdf" => Ok(Self::Pdf),
            // Spelled as the file is, and as it is spoken of.
            "anno.json" | "annos.json" | "annos" => Ok(Self::Annos),
            other => Err(format!(
                "cannot give you {other}: fmt is typ, html, pdf or anno.json\n"
            )),
        }
    }

    /// The extension a saved file takes.
    fn extension(self) -> &'static str {
        match self {
            Self::Typ => "typ",
            Self::Html => "html",
            Self::Pdf => "pdf",
            Self::Annos => "annos.json",
        }
    }

    /// What the answer says it is.
    fn mime(self) -> &'static str {
        match self {
            // Read rather than run: a browser asked for the source shows it.
            Self::Typ => "text/plain; charset=utf-8",
            Self::Html => "text/html; charset=utf-8",
            Self::Pdf => "application/pdf",
            Self::Annos => "application/json; charset=utf-8",
        }
    }
}

/// The document in one of its formats: the bytes, what they are, and the name
/// they should be saved under.
pub struct Export {
    /// The file itself.
    pub bytes: Vec<u8>,
    /// Its content type.
    pub mime: &'static str,
    /// What a browser saving it should call it.
    pub filename: String,
}

/// The value of a query parameter, if the query has one.
///
/// Written out rather than parsed into a map: two values are read from a query
/// in this server, and both are read once.
pub fn query_value(query: Option<&str>, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix(prefix.as_str()))
        .map(|value| super::http::unescape(value, true))
}

/// The document as the format asks for it.
///
/// `rendered` is the HTML the server already has, since the document is
/// compiled for the page anyway and compiling it again to answer this would be
/// answering with a different rendering.
pub fn export(
    format: Format,
    document: &Path,
    rendered: Option<&str>,
) -> Result<Export, String> {
    let stem = document
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_owned());
    let bytes = match format {
        Format::Typ => std::fs::read(document)
            .map_err(|err| format!("cannot read {}: {err}\n", document.display()))?,
        Format::Html => {
            let html = rendered.ok_or("nothing compiled yet\n")?;
            standalone(html).into_bytes()
        }
        Format::Pdf => pdf(document)?,
        // A document nobody has annotated has an empty sidecar rather than no
        // sidecar: whoever asked wants something to read, and "no annotations"
        // is an answer.
        Format::Annos => {
            let path = tinymist_annos::sidecar_path(document);
            match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    let empty = tinymist_annos::Sidecar::default();
                    let mut json = serde_json::to_string_pretty(&empty)
                        .map_err(|err| format!("cannot write an empty sidecar: {err}\n"))?;
                    json.push('\n');
                    json.into_bytes()
                }
                Err(err) => return Err(format!("cannot read {}: {err}\n", path.display())),
            }
        }
    };
    Ok(Export {
        bytes,
        mime: format.mime(),
        filename: format!("{stem}.{}", format.extension()),
    })
}

/// The rendering as a file that stands on its own.
///
/// The exporter already writes one: it puts its stylesheet in a `<style>` and
/// every picture in a `data:` URL, so the page fetches nothing. What it also
/// writes is the bookkeeping this server asked for — the element numbers and
/// source ranges the annotator reads — which means nothing outside the
/// annotator and is taken off.
///
/// The annotator's own script and stylesheet are not in here to be taken off:
/// they are named by the page that the document is fetched into, and this is
/// not that page.
pub fn standalone(rendered: &str) -> String {
    super::capture::plain_svg(rendered)
}

/// The document laid out in pages.
///
/// Compiled again rather than converted: the server compiles for HTML, and the
/// two targets are different documents — a Typst document may say `#if
/// target() == "html"` and mean it.
fn pdf(document: &Path) -> Result<Vec<u8>, String> {
    use reflexo_typst::TypstDocument;
    use tinymist_project::{CompiledArtifact, WorldComputeGraph};

    let root = document.parent().map(Path::to_path_buf);
    let args = crate::CompileOnceArgs {
        input: Some(document.display().to_string()),
        root,
        ..Default::default()
    };
    let universe = tinymist_project::WorldProvider::resolve(&args)
        .map_err(|err| format!("cannot read the project: {err}\n"))?;
    let graph = WorldComputeGraph::from_world(universe.snapshot());
    let art = CompiledArtifact::from_graph(graph, false);
    let Some(TypstDocument::Paged(paged)) = art.success_doc() else {
        let said: Vec<String> = art
            .diagnostics()
            .map(|diag| diag.message.to_string())
            .collect();
        return Err(if said.is_empty() {
            "the document does not compile to pages\n".to_owned()
        } else {
            format!("{}\n", said.join("\n"))
        });
    };
    typst_pdf::pdf(&paged, &typst_pdf::PdfOptions::default())
        .map_err(|err| format!("cannot make a PDF of the document: {err:?}\n"))
}

#[cfg(test)]
mod tests {
    use super::{query_value, Format};

    #[test]
    fn a_format_is_read_from_the_query() {
        assert_eq!(query_value(Some("fmt=pdf"), "fmt").as_deref(), Some("pdf"));
        assert_eq!(
            query_value(Some("doc=paper&fmt=typ"), "fmt").as_deref(),
            Some("typ")
        );
        assert_eq!(query_value(Some("doc=paper"), "fmt"), None);
        assert_eq!(query_value(None, "fmt"), None);
    }

    #[test]
    fn the_formats_are_the_three_it_says() {
        assert_eq!(Format::parse("typ"), Ok(Format::Typ));
        assert_eq!(Format::parse("html"), Ok(Format::Html));
        assert_eq!(Format::parse("pdf"), Ok(Format::Pdf));
        assert_eq!(Format::parse("anno.json"), Ok(Format::Annos));
        assert!(Format::parse("docx").is_err());
    }

    #[test]
    fn a_standalone_page_keeps_the_document_and_drops_the_bookkeeping() {
        let rendered = concat!(
            r#"<html><head><style>p{margin:0}</style></head><body>"#,
            r#"<p id="n7" data-uid="n7" data-typst-src="4:9">Hello</p>"#,
            r#"<img src="data:image/png;base64,AAAA"></body></html>"#,
        );
        let out = super::standalone(rendered);
        assert!(out.contains("<style>p{margin:0}</style>"), "{out}");
        assert!(out.contains("<p>Hello</p>"), "{out}");
        // The picture is in the file, not fetched from this server.
        assert!(out.contains("data:image/png;base64,AAAA"), "{out}");
        assert!(!out.contains("data-uid"), "{out}");
        assert!(!out.contains("data-typst-src"), "{out}");
    }
}
