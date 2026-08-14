//! `talimist mcp`: the address agents are given, and the thing that answers it.
//!
//! One of these answers for the whole machine, whatever is being served and
//! from wherever: an agent is told about it once, and it finds the rest.
//!
//! Run it and it makes sure that one is listening — starting it detached if it
//! is not — and says which. Running it again does nothing and says so, which is
//! what makes it safe to put in a session hook: every agent session begins by
//! making sure, and the second one through the door pays nothing.

use clap::Parser;
use tinymist::tool::serve::hub;
use tinymist_std::error::prelude::*;

use crate::utils::block_on;

#[derive(Debug, Clone, Parser)]
pub struct McpArgs {
    /// The port to answer on. One fixed address is the point of this, so it is
    /// only worth changing when something else already has that port.
    #[clap(long = "port", default_value_t = hub::HUB_PORT, value_name = "PORT")]
    pub port: u16,

    /// Print how to tell an agent about this, and what to put in a session
    /// hook so it is always there, then exit.
    #[clap(long = "print-config")]
    pub print_config: bool,

    /// Answer on stdin and stdout, for a client that starts its tools itself.
    /// This is how Claude Code and its like connect: they spawn the command
    /// and speak to it directly, so nothing has to be listening beforehand and
    /// nothing is left running afterwards.
    #[clap(long = "stdio")]
    pub stdio: bool,

    /// Answer here rather than starting something that will. This is what the
    /// detached process runs; there is rarely a reason to say it by hand.
    #[clap(long = "serve", hide = true)]
    pub serve: bool,
}

/// The lines that connect an agent to this, once and for all.
fn print_config(port: u16) {
    let url = format!("http://127.0.0.1:{port}/m/");
    println!("The MCP tool agents use for annotations. It dispatches to whichever");
    println!("talimist server is handling a given document or path, so it is registered");
    println!("once and never again:");
    println!();
    println!("  claude mcp add --transport stdio --scope user talimist -- talimist mcp --stdio");
    println!();
    println!("The client starts it with each session and it goes when the session does.");
    println!("Nothing needs to be listening beforehand.");
    println!();
    println!("For a client that wants an address instead, there is one — started by hand");
    println!("or from a session hook, since a client will not start it for you:");
    println!();
    println!("  talimist mcp                     # starts it if it is not up");
    println!("  claude mcp add --transport http --scope user talimist {url}");
    println!();
    println!("Documents are served per project, on ports of their own. This endpoint");
    println!("finds all of them, and an agent says which it means by name:");
    println!();
    println!("  talimist serve --anno --mcp ~/some/docs");
    println!("  talimist serve --anno --mcp ~/another/paper.typ");
}

/// Entry point of `talimist mcp`.
pub fn mcp_main(args: McpArgs) -> Result<()> {
    if args.print_config {
        print_config(args.port);
        return Ok(());
    }

    if args.stdio {
        // Logs would be read as protocol on stdout, so everything says what it
        // has to say on stderr, where the client shows it as the server's log.
        let _ = tinymist::init_log(tinymist::InitLogOpts {
            verbose: false,
            filter: None,
            output: None,
        });
        return hub::serve_stdio().with_context("cannot answer on stdio", || None);
    }

    if args.serve {
        // The detached half: this one answers until it is killed.
        tinymist::tool::preview::note_build_stamp();
        return block_on(async move {
            hub::serve(args.port)
                .await
                .with_context("cannot serve the agent endpoint", || None)
        });
    }

    let url = format!("http://127.0.0.1:{}/m/", args.port);
    match hub::ensure_running(args.port) {
        Ok(true) => println!("talimist mcp started at {url}"),
        Ok(false) => println!("talimist mcp already running at {url}"),
        Err(err) => bail!("cannot start talimist mcp: {err}"),
    }
    Ok(())
}
