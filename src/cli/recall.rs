use crate::identity::SessionId;
use clap::{Args, Subcommand};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub(crate) struct RecallArgs {
    /// Exact Conversation whose Project/Profile scopes the evidence.
    #[arg(long)]
    pub(crate) conversation: SessionId,
    #[command(subcommand)]
    pub(crate) command: RecallCommand,
}
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum RecallCommand {
    /// Search local indexed evidence without sending content to a provider.
    Search { query: String },
    /// Advance one bounded history-indexing batch; repeat while pending is true.
    Refresh,
    /// Discard only this scope's derived index/cursors; refresh to rebuild.
    Rebuild,
    /// Explicitly include one same-Project Conversation with a different Profile.
    Include {
        source: SessionId,
        #[arg(long)]
        remove: bool,
    },
    /// Manage ordinary editable notes separately from personal memory.
    Notes {
        #[command(subcommand)]
        command: NotesCommand,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub(crate) enum NotesCommand {
    /// Select/import an existing folder for local indexing only.
    #[command(alias = "import")]
    Select {
        path: PathBuf,
    },
    List,
    Revoke {
        root: Uuid,
    },
    Refresh {
        root: Uuid,
        /// Discard a pending scan cursor and take a new bounded file inventory.
        #[arg(long)]
        restart: bool,
    },
    /// Permit this notes root's retrieval to disclose to one exact native route.
    Disclose {
        root: Uuid,
        #[arg(long)]
        connection: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        remove: bool,
    },
    /// Explicitly create the optional ordinary data-directory notes location.
    Create,
    /// Export fresh indexed Markdown/text originals into a new directory.
    Export {
        root: Uuid,
        destination: PathBuf,
    },
}
