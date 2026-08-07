//! Temporary per-call approval seam for local command execution.
//!
//! The neutral contract carries a fully resolved action. Frontends decide how
//! to ask a controlling user; tools and the headless agent never read stdin or
//! render prompts.

use std::{error::Error, fmt, io, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestedAction {
    pub(crate) tool_name: &'static str,
    pub(crate) shell: &'static str,
    pub(crate) command: String,
    pub(crate) argv: String,
    pub(crate) cwd: PathBuf,
}

pub(crate) trait ProvisionalApprover {
    fn confirm(&self, action: &RequestedAction) -> Result<bool, ApprovalError>;
}

#[derive(Debug)]
pub(crate) struct ApprovalError {
    source: io::Error,
}

impl ApprovalError {
    pub(crate) fn io(source: io::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not request command approval: {}", self.source)
    }
}

impl Error for ApprovalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
