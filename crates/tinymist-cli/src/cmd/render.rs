//! `talimist render`: what a server would send, without a server.
//!
//! The document server and the editor's previewer both show a rendering of a
//! document, made by `tool/render`. Everything that goes wrong in one of them —
//! a diagram the HTML exporter dropped, an element with no source range on it,
//! a drawing that framed at the wrong size — goes wrong in the rendering, and
//! chasing it through a browser means a compile, a server, a page and a
//! reload before there is anything to look at.
//!
//! This prints the rendering instead. It is a debugging aid, and the second
//! caller the render layer needs: a library with one consumer drifts back into
//! being part of that consumer.

use std::path::PathBuf;

use tinymist::CompileOnceArgs;
use tinymist::tool::render::html;
use tinymist_project::{CompiledArtifact, WorldComputeGraph, WorldProvider};
use tinymist_std::error::prelude::*;

/// What to render, and as what.
#[derive(Debug, Clone, clap::Parser)]
pub struct RenderArgs {
    /// The document, and where its project is.
    #[clap(flatten)]
    pub compile: CompileOnceArgs,

    /// What to print.
    #[clap(long = "format", default_value = "html", value_name = "WHAT")]
    pub format: RenderFormat,

    /// Where to write it. Standard output when absent — for `body`, that is
    /// what a page fetches, so it can be diffed between compiles.
    #[clap(long = "output", short = 'o', value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Write every framed drawing to this directory instead, one SVG each,
    /// named by the hash a capture would be stored under.
    #[clap(long = "captures", value_name = "DIR")]
    pub captures: Option<PathBuf>,
}

/// The renderings this can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RenderFormat {
    /// The whole page: the shell a browser is given, with the document in it.
    Html,
    /// The document alone, as the page fetches it, labelled with source ranges.
    Body,
    /// The map that says what the rendering was made of: which stretch of
    /// which file each part of it came from.
    Map,
}

/// Renders a document and prints it.
pub fn render_main(args: RenderArgs) -> Result<()> {
    let universe = args.compile.resolve()?;
    let mut universe = universe;
    // The same shims a served document is compiled through: without them this
    // would print what Typst's own HTML export produces, which is a different
    // thing and not the one anybody is debugging.
    if let Err(err) = html::install_shims(&mut universe) {
        log::warn!("rendering without the HTML export shims: {err}");
    }
    let world = universe.snapshot();
    let graph = WorldComputeGraph::from_world(world);
    let art = CompiledArtifact::from_graph(graph, true);

    // Errors are printed and rendering goes on: a document that fails to
    // compile still has a rendering of whatever it managed, which is usually
    // the thing being looked at.
    let diag = art.diagnostics();
    tinymist::project::system::print_diagnostics(
        art.world(),
        diag,
        reflexo_typst::DiagnosticFormat::Human,
    )
    .context_ut("cannot print diagnostics")?;

    let (document, map) = html::html_document_with_map(&art)
        .map_err(|err| error_once!("cannot render", err: err))?;
    let fragment = html::fragment(&document);

    if let Some(dir) = &args.captures {
        std::fs::create_dir_all(dir).context("cannot make the captures directory")?;
        let drawings = html::framed_drawings(&fragment.body);
        for drawing in &drawings {
            let hash = html::hash_of(drawing.svg.as_bytes());
            let path = dir.join(format!("{hash}.svg"));
            std::fs::write(&path, &drawing.svg).context("cannot write a capture")?;
            let (width, height) = html::pixel_size(&drawing.svg).unwrap_or((0, 0));
            println!(
                "{} · {width}×{height} · from {}–{}",
                path.display(),
                drawing.range.start,
                drawing.range.end
            );
        }
        if drawings.is_empty() {
            println!("no framed drawings in this document");
        }
        return Ok(());
    }

    let out = match args.format {
        RenderFormat::Html => {
            // A file to open, not a page to be fetched into: the document goes
            // in where the client would have put it, the stylesheet comes with
            // it, and the client itself does not — it would ask a server that
            // is not there. Without the stylesheet this would be the document
            // with none of the rules that lay it out, which is a different
            // thing to be looking at and the wrong one to debug.
            let title = if fragment.title.is_empty() {
                "document".to_owned()
            } else {
                fragment.title.clone()
            };
            html::shell_html()
                .replace(
                    "<link rel=\"stylesheet\" href=\"api/html/annotate.css\" />",
                    &format!("<style>\n{}\n</style>", html::client_css()),
                )
                .replace("<script src=\"api/html/annotate.js\"></script>", "")
                .replace("<title>Typst</title>", &format!("<title>{title}</title>"))
                .replace(
                    "<main id=\"tinymist-doc\" class=\"tm-doc\"></main>",
                    &format!(
                        "<main id=\"tinymist-doc\" class=\"tm-doc\">{}</main>",
                        fragment.body
                    ),
                )
        }
        RenderFormat::Body => fragment.body,
        RenderFormat::Map => map.to_json(),
    };

    match &args.output {
        Some(path) => {
            std::fs::write(path, &out).context("cannot write the rendering")?;
            eprintln!("wrote {} ({} bytes)", path.display(), out.len());
        }
        None => println!("{out}"),
    }
    Ok(())
}
