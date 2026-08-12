//! Versioned private records for optional interoperable product state.
//!
//! Shared configuration contains declarations and stable references. Project
//! identity, local bindings, package install state, and endpoint trust remain
//! separate runtime-owned records and are never copied into portable files.

mod schema;
mod store;

pub(crate) use store::{
    PrivateRecordInspection, PrivateRecordStatus, ensure_interoperable_records,
    inspect_interoperable_records,
};
