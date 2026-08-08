//! Foreign managed-agent runtimes.
//!
//! A managed runtime owns its inner agent loop, provider protocol, account
//! credentials, tools, sandbox, and approval callbacks. Xana owns process
//! supervision, UI projection, selection, and explicit handoff boundaries.

pub(crate) mod codex;
