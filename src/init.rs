//! First-run initialization boundary.
//!
//! Planning turns CLI values or scripted answers into a validated draft without
//! touching the filesystem. Writing re-validates the rendered document and
//! creates exactly one new configuration without overwriting existing data.

mod plan;
mod write;

pub(crate) use plan::{InitPlan, plan};
pub(crate) use write::{WriteOutcome, write_new_config};
