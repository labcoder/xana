//! Xana application package.
//!
//! This package owns the headless agent, application policy, persistence,
//! services, provider adapters, and current terminal frontends. Its library
//! entry point exists for the package's executable and integration tests; it
//! is not a stable public SDK boundary.

mod a2a;
mod agent;
mod app;
mod artifact;
mod bounded_file;
mod capability;
mod cli;
mod config;
mod config_edit;
mod config_migration;
mod context;
mod credential;
mod diagnostics;
mod doctor;
mod documents;
mod focused_service;
mod frontend;
mod identity;
mod init;
mod local_host;
mod managed;
mod managed_execution;
mod mcp;
mod message;
mod model_catalog;
mod native_runtime;
mod oneshot;
mod operation;
mod orchestration;
mod outbound;
mod paths;
mod permission;
mod plain_terminal;
mod plugin;
mod portable_project;
mod presentation;
mod private_state;
mod process_capture;
mod profile;
mod project;
mod prompt;
mod provider;
mod reset;
mod self_docs;
mod session;
mod setup;
mod shell;
mod skill;
mod sse;
mod tool;
mod tui;
mod vision;
mod workspace_host;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use paths::XanaPaths;
use std::process::ExitCode;

const APPLICATION_STACK_BYTES: usize = 4 * 1024 * 1024;

fn run_cli(cli: Cli) -> Result<()> {
    run_on_application_stack(move || run_cli_on_application_thread(cli))?
}

fn run_cli_on_application_thread(mut cli: Cli) -> Result<()> {
    app::one_shot::preflight(&mut cli)?;
    let paths = XanaPaths::resolve(std::env::var_os("XANA_HOME"))
        .context("could not resolve Xana paths")?;
    let diagnostics_read_only = match &cli.command {
        Some(cli::Command::Doctor(_) | cli::Command::Logs(_) | cli::Command::Config(_)) => true,
        Some(cli::Command::Setup(args)) => args.if_needed || args.dry_run,
        Some(cli::Command::Reset(args)) => args.dry_run,
        _ => false,
    };
    let diagnostic_runtime = if diagnostics_read_only {
        None
    } else {
        match diagnostics::DiagnosticRuntime::start(&paths) {
            Ok(runtime) => runtime,
            Err(_) => {
                eprintln!(
                    "warning: Xana diagnostics are unavailable; run `xana doctor` for the local path and permission checks"
                );
                None
            }
        }
    };
    if let Some(runtime) = &diagnostic_runtime {
        if runtime.stale_markers() > 0 {
            eprintln!(
                "warning: Xana found {} prior unclean-exit marker(s); run `xana logs list` and `xana doctor`",
                runtime.stale_markers()
            );
        }
        diagnostics::emit(diagnostics::DiagnosticFact::new(
            config::DiagnosticLevel::Info,
            config::DiagnosticTarget::Application,
            diagnostics::EventKind::ApplicationStarted,
            diagnostics::EventOutcome::Started,
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not create Xana runtime")?;
    let result = runtime.block_on(app::run(cli, paths));
    diagnostics::emit(diagnostics::DiagnosticFact::new(
        if result.is_ok() {
            config::DiagnosticLevel::Info
        } else {
            config::DiagnosticLevel::Error
        },
        config::DiagnosticTarget::Application,
        if result.is_ok() {
            diagnostics::EventKind::ApplicationStopped
        } else {
            diagnostics::EventKind::ApplicationFailed
        },
        if result.is_ok() {
            diagnostics::EventOutcome::Completed
        } else {
            diagnostics::EventOutcome::Failed
        },
    ));
    drop(diagnostic_runtime);
    result
}

fn run_on_application_stack<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let thread = std::thread::Builder::new()
        .name("xana-application".to_owned())
        .stack_size(APPLICATION_STACK_BYTES)
        .spawn(operation)
        .context("could not create Xana application thread")?;
    match thread.join() {
        Ok(result) => Ok(result),
        Err(payload) => std::panic::resume_unwind(payload),
    }
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
            if let Some(pending) = error.downcast_ref::<setup::SetupPending>() {
                return pending.exit_code();
            }
            eprintln!("Error: {error:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[inline(never)]
    fn debug_stack_probe() -> usize {
        let mut bytes = [0_u8; 2 * 1024 * 1024];
        bytes[0] = 1;
        bytes[bytes.len() - 1] = 2;
        std::hint::black_box(&mut bytes);
        usize::from(bytes[0]) + usize::from(bytes[bytes.len() - 1])
    }

    #[test]
    fn application_entry_has_bounded_debug_stack_headroom() {
        let (name, total) = run_on_application_stack(|| {
            (
                std::thread::current().name().map(str::to_owned),
                debug_stack_probe(),
            )
        })
        .unwrap();
        assert_eq!(name.as_deref(), Some("xana-application"));
        assert_eq!(total, 3);
    }
}
