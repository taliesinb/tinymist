//! Rendering a fragment of Typst beside a document, and deleting it afterwards.
//!
//! A Typst document is source. Nothing in these tools shows what a change would
//! look like, so an agent editing a table or a figure has to write the change
//! into the document and ask somebody to look at it. This compiles a fragment
//! on its own and returns the rendering, so the change can be checked first.
//!
//! The fragment is written beside the document rather than in a temporary
//! directory, because it is compiled with the document's root: `#import
//! "style.typ"` and any relative path to data or images must resolve. The file
//! is deleted after the compile, whether or not the compile succeeded.

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

/// A file to delete when this value goes out of scope.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Renders a fragment and reports the result.
///
/// Diagnostics are returned whether or not the compile succeeded, since the
/// error message is the useful part of a failed compile.
pub fn render(document: &Path, source: &str, format: &str) -> Result<serde_json::Value, String> {
    let format = known(format)?;
    let dir = document
        .parent()
        .map(Path::to_path_buf)
        .ok_or("the document has no directory")?;
    // The document's own directory is the root, which is what makes a relative
    // import in the fragment resolve the same way it does in the document.
    let root = Some(dir.clone());
    // The name carries the process id and the time, so that two calls at once
    // write two files. A leading dot keeps it out of directory listings.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!(".snippet-{}-{stamp}.typ", std::process::id()));
    std::fs::write(&path, source)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    let _scratch = Scratch(path.clone());

    // The document's root, so the fragment can read the same files the document
    // can: its style file, its data files, its images.
    let args = crate::CompileOnceArgs {
        input: Some(path.display().to_string()),
        root: root.clone(),
        ..Default::default()
    };
    let mut universe = tinymist_project::WorldProvider::resolve(&args)
        .map_err(|err| format!("cannot read the project: {err}"))?;
    let html_out = format == "html";
    if html_out {
        // The same shims a served document is compiled with, so that the
        // fragment renders the way it would in the document.
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
                // Under `image`, which the tool layer turns into an image
                // content block rather than base64 inside the text.
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
