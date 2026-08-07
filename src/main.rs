//! Xana's executable composition root.
//!
//! Process arguments and the optional `XANA_HOME` override enter here. The
//! application module owns command orchestration; headless agent code never
//! reads process-global configuration.

mod agent;
mod app;
mod approval;
mod cli;
mod config;
mod context;
mod init;
mod message;
mod paths;
mod presentation;
mod prompt;
mod provider;
mod shell;
mod terminal;
mod tool;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use paths::XanaPaths;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = XanaPaths::resolve(std::env::var_os("XANA_HOME"))
        .context("could not resolve Xana paths")?;

    app::run(cli, paths)
}
