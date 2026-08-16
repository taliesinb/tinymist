#![doc = include_str!("../README.md")]

mod conn;
mod utils;
mod cmd {
    pub mod annos;
    #[cfg(feature = "export")]
    pub mod compile;
    pub mod completion;
    pub mod cov;
    #[cfg(feature = "dap")]
    pub mod dap;
    pub mod generate_script;
    pub mod lint;
    pub mod lsp;
    #[cfg(feature = "export")]
    pub mod package;
    pub mod procs;
    #[cfg(feature = "preview")]
    pub mod preview;
    #[cfg(feature = "preview")]
    pub mod open_preview;
    #[cfg(feature = "preview")]
    pub mod render;
    #[cfg(feature = "serve")]
    pub mod server;
    #[cfg(feature = "serve")]
    pub mod mcp;
    #[cfg(feature = "serve")]
    pub mod serve;
    pub mod query;
    pub mod test;
    pub mod trace_lsp;

    #[cfg(feature = "lock")]
    pub mod doc;
    #[cfg(feature = "lock")]
    pub mod task;
}

use std::sync::LazyLock;

use clap::Parser;
#[cfg(feature = "l10n")]
use tinymist_l10n::{load_translations, set_translations};
use tinymist_std::error::prelude::*;

use crate::cmd::*;
#[cfg(feature = "export")]
use crate::compile::CompileArgs;
use crate::conn::client_root;
use crate::lint::LintArgs;
use crate::utils::*;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The runtimes used by the application.
pub struct Runtimes {
    /// The tokio runtime.
    pub tokio_runtime: tokio::runtime::Runtime,
}

impl Default for Runtimes {
    fn default() -> Self {
        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        Self { tokio_runtime }
    }
}

static RUNTIMES: LazyLock<Runtimes> = LazyLock::new(Runtimes::default);

#[derive(Debug, Clone, clap::Parser)]
#[clap(name = "talimist", author, version, about, long_version(tinymist::LONG_VERSION.as_str()))]
struct Args {
    /// Configure log filter of tinymist
    #[clap(long = "log-filter", env = "TINYMIST_LOG")]
    pub log_filter: Option<String>,

    /// Mode of the binary
    #[clap(subcommand)]
    pub cmd: Option<Commands>,
}

#[derive(Debug, Clone, clap::Subcommand)]
#[clap(rename_all = "kebab-case")]
enum Commands {
    /// Probe existence (Nop run)
    Probe,

    /// Run language server
    Lsp(crate::lsp::LspArgs),
    /// Run debug adapter
    #[cfg(feature = "dap")]
    Dap(crate::dap::DapArgs),
    /// Run language server for tracing some typst program.
    #[clap(hide(true))]
    TraceLsp(crate::trace_lsp::TraceLspArgs),

    /// Run language query
    #[clap(hide(true))] // still in development
    #[clap(subcommand)]
    Query(crate::query::QueryCommands),
    /// List or stop the servers that are running
    #[clap(subcommand)]
    Procs(crate::procs::ProcsCommands),
    /// Read a document's annotations
    #[clap(subcommand)]
    Annos(crate::annos::AnnosCommands),
    /// Serve a file or directory as HTML
    #[cfg(feature = "serve")]
    Serve(crate::serve::ServeArgs),
    /// Answer agents, at one address, about every document being served
    #[cfg(feature = "serve")]
    Mcp(crate::mcp::McpArgs),
    /// Preview a document as pages, for an editor to follow
    #[cfg(feature = "preview")]
    Preview(tinymist::tool::preview::PreviewCliArgs),
    /// Open the preview the language server is already running for a project
    #[cfg(feature = "preview")]
    OpenPreview(crate::open_preview::OpenPreviewArgs),
    /// Print what a server would send: the rendered document, or its drawings
    #[cfg(feature = "preview")]
    Render(crate::render::RenderArgs),
    /// Run compile command like `typst-cli compile`
    #[cfg(feature = "export")]
    #[clap(alias = "c")]
    Compile(CompileArgs),
    /// Run Tinymist lint checks
    Lint(LintArgs),
    /// Run package tools
    #[cfg(feature = "export")]
    #[clap(subcommand)]
    Package(crate::package::PackageCommands),

    /// Generate completion script to stdout
    Completion(crate::completion::ShellCompletionArgs),
    /// Generate build script for compilation
    #[clap(hide(true))] // still in development
    GenerateScript(crate::generate_script::GenerateScriptArgs),

    /// Run documents
    #[cfg(feature = "lock")]
    #[clap(hide(true))] // still in development
    #[clap(subcommand)]
    Doc(tinymist::project::DocCommands),
    /// Run tasks
    #[cfg(feature = "lock")]
    #[clap(hide(true))] // still in development
    #[clap(subcommand)]
    Task(crate::task::TaskCommands),

    /// Execute a document and collect coverage
    #[clap(hide(true))] // still in development
    Cov(crate::cov::CovArgs),
    /// Test a document and give summary
    Test(crate::test::TestArgs),
}

/// The main entry point.
fn main() -> Result<()> {
    // The root allocator for heap memory profiling.
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    // Parses command line arguments
    let args = Args::parse();
    let cmd = args
        .cmd
        .unwrap_or_else(|| Commands::Lsp(Default::default()));

    // Probes soon to avoid other initializations causing errors
    if matches!(cmd, Commands::Probe) {
        return Ok(());
    }

    // Loads translations
    #[cfg(feature = "l10n")]
    set_translations(load_translations(tinymist_assets::L10N_DATA)?);
    // Starts logging
    let verbose = match &cmd {
        // Short-running commands, usually run from the CLI.
        Commands::Completion(..) | Commands::Probe | Commands::Procs(..) | Commands::Annos(..) => {
            false
        }
        #[cfg(feature = "export")]
        Commands::Compile(..) => false,
        Commands::Lint(..) => false,
        #[cfg(feature = "export")]
        Commands::Package(package::PackageCommands::Docs(args)) => args.watch,

        // Long-running commands, usually run from the CLI.
        Commands::Test(test) => test.verbose,
        #[cfg(feature = "preview")]
        Commands::Preview(preview) => preview.verbose,
        #[cfg(feature = "preview")]
        Commands::Render(..) => false,
        #[cfg(feature = "preview")]
        Commands::OpenPreview(..) => false,
        #[cfg(feature = "serve")]
        Commands::Serve(serve) => serve.verbose,
        #[cfg(feature = "serve")]
        Commands::Mcp(..) => false,

        // Long-running commands, usually run from an editor.
        Commands::Lsp(..) => true,
        #[cfg(feature = "dap")]
        Commands::Dap(..) => true,

        // Hidden commands.
        Commands::TraceLsp(..)
        | Commands::Query(..)
        | Commands::GenerateScript(..)
        | Commands::Cov(..) => true,
        #[cfg(feature = "lock")]
        Commands::Doc(..) => true,
        #[cfg(feature = "lock")]
        Commands::Task(..) => true,
    };
    // Serving prints its own address block; the preview server's own address
    // logging would arrive alongside it as two decoys for the one line worth
    // copying, so that target is turned down unless asked for.
    #[cfg(feature = "serve")]
    let quiet_preview = !verbose && matches!(cmd, Commands::Serve(..));
    #[cfg(not(feature = "serve"))]
    let quiet_preview = false;
    let _ = tinymist::init_log(tinymist::InitLogOpts {
        verbose,
        filter: args.log_filter.or_else(|| {
            quiet_preview.then(|| "tinymist::compat::preview=warn".to_string())
        }),
        output: None,
    });

    // Nothing should outlive the binary it came from: a language server or
    // preview left running after `cargo install` would serve code that no
    // longer exists on disk.
    crate::utils::exit_when_binary_replaced();

    match cmd {
        Commands::Probe => Ok(()),

        Commands::Lsp(args) => crate::lsp::lsp_main(args),
        #[cfg(feature = "dap")]
        Commands::Dap(args) => crate::dap::dap_main(args),
        Commands::TraceLsp(args) => crate::trace_lsp::trace_lsp_main(args),

        Commands::Query(cmds) => crate::query::query_main(cmds),
        Commands::Procs(cmds) => crate::procs::procs_main(cmds),
        Commands::Annos(cmds) => crate::annos::annos_main(cmds),
        #[cfg(feature = "preview")]
        Commands::Render(args) => crate::render::render_main(args),
        #[cfg(feature = "preview")]
        Commands::Preview(args) => block_on(crate::preview::preview_main(args)),
        #[cfg(feature = "preview")]
        Commands::OpenPreview(args) => crate::open_preview::open_preview_main(args),
        #[cfg(feature = "serve")]
        Commands::Serve(args) => crate::serve::serve_main(args),
        #[cfg(feature = "serve")]
        Commands::Mcp(args) => crate::mcp::mcp_main(args),
        #[cfg(feature = "export")]
        Commands::Compile(args) => block_on(crate::compile::compile_main(args)),
        Commands::Lint(args) => crate::lint::lint_main(args),
        #[cfg(feature = "export")]
        Commands::Package(args) => block_on(crate::package::package_main(args)),

        Commands::Completion(args) => crate::completion::completion_main(args),
        Commands::GenerateScript(args) => crate::generate_script::generate_script_main(args),

        #[cfg(feature = "lock")]
        Commands::Doc(cmds) => crate::doc::doc_main(cmds),
        #[cfg(feature = "lock")]
        Commands::Task(cmds) => crate::task::task_main(cmds),

        Commands::Cov(args) => crate::cov::cov_main(args),
        Commands::Test(args) => block_on(crate::test::test_main(args)),
    }
}
