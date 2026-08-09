//! Authenticated loopback projection of Xana's repository-private frontend contract.

mod descriptor;
mod hub;
mod protocol;
mod transport;

pub(crate) use protocol::HostSnapshotSeed;
pub(crate) use transport::{LocalHostError, LocalHostServer, connect_observer};

#[cfg(test)]
mod tests;
