//! Discoverable typed owner controls; parsing never starts a host or a job.
use clap::{Args, Subcommand};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub(crate) struct AutonomyArgs {
    #[command(subcommand)]
    pub(crate) command: AutonomyCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum AutonomyCommand {
    /// Inspect a bounded protected page. Names and payloads stay local.
    List {
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Inspect exact saved intent, revision, authority, and last receipt.
    Show { id: Uuid },
    /// Read immutable occurrence receipts; never repeats work.
    Receipts {
        id: Uuid,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Create one explicitly authorized reminder or native bounded task.
    Create(Box<CreateTask>),
    /// Pause new admission; an in-flight run is allowed to finish.
    Pause {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Resume reviewed work; unknown prior effects require an extra acknowledgement.
    Resume {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        review_unknown: bool,
    },
    /// Cancel pending work or request bounded interruption of the exact current run.
    Cancel {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Explicit detached lifecycle; OS startup is a separate opt-in.
    Host {
        #[command(subcommand)]
        command: HostCommand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub(crate) struct CreateTask {
    #[arg(long)]
    pub(crate) name: String,
    #[arg(long)]
    pub(crate) workspace: PathBuf,
    #[arg(long)]
    pub(crate) profile: String,
    #[arg(long)]
    pub(crate) project: Option<Uuid>,
    /// Local reminder text: completion creates a durable local inbox receipt.
    #[arg(long, conflicts_with = "prompt", required_unless_present = "prompt")]
    pub(crate) reminder: Option<String>,
    /// Fixed native task; no inherited foreground Conversation or worker.
    #[arg(long, conflicts_with = "reminder")]
    pub(crate) prompt: Option<String>,
    /// Permit only Profile-selected bounded workspace read tools and their disclosure.
    #[arg(long, requires = "prompt")]
    pub(crate) workspace_reads: bool,
    /// One-shot RFC3339 instant including an offset, e.g. 2026-09-06T09:00:00-07:00.
    #[arg(long, conflicts_with = "daily", required_unless_present = "daily")]
    pub(crate) at: Option<String>,
    /// Daily wall time HH:MM; timezone must be an explicit IANA name.
    #[arg(long, requires = "timezone")]
    pub(crate) daily: Option<String>,
    #[arg(long, requires = "daily")]
    pub(crate) timezone: Option<String>,
    /// Mandatory authority expiry as an RFC3339 instant, independent of trigger.
    #[arg(long)]
    pub(crate) expires: String,
    /// Confirm exact task, workspace, Profile, recipient, read ceiling, and budgets.
    #[arg(long)]
    pub(crate) authorize: bool,
    /// Bind a previously inspected non-secret route/policy fingerprint.
    #[arg(long)]
    pub(crate) reviewed_route: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum HostCommand {
    /// Read policy and lock-backed discovery; does not spawn or poll a provider.
    Status,
    /// Enable detached admission and request a hidden independent local process.
    Start {
        #[arg(long)]
        revision: u64,
    },
    /// Stop admission and request bounded shutdown; not a stop acknowledgement.
    Stop {
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        lock: bool,
    },
    /// Disable future launches and request shutdown; preserves jobs and receipts.
    Disable {
        #[arg(long)]
        revision: u64,
    },
    /// Explicitly install or remove this home's per-user login registration.
    Startup {
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        enable: bool,
    },
    /// Run the opted-in host in this process; intended for explicit OS startup registration.
    Run {
        #[arg(long)]
        from_startup: bool,
        #[arg(long, requires = "from_startup")]
        home: Option<PathBuf>,
    },
    /// Attach as a passive observer; Ctrl+C detaches this client, never stops jobs.
    Observe,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    #[test]
    fn controls_are_discoverable_and_startup_home_requires_explicit_mode() {
        let cli = crate::cli::Cli::command();
        let autonomy = cli.find_subcommand("autonomy").unwrap();
        for command in [
            "create", "show", "list", "receipts", "pause", "resume", "cancel", "host",
        ] {
            assert!(autonomy.find_subcommand(command).is_some());
        }
        assert!(
            crate::cli::Cli::try_parse_from([
                "xana",
                "autonomy",
                "host",
                "run",
                "--home",
                "C:/fixture"
            ])
            .is_err()
        );
        let parsed = crate::cli::Cli::try_parse_from([
            "xana",
            "autonomy",
            "host",
            "run",
            "--from-startup",
            "--home",
            "C:/fixture",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(crate::cli::Command::Autonomy(AutonomyArgs {
                command: AutonomyCommand::Host {
                    command: HostCommand::Run {
                        from_startup: true,
                        ..
                    }
                }
            }))
        ));
        assert!(
            crate::cli::Cli::try_parse_from([
                "xana",
                "autonomy",
                "cancel",
                "00000000-0000-0000-0000-000000000001"
            ])
            .is_err()
        );
    }
}
