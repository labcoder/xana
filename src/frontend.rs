//! Repository-private application contract for Xana frontends.
//!
//! Frontends send correlated commands and render bounded observations. This
//! facade contains no terminal, network, provider-wire, or presentation
//! concerns. The embedded adapter is the reference transport; later transport
//! projections must preserve these semantics rather than invent another API.

mod embedded;
mod managed;
mod protocol;

pub(crate) use embedded::{EmbeddedClient, EmbeddedObserver, EmbeddedOwner};
pub(crate) use managed::{ManagedClientEvent, ManagedClientItem};
pub(crate) use protocol::{
    ClientCommand, ClientCommandResult, ClientCommandValue, ClientEvent, ClientObservation,
    ClientSnapshot, ClientSnapshotSeed, FRONTEND_PROTOCOL_VERSION,
};
