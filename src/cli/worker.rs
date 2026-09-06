//! Owner-facing retained work syntax; execution remains application policy.
use crate::identity::{AgentId, ArtifactId, SessionId};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Args, PartialEq, Eq)]
pub(crate) struct WorkerArgs {
    #[command(subcommand)]
    pub(crate) command: WorkerCommand,
}

#[derive(Debug, Clone, Args, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WorkerTarget {
    pub(crate) id: AgentId,
    #[arg(long)]
    pub(crate) revision: u64,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkerCommand {
    /// Retain a completed root-owned child, not its process or vendor heap.
    Retain {
        #[arg(long)]
        session: SessionId,
        #[arg(long)]
        agent: AgentId,
        #[arg(long)]
        goal: String,
        #[arg(long)]
        expires: String,
        #[arg(long)]
        evidence: Vec<ArtifactId>,
        #[arg(long)]
        authorize: bool,
    },
    /// Page through at most 32 retained identities.
    List {
        #[arg(long)]
        after: Option<AgentId>,
    },
    Inspect {
        id: AgentId,
    },
    /// Queue one idempotent, bounded owner message; it does not start a model.
    FollowUp {
        #[command(flatten)]
        target: WorkerTarget,
        #[arg(long)]
        request_id: Uuid,
        #[arg(long)]
        text: String,
    },
    /// Execute one queued follow-up through the original bounded child route.
    Run {
        #[command(flatten)]
        target: WorkerTarget,
    },
    /// Refuse new messages and stop after explicitly running the existing queue.
    Drain {
        #[command(flatten)]
        target: WorkerTarget,
    },
    /// Revoke continuation and request stopping any active execution.
    Stop {
        #[command(flatten)]
        target: WorkerTarget,
    },
    /// Review an interrupted attempt without replaying its consumed message.
    Recover {
        #[command(flatten)]
        target: WorkerTarget,
        #[arg(long)]
        review_unknown: bool,
    },
    /// Run a closed deterministic operation over selected immutable evidence.
    Context {
        #[command(flatten)]
        target: WorkerTarget,
        #[arg(long)]
        operation: String,
    },
}
