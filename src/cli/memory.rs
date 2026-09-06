//! Owner-facing personal memory controls; arguments never grant tool authority.
use crate::memory::MemoryScope;
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub(crate) struct MemoryArgs {
    #[command(subcommand)]
    pub(crate) command: MemoryCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Toggle {
    Off,
    On,
}
impl From<Toggle> for bool {
    fn from(value: Toggle) -> Self {
        matches!(value, Toggle::On)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum MemoryCommand {
    /// Inspect and review learned suggestions; Skill drafts remain inert.
    Candidate {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    /// Inspect bounded pending learning, route and last receipt; no provider call.
    LearningStatus,
    /// Authorize one exact native helper connection/model for personal processing.
    LearningRoute {
        #[arg(long)]
        connection: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        disable: bool,
    },
    /// Process one bounded pending batch using the separately authorized helper.
    Process,
    /// Preview restored-memory eligibility separately from automation review.
    ReviewRestore {
        #[arg(long)]
        review: Option<String>,
    },
    /// Inspect a bounded page, including inactive records; never calls a model.
    List {
        #[arg(long)]
        scope: Option<MemoryScope>,
        #[arg(long)]
        after: Option<u64>,
    },
    /// Inspect one exact current record and its revision.
    Show { id: Uuid },
    /// Explicitly retain a stated fact in the named scope.
    Remember {
        #[arg(long)]
        scope: MemoryScope,
        #[arg(long)]
        text: String,
        #[arg(long)]
        expires_at: Option<u64>,
    },
    /// Correct a reviewed revision; stale edits fail instead of overwriting.
    Correct {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        text: String,
        #[arg(long, conflicts_with = "clear_expiry")]
        expires_at: Option<u64>,
        /// Explicitly remove expiry; otherwise an omitted expiry is preserved.
        #[arg(long)]
        clear_expiry: bool,
    },
    /// Change one record's applicability, only with explicit confirmation.
    Scope {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        to: MemoryScope,
        #[arg(long)]
        confirm: bool,
    },
    /// Make one reviewed record ineligible without claiming secure forgetting.
    Disable {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Forget an exact fact and exclude its sources from automatic reuse.
    Forget {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Explicitly restore a forgotten fact, not its excluded source history.
    Restore {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        confirm: bool,
    },
    /// Preview deletion of one inactive native Conversation's source history.
    DeleteSource {
        conversation: Uuid,
        /// Apply only the exact reviewed preview; omit for a read-only preview.
        #[arg(long)]
        review: Option<String>,
    },
    /// Inspect or edit use, automatic-learning permission and no-memory independently.
    Controls {
        #[arg(long)]
        scope: MemoryScope,
        #[arg(long)]
        revision: Option<u64>,
        #[arg(long = "use", value_enum)]
        use_memory: Option<Toggle>,
        #[arg(long, value_enum)]
        learn: Option<Toggle>,
        #[arg(long, value_enum)]
        no_memory: Option<Toggle>,
    },
    /// Create a readable private JSON copy; existing files are never replaced.
    Export {
        #[arg(long)]
        scope: Option<MemoryScope>,
        #[arg(long)]
        output: PathBuf,
    },
    /// Run an explicit natural-language control locally, without a model call.
    Say {
        #[arg(long)]
        conversation: Option<Uuid>,
        #[arg(long)]
        profile: Option<Uuid>,
        #[arg(long)]
        project: Option<Uuid>,
        request: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum CandidateCommand {
    /// Read a bounded page of candidate metadata.
    List {
        #[arg(long)]
        scope: Option<MemoryScope>,
        #[arg(long)]
        after: Option<u64>,
    },
    /// Inspect the exact proposal, provenance, and current validity.
    Show { id: Uuid },
    /// Compare a proposal with its reviewed base.
    Diff { id: Uuid },
    /// Accept the inspected memory revision, or mark an inert draft reviewed.
    Approve {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        confirm_sensitive: bool,
    },
    /// Reject one inspected revision without changing its target.
    Reject {
        id: Uuid,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        reason: String,
    },
    /// Retain an inactive review record; this is not secure forgetting.
    Archive {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Undo an unchanged approved target; concurrent changes fail closed.
    Undo {
        id: Uuid,
        #[arg(long)]
        revision: u64,
    },
    /// Retain Markdown as an inert draft, never an installed Skill.
    StageSkill {
        #[arg(long)]
        scope: MemoryScope,
        #[arg(long)]
        name: String,
        #[arg(long)]
        markdown: String,
    },
}

#[cfg(test)]
mod candidate_tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser as _;

    #[test]
    fn review_requires_exact_revision_and_never_implies_installation() {
        let id = Uuid::new_v4().to_string();
        assert!(Cli::try_parse_from(["xana", "memory", "candidate", "approve", &id]).is_err());
        let args = Cli::try_parse_from([
            "xana",
            "memory",
            "candidate",
            "approve",
            &id,
            "--revision",
            "4",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Memory(MemoryArgs {
                command: MemoryCommand::Candidate {
                    command: CandidateCommand::Approve {
                        revision: 4,
                        confirm_sensitive: false,
                        ..
                    }
                }
            }))
        ));
        assert!(Cli::try_parse_from(["xana", "memory", "candidate", "install", &id]).is_err());
    }

    #[test]
    fn drafts_require_explicit_scope_and_keep_shell_text_as_data() {
        let input = [
            "xana",
            "memory",
            "candidate",
            "stage-skill",
            "--scope",
            "user",
            "--name",
            "fixture",
            "--markdown",
            "```sh\nnever-execute-this\n```",
        ];
        let args = Cli::try_parse_from(input).unwrap();
        assert!(matches!(args.command, Some(Command::Memory(MemoryArgs {
            command: MemoryCommand::Candidate { command: CandidateCommand::StageSkill {
                scope: MemoryScope::User, markdown, ..
            }}
        })) if markdown.contains("never-execute-this")));
    }
}
