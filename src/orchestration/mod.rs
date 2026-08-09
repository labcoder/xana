//! Runtime-owned bounded child orchestration.
//!
//! Route resolution is pure over explicit configuration and local availability
//! inputs. Child supervision and collection build on the resolved snapshot in
//! later modules without moving provider or frontend policy into this domain.

mod routing;

pub(crate) use routing::{ExecutionOwner, ResolvedAgentConfig, RouteResolver};
