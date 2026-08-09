//! Xana runtime package.
//!
//! This package owns state, policy, persistence, services, and provider
//! adapters. The `xana-cli` workspace member is the preferred installed
//! binary; this library also exposes a small compatibility runner for source
//! checkouts and integration tests.

mod agent;
mod app;
mod artifact;
mod bounded_file;
pub mod capability;
mod cli;
mod config;
mod context;
mod credential;
pub mod documents;
mod frontend;
mod identity;
mod init;
mod managed;
mod managed_terminal;
mod message;
mod model;
mod operation;
mod orchestration;
mod paths;
mod permission;
mod presentation;
mod process_capture;
mod prompt;
mod provider;
mod reset;
mod runtime;
pub mod self_docs;
mod session;
mod shell;
mod terminal;
mod tool;
mod vision;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use paths::XanaPaths;

/// Run the process-bound command using the runtime's stable CLI adapter.
pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not create Xana runtime")?;
    runtime.block_on(async {
        let cli = Cli::parse();
        let paths = XanaPaths::resolve(std::env::var_os("XANA_HOME"))
            .context("could not resolve Xana paths")?;
        app::run(cli, paths).await
    })
}
