//! Rendering a fragment of Typst beside a document, and throwing it away.
//!
//! An agent asked to change how something looks has no way to see what it did:
//! the document is source, the reader is a person, and the loop between them is
//! the person. This closes it for the small case — a table, a figure, a piece
//! of markup — by compiling what the agent wrote as if it were part of the
//! document, and answering with the result.
//!
//! Beside the document, not in a temporary directory of its own: a fragment is
//! written against the document's own imports, its own fonts and its own data
//! files, and `#import "style.typ"` has to resolve. The file is deleted as soon
//! as it has been compiled, whether or not it compiled.

use std::path::{Path, PathBuf};

use base64::Engine;
use reflexo_typst::TypstDocument;
use tinymist_project::{CompiledArtifact, WorldComputeGraph};

use crate::tool::render::html;

/// What a snippet can be rendered as.
fn known(format: &str) -> Result<&'static str, String> {
    match format {
        "" | "png" => Ok("png"),
        "pdf" => Ok("pdf"),
        "svg" => Ok("svg"),
        "html" => Ok("html"),
        other => Err(format!(
            "cannot render a snippet as {other}: png, pdf, svg or html"
        )),
    }
}

/// A file beside the document that this process owns and nobody else will
/// mistake for the document's own.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Renders a fragment, and says how it came out.
///
/// The answer carries the diagnostics whether or not it compiled: a snippet
/// that fails is exactly the case an agent wants the message from.
pub fn render(document: &Path, source: &str, format: &str) -> Result<serde_json::Value, String> {
    let format = known(format)?;
    let dir = document
        .parent()
        .map(Path::to_path_buf)
        .ok_or("the document has no directory")?;
    // Where the project starts, so that an import reaching above the
    // document's own directory still resolves.
    let root = Some(dir.clone());
    // Named after this process and the moment: two agents asking at once are
    // two files, and a crash leaves one behind that says what it was.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!(".snippet-{}-{stamp}.typ", std::process::id()));
    std::fs::write(&path, source)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    let _scratch = Scratch(path.clone());

    // Compiled against the document's own root, so that everything the
    // document can read, the snippet can read: its style file, its data, its
    // fonts.
    let args = crate::CompileOnceArgs {
        input: Some(path.display().to_string()),
        root: root.clone(),
        ..Default::default()
    };
    let mut universe = tinymist_project::WorldProvider::resolve(&args)
        .map_err(|err| format!("cannot read the project: {err}"))?;
    let html_out = format == "html";
    if html_out {
        // The same shims a served document is compiled through: a snippet
        // rendered without them is not what the reader would see.
        if let Err(err) = html::install_shims(&mut universe) {
            log::warn!("rendering a snippet without the HTML shims: {err}");
        }
    }
    let world = universe.snapshot();
    let graph = WorldComputeGraph::from_world(world);
    let art = CompiledArtifact::from_graph(graph, html_out);

    let diagnostics: Vec<String> = art
        .diagnostics()
        .map(|diag| diag.message.to_string())
        .collect();
    let Some(doc) = art.success_doc() else {
        return Ok(serde_json::json!({
            "ok": false,
            "format": format,
            "diagnostics": diagnostics,
        }));
    };

    let out = match doc {
        TypstDocument::Html(html_doc) => {
            let body = typst_html::html(&html_doc, &typst_html::HtmlOptions::default())
                .map_err(|err| format!("cannot encode the snippet as HTML: {err:?}"))?;
            serde_json::json!({ "html": html::fragment(&body).body })
        }
        TypstDocument::Paged(paged) => match format {
            "pdf" => {
                let bytes = typst_pdf::pdf(&paged, &typst_pdf::PdfOptions::default())
                    .map_err(|err| format!("cannot make a PDF of the snippet: {err:?}"))?;
                // Under `image`, which is what the tool layer lifts into a
                // block of its own: a picture a model can look at, rather than
                // base64 in the middle of the text.
                serde_json::json!({
                    "image": {
                        "mimeType": "application/pdf",
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    },
                    "bytes": bytes.len(),
                })
            }
            "svg" => serde_json::json!({
                "svg": typst_svg::svg_merged(&paged, &Default::default(), typst::layout::Abs::pt(0.0)),
            }),
            _ => {
                let pixmap = typst_render::render_merged(
                    &paged,
                    &typst_render::RenderOptions {
                        pixel_per_pt: 2.0.into(),
                        ..Default::default()
                    },
                    Default::default(),
                    None,
                );
                let bytes = pixmap
                    .encode_png()
                    .map_err(|err| format!("cannot make a picture of the snippet: {err}"))?;
                serde_json::json!({
                    "image": {
                        "mimeType": "image/png",
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    },
                    "width": pixmap.width(),
                    "height": pixmap.height(),
                })
            }
        },
    };

    let mut answer = serde_json::json!({
        "ok": true,
        "format": format,
        "diagnostics": diagnostics,
    });
    if let (Some(map), Some(extra)) = (answer.as_object_mut(), out.as_object()) {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    Ok(answer)
}
