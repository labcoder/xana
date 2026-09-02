//! Conversation-scoped controller leases shared by embedded and loopback hosts.
//!
//! Authentication proves that a client may speak to a host. This module makes
//! the separate, narrower decision about which client may mutate one
//! Conversation. It owns no I/O and accepts an explicit monotonic time so lease
//! expiry remains deterministic and independent of wall-clock changes.

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    time::{Duration, Instant},
};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ControllerClientId(Uuid);

impl ControllerClientId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ControllerId(Uuid);

impl ControllerId {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ControllerId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(output)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControllerLeaseState {
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControllerTakeoverState {
    NotTakenOver,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControllerTakeoverConfirmation {
    pub(crate) controller_id: ControllerId,
    pub(crate) generation: u64,
}

impl<K> From<&ControllerLeaseSnapshot<K>> for ControllerTakeoverConfirmation {
    fn from(snapshot: &ControllerLeaseSnapshot<K>) -> Self {
        Self {
            controller_id: snapshot.controller_id,
            generation: snapshot.generation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControllerDisconnectReason {
    TransportClosed,
    SequenceGap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControllerLeaseSnapshot<K> {
    pub(crate) conversation: K,
    pub(crate) controller_id: ControllerId,
    pub(crate) generation: u64,
    pub(crate) state: ControllerLeaseState,
    pub(crate) takeover: ControllerTakeoverState,
    pub(crate) reconnect_grace_remaining_ms: Option<u64>,
    pub(crate) disconnect_reason: Option<ControllerDisconnectReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ControllerChangeKind {
    Acquired,
    Renewed,
    Reconnected,
    TakenOver { previous_controller: ControllerId },
    Disconnected { reason: ControllerDisconnectReason },
    Released,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControllerChange<K> {
    pub(crate) conversation: K,
    pub(crate) controller: Option<ControllerLeaseSnapshot<K>>,
    pub(crate) change: ControllerChangeKind,
}

pub(crate) struct ControllerGrant<K> {
    pub(crate) snapshot: ControllerLeaseSnapshot<K>,
    pub(crate) change: ControllerChangeKind,
    reconnect_capability: String,
}

impl<K> ControllerGrant<K> {
    pub(crate) fn take_reconnect_capability(&mut self) -> String {
        std::mem::take(&mut self.reconnect_capability)
    }
}

impl<K: fmt::Debug> fmt::Debug for ControllerGrant<K> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ControllerGrant")
            .field("snapshot", &self.snapshot)
            .field("change", &self.change)
            .field("reconnect_capability", &"[REDACTED]")
            .finish()
    }
}

impl<K> Drop for ControllerGrant<K> {
    fn drop(&mut self) {
        self.reconnect_capability.zeroize();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ControllerExpiry<K> {
    pub(crate) conversation: K,
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerLeaseError<K> {
    TakeoverConfirmationRequired { current: ControllerLeaseSnapshot<K> },
    PendingApproval(K),
    NotController(K),
    InvalidReconnect,
    ExpiredReconnect,
}

impl<K: fmt::Display> fmt::Display for ControllerLeaseError<K> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TakeoverConfirmationRequired { current } => write!(
                output,
                "Conversation {} is controlled by {} at lease generation {}; confirm explicit takeover of that exact lease or remain an observer",
                current.conversation, current.controller_id, current.generation
            ),
            Self::PendingApproval(conversation) => write!(
                output,
                "Conversation {conversation} has a pending approval; resolve or cancel it before controller takeover"
            ),
            Self::NotController(conversation) => write!(
                output,
                "this client is not the current controller for Conversation {conversation}"
            ),
            Self::InvalidReconnect => {
                output.write_str("controller reconnect capability is invalid")
            }
            Self::ExpiredReconnect => {
                output.write_str("controller reconnect capability has expired")
            }
        }
    }
}

impl<K: fmt::Debug + fmt::Display> Error for ControllerLeaseError<K> {}

pub(crate) struct ControllerLeases<K> {
    leases: BTreeMap<K, Lease>,
    generations: BTreeMap<K, u64>,
}

struct Lease {
    client_id: ControllerClientId,
    controller_id: ControllerId,
    generation: u64,
    reconnect_hash: [u8; 32],
    state: ControllerLeaseState,
    takeover: ControllerTakeoverState,
    reconnect_deadline: Option<Instant>,
    disconnect_reason: Option<ControllerDisconnectReason>,
}

impl<K> ControllerLeases<K>
where
    K: Clone + Ord,
{
    pub(crate) fn new() -> Self {
        Self {
            leases: BTreeMap::new(),
            generations: BTreeMap::new(),
        }
    }

    pub(crate) fn acquire(
        &mut self,
        conversation: K,
        client_id: ControllerClientId,
        takeover: Option<ControllerTakeoverConfirmation>,
        has_pending_approval: bool,
        now: Instant,
    ) -> Result<ControllerGrant<K>, ControllerLeaseError<K>> {
        if let Some(existing) = self.leases.get(&conversation) {
            if existing.client_id == client_id && existing.state == ControllerLeaseState::Connected
            {
                return self.renew(&conversation, client_id, now);
            }
            let expected = ControllerTakeoverConfirmation::from(&snapshot(
                conversation.clone(),
                existing,
                now,
            ));
            if takeover != Some(expected) {
                return Err(ControllerLeaseError::TakeoverConfirmationRequired {
                    current: snapshot(conversation, existing, now),
                });
            }
            if has_pending_approval {
                return Err(ControllerLeaseError::PendingApproval(conversation));
            }
        }

        let previous = self
            .leases
            .get(&conversation)
            .map(|lease| lease.controller_id);
        let generation = self.next_generation(&conversation);
        let controller_id = ControllerId::new();
        let (reconnect_capability, reconnect_hash) = reconnect_capability();
        let takeover = if previous.is_some() {
            ControllerTakeoverState::Confirmed
        } else {
            ControllerTakeoverState::NotTakenOver
        };
        self.leases.insert(
            conversation.clone(),
            Lease {
                client_id,
                controller_id,
                generation,
                reconnect_hash,
                state: ControllerLeaseState::Connected,
                takeover,
                reconnect_deadline: None,
                disconnect_reason: None,
            },
        );
        let change = previous.map_or(ControllerChangeKind::Acquired, |previous_controller| {
            ControllerChangeKind::TakenOver {
                previous_controller,
            }
        });
        Ok(ControllerGrant {
            snapshot: self
                .snapshot(&conversation, now)
                .expect("newly inserted controller lease must exist"),
            change,
            reconnect_capability,
        })
    }

    pub(crate) fn renew(
        &mut self,
        conversation: &K,
        client_id: ControllerClientId,
        now: Instant,
    ) -> Result<ControllerGrant<K>, ControllerLeaseError<K>> {
        let authorized = self.leases.get(conversation).is_some_and(|lease| {
            lease.client_id == client_id && lease.state == ControllerLeaseState::Connected
        });
        if !authorized {
            return Err(ControllerLeaseError::NotController(conversation.clone()));
        }
        let next_generation = self.next_generation(conversation);
        let lease = self
            .leases
            .get_mut(conversation)
            .expect("authorized controller lease must exist");
        let (reconnect_capability, reconnect_hash) = reconnect_capability();
        lease.generation = next_generation;
        lease.reconnect_hash = reconnect_hash;
        lease.reconnect_deadline = None;
        lease.disconnect_reason = None;
        Ok(ControllerGrant {
            snapshot: snapshot(conversation.clone(), lease, now),
            change: ControllerChangeKind::Renewed,
            reconnect_capability,
        })
    }

    pub(crate) fn disconnect_client(
        &mut self,
        client_id: ControllerClientId,
        reason: ControllerDisconnectReason,
        now: Instant,
        grace: Duration,
    ) -> Vec<(ControllerChange<K>, ControllerExpiry<K>)> {
        let conversations = self
            .leases
            .iter()
            .filter_map(|(conversation, lease)| {
                (lease.client_id == client_id && lease.state == ControllerLeaseState::Connected)
                    .then_some(conversation.clone())
            })
            .collect::<Vec<_>>();
        let mut changes = Vec::with_capacity(conversations.len());
        for conversation in conversations {
            let generation = self.next_generation(&conversation);
            let deadline = now.checked_add(grace).unwrap_or(now);
            let lease = self
                .leases
                .get_mut(&conversation)
                .expect("selected controller lease must exist");
            lease.generation = generation;
            lease.state = ControllerLeaseState::Reconnecting;
            lease.reconnect_deadline = Some(deadline);
            lease.disconnect_reason = Some(reason);
            let controller = snapshot(conversation.clone(), lease, now);
            changes.push((
                ControllerChange {
                    conversation: conversation.clone(),
                    controller: Some(controller),
                    change: ControllerChangeKind::Disconnected { reason },
                },
                ControllerExpiry {
                    conversation,
                    generation,
                    deadline,
                },
            ));
        }
        changes
    }

    pub(crate) fn reconnect(
        &mut self,
        client_id: ControllerClientId,
        reconnect: &str,
        now: Instant,
    ) -> Result<ControllerGrant<K>, ControllerLeaseError<K>> {
        let reconnect_hash = *blake3::hash(reconnect.as_bytes()).as_bytes();
        let matched = self
            .leases
            .iter()
            .find_map(|(conversation, lease)| {
                constant_time_equal(&reconnect_hash, &lease.reconnect_hash)
                    .then_some(conversation.clone())
            })
            .ok_or(ControllerLeaseError::InvalidReconnect)?;
        let expired = self.leases.get(&matched).is_some_and(|lease| {
            lease.state == ControllerLeaseState::Reconnecting
                && lease
                    .reconnect_deadline
                    .is_none_or(|deadline| now >= deadline)
        });
        if expired {
            return Err(ControllerLeaseError::ExpiredReconnect);
        }
        let generation = self.next_generation(&matched);
        let (reconnect_capability, next_hash) = reconnect_capability();
        let lease = self
            .leases
            .get_mut(&matched)
            .expect("matched controller lease must exist");
        lease.client_id = client_id;
        lease.generation = generation;
        lease.reconnect_hash = next_hash;
        lease.state = ControllerLeaseState::Connected;
        lease.reconnect_deadline = None;
        lease.disconnect_reason = None;
        Ok(ControllerGrant {
            snapshot: snapshot(matched, lease, now),
            change: ControllerChangeKind::Reconnected,
            reconnect_capability,
        })
    }

    pub(crate) fn release(
        &mut self,
        conversation: &K,
        client_id: ControllerClientId,
    ) -> Result<ControllerChange<K>, ControllerLeaseError<K>> {
        let authorized = self
            .leases
            .get(conversation)
            .is_some_and(|lease| lease.client_id == client_id);
        if !authorized {
            return Err(ControllerLeaseError::NotController(conversation.clone()));
        }
        self.leases.remove(conversation);
        Ok(ControllerChange {
            conversation: conversation.clone(),
            controller: None,
            change: ControllerChangeKind::Released,
        })
    }

    pub(crate) fn expire(
        &mut self,
        expiry: &ControllerExpiry<K>,
        now: Instant,
    ) -> Option<ControllerChange<K>> {
        let expires = self.leases.get(&expiry.conversation).is_some_and(|lease| {
            lease.state == ControllerLeaseState::Reconnecting
                && lease.generation == expiry.generation
                && lease
                    .reconnect_deadline
                    .is_some_and(|deadline| now >= deadline)
        });
        if !expires {
            return None;
        }
        self.leases.remove(&expiry.conversation);
        Some(ControllerChange {
            conversation: expiry.conversation.clone(),
            controller: None,
            change: ControllerChangeKind::Expired,
        })
    }

    pub(crate) fn is_controller(&self, conversation: &K, client_id: ControllerClientId) -> bool {
        self.leases.get(conversation).is_some_and(|lease| {
            lease.client_id == client_id && lease.state == ControllerLeaseState::Connected
        })
    }

    pub(crate) fn snapshot(
        &self,
        conversation: &K,
        now: Instant,
    ) -> Option<ControllerLeaseSnapshot<K>> {
        self.leases
            .get(conversation)
            .map(|lease| snapshot(conversation.clone(), lease, now))
    }

    fn next_generation(&mut self, conversation: &K) -> u64 {
        let generation = self.generations.entry(conversation.clone()).or_default();
        *generation = generation.saturating_add(1);
        *generation
    }
}

fn snapshot<K: Clone>(conversation: K, lease: &Lease, now: Instant) -> ControllerLeaseSnapshot<K> {
    let reconnect_grace_remaining_ms = lease.reconnect_deadline.map(|deadline| {
        deadline
            .saturating_duration_since(now)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    });
    ControllerLeaseSnapshot {
        conversation,
        controller_id: lease.controller_id,
        generation: lease.generation,
        state: lease.state,
        takeover: lease.takeover,
        reconnect_grace_remaining_ms,
        disconnect_reason: lease.disconnect_reason,
    }
}

fn reconnect_capability() -> (String, [u8; 32]) {
    let capability = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let digest = *blake3::hash(capability.as_bytes()).as_bytes();
    (capability, digest)
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |different, (left, right)| different | (left ^ right))
        == 0
}
