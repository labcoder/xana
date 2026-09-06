//! Optional owned browser tasks: typed authority, pinned recipient egress and
//! bounded evidence. No provider loop, ambient profile, or public CDP escape.

mod cdp;
mod page;
mod process;
mod proxy;
mod session;
mod tool;

pub(crate) use session::{BrowserOwner, BrowserPlan, BrowserReceipt};
pub(crate) use tool::register_tools;

use serde::{Deserialize, Serialize};

/// Direct client controls can inspect or revoke automation, never launch a
/// browser or approve a page action. Resumption uses the reviewed tool path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserControl {
    Status,
    Takeover,
    Close,
}
impl BrowserControl {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "" | "status" => Ok(Self::Status), "takeover" => Ok(Self::Takeover), "close" => Ok(Self::Close),
            _ => Err("usage: /browser [status|takeover|close]; resuming automation requires a reviewed browser tool request".into()),
        }
    }
}

pub(crate) const MAX_ACTIONS: u32 = 30;
pub(crate) const MAX_TASK_SECONDS: u64 = 300;
pub(crate) const MAX_OBSERVATION_BYTES: usize = 128 * 1024;
pub(crate) const EGRESS_DISCLOSURE: &str = "Dedicated fresh browser; exact reviewed recipients through a DNS-pinned mandatory proxy. Reviewed page scripts may contact those recipients, including WebSockets; this is not an effect sandbox or OS firewall. Other recipients, local files, non-proxied UDP, uploads and downloads are unavailable. Temporary browser cache/login material is not Xana-record encryption.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BrowserRequest {
    Launch {
        origins: Vec<String>,
    },
    Navigate {
        url: String,
    },
    Observe {},
    Screenshot {},
    Act {
        reference: String,
        effect: BrowserEffect,
        purpose: String,
    },
    Takeover {},
    Resume {},
    Close {},
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BrowserEffect {
    Click {},
    Fill { text: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrowserError {
    Unavailable,
    UnsupportedBrowser,
    UnsupportedEgress,
    InvalidInput,
    LockedStorage,
    NoSession,
    Busy,
    TakenOver,
    Stale,
    Limit,
    Cancelled,
    TimedOut,
    Process,
    Protocol,
    Uncertain,
    Storage,
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "browser {:?}; no action is automatically replayed", self)
    }
}
impl std::error::Error for BrowserError {}

#[cfg(test)]
mod tests;
