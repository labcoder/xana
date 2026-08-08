//! Provider adapter boundary.
//!
//! Provider wire formats remain private below this module. The rest of Xana
//! exchanges only internal messages, tool definitions, and structured errors.

pub(crate) mod anthropic;
pub(crate) mod contract;
pub(crate) mod openai_compat;
