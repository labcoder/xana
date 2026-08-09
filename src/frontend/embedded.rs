//! In-process reference transport for the frontend application contract.

use super::protocol::{
    ClientCommand, ClientCommandResult, ClientEvent, ClientObservation, ClientSnapshot,
    ClientSnapshotSeed, FRONTEND_PROTOCOL_VERSION,
};
use crate::runtime::{AgentEvent, RuntimeCommand, RuntimeHandle, RuntimeUnavailable};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;

const OBSERVATION_CAPACITY: usize = 256;

/// The controlling half of an embedded client. Dropping the last command
/// sender closes the foreground runtime and triggers its existing cancellation
/// path. Observation ownership is deliberately separate.
pub(crate) struct EmbeddedOwner {
    runtime: RuntimeHandle,
}

/// A passive, bounded observation stream. Dropping or overflowing this stream
/// cannot cancel work or delay the runtime.
pub(crate) struct EmbeddedObserver {
    snapshot: ClientSnapshot,
    observations: mpsc::Receiver<ClientObservation>,
}

pub(crate) struct EmbeddedClient {
    owner: EmbeddedOwner,
    observer: EmbeddedObserver,
}

impl EmbeddedClient {
    pub(crate) fn from_runtime(runtime: RuntimeHandle, seed: ClientSnapshotSeed) -> Self {
        let (runtime, events, history) = runtime.into_frontend_parts();
        let snapshot = ClientSnapshot::initial(seed, history);
        let (observations, observation_receiver) = mpsc::channel(OBSERVATION_CAPACITY);
        let sequence = Arc::new(AtomicU64::new(snapshot.sequence));
        tokio::spawn(forward_observations(events, observations, sequence));
        Self {
            owner: EmbeddedOwner { runtime },
            observer: EmbeddedObserver {
                snapshot,
                observations: observation_receiver,
            },
        }
    }

    pub(crate) fn snapshot(&self) -> &ClientSnapshot {
        self.observer.snapshot()
    }

    pub(crate) fn into_parts(self) -> (EmbeddedOwner, EmbeddedObserver) {
        (self.owner, self.observer)
    }

    pub(crate) async fn send(
        &self,
        command: RuntimeCommand,
    ) -> Result<ClientCommandResult, RuntimeUnavailable> {
        self.owner.send(ClientCommand::new(command)).await
    }

    pub(crate) async fn next_event(&mut self) -> Option<AgentEvent> {
        let observation = self.observer.next().await?;
        match observation.event {
            ClientEvent::Runtime(event) => Some(*event),
            ClientEvent::Managed(_) => Some(AgentEvent::CommandRejected {
                reason: "managed observation reached a native runtime client".to_owned(),
            }),
            ClientEvent::PayloadOmitted {
                kind,
                encoded_bytes,
                limit,
            } => Some(AgentEvent::CommandRejected {
                reason: format!(
                    "frontend omitted oversized {kind} observation ({encoded_bytes} bytes; limit {limit})"
                ),
            }),
        }
    }
}

impl EmbeddedOwner {
    pub(crate) async fn send(
        &self,
        command: ClientCommand,
    ) -> Result<ClientCommandResult, RuntimeUnavailable> {
        if command.version != FRONTEND_PROTOCOL_VERSION {
            return Ok(ClientCommandResult::rejected(
                command.id,
                format!(
                    "unsupported frontend protocol version {}; expected {}",
                    command.version, FRONTEND_PROTOCOL_VERSION
                ),
            ));
        }
        let id = command.id;
        if let Err(reason) = command.value.validate() {
            return Ok(ClientCommandResult::rejected(id, reason));
        }
        self.runtime.send(command.value.into()).await?;
        Ok(ClientCommandResult::accepted(id))
    }
}

impl EmbeddedObserver {
    pub(crate) fn snapshot(&self) -> &ClientSnapshot {
        &self.snapshot
    }

    pub(crate) async fn next(&mut self) -> Option<ClientObservation> {
        self.observations.recv().await
    }
}

async fn forward_observations(
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
    observations: mpsc::Sender<ClientObservation>,
    sequence: Arc<AtomicU64>,
) {
    while let Some(event) = events.recv().await {
        let next = sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let observation = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: next,
            event: ClientEvent::bounded(event),
        };
        match observations.try_send(observation) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_) | mpsc::error::TrySendError::Closed(_)) => {
                return;
            }
        }
    }
}
