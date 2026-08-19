//! Putting the pieces a static site needs into it.
//!
//! A site on Netlify is deployed whole, so a page rebuilt since the deploy is
//! served from a blob instead and listed in a patchset the page reads for
//! itself. This writes the three small programs that do that — the patchset,
//! the pages, the annotations — and the script that asks for them, into a
//! site's own tree, where its next deploy will pick them up.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Subcommand)]
pub enum NetlifyCommands {
    /// Write what a site needs into it
    Install(InstallArgs),
    /// Say what would be written, and where
    Show(ShowArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct InstallArgs {
    /// The site's root: where `netlify.toml` is, or would be.
    pub site: PathBuf,
}

#[derive(Debug, Clone, Parser)]
pub struct ShowArgs {
    /// Which file to print, of those `install` writes. Omit for the list.
    pub asset: Option<String>,
}

/// Runs one of them.
pub fn netlify_main(cmd: NetlifyCommands) -> tinymist_std::Result<()> {
    match cmd {
        NetlifyCommands::Install(args) => {
            let written = tinymist_netlify::install(&args.site)
                .map_err(|err| tinymist_std::error_once!("cannot write into the site", err: err))?;
            for path in written {
                println!("{}", path.display());
            }
            println!();
            println!("`netlify/functions` and `netlify/edge-functions` are read from the");
            println!("repository; `netlify/patch.js` has to be in the published directory.");
            println!();
            println!("Add the script to every page:");
            println!("  <script src=\"/netlify/patch.js\" defer></script>");
            println!("  <meta name=\"tm-page\" content=\"THE PAGE'S CONTENT HASH\">");
            println!();
            println!("Then deploy. The functions answer at:");
            println!("  {}", tinymist_netlify::MANIFEST_ROUTE);
            println!("  {}/:hash", tinymist_netlify::PAGE_ROUTE);
            println!("  {}?page=/some/page/", tinymist_netlify::ANNOS_ROUTE);
            Ok(())
        }
        NetlifyCommands::Show(args) => {
            let assets = tinymist_netlify::assets();
            match args.asset {
                None => {
                    for asset in assets {
                        println!("{}", asset.path);
                    }
                }
                Some(wanted) => {
                    let found = assets
                        .into_iter()
                        .find(|asset| asset.path == wanted || asset.path.ends_with(&wanted));
                    match found {
                        Some(asset) => print!("{}", asset.text),
                        None => {
                            return Err(tinymist_std::error_once!("no such file", name: wanted))
                        }
                    }
                }
            }
            Ok(())
        }
    }
}
