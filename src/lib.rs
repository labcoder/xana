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
mod oneshot;
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
mod workspace_host;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use paths::XanaPaths;
use std::process::ExitCode;

/// Run the process-bound command using the runtime's stable CLI adapter.
pub fn run() -> Result<()> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not create Xana runtime")?;
    runtime.block_on(async {
        let paths = XanaPaths::resolve(std::env::var_os("XANA_HOME"))
            .context("could not resolve Xana paths")?;
        app::run(cli, paths).await
    })
}

/// Run Xana as a process and return its stable process status.
pub fn entry() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return if code == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(u8::try_from(code).unwrap_or(2))
            };
        }
    };
    match run_cli(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<oneshot::OneShotFailure>()
                && failure.rendered
            {
                return failure.exit_code();
            }
            eprintln!("Error: {error:#}");
            ExitCode::from(1)
        }
    }
}
