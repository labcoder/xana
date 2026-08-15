//! In-process reference transport for the frontend application contract.

use super::protocol::{
    ClientCommand, ClientCommandResult, ClientEvent, ClientObservation, ClientSnapshot,
    ClientSnapshotSeed, FRONTEND_PROTOCOL_VERSION,
};
use crate::native_runtime::{
    AgentEvent, RuntimeCommand, RuntimeExit, RuntimeHandle, RuntimeUnavailable,
};
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const OBSERVATION_CAPACITY: usize = 256;
const CRITICAL_OBSERVATION_GRACE: Duration = Duration::from_secs(5);

/// The controlling half of an embedded client. Dropping the last command
/// sender closes the foreground runtime and triggers its existing cancellation
/// path. Observation ownership is deliberately separate.
pub(crate) struct EmbeddedOwner {
    runtime: RuntimeHandle,
}

/// A passive, bounded observation stream. Replaceable live deltas may be
/// omitted under pressure; authoritative observations receive a bounded grace
/// period. Neither path can cancel or delay runtime work.
pub(crate) struct EmbeddedObserver {
    snapshot: ClientSnapshot,
    observations: mpsc::Receiver<ClientObservation>,
    forwarder: Option<JoinHandle<ObservationStreamExit>>,
    exit: Option<ObservationStreamExit>,
}

pub(crate) struct EmbeddedClient {
    owner: EmbeddedOwner,
    observer: EmbeddedObserver,
}

impl EmbeddedClient {
    pub(crate) fn from_runtime(runtime: RuntimeHandle, seed: ClientSnapshotSeed) -> Self {
        let (runtime, events, history, runtime_exit) = runtime.into_frontend_parts();
        let snapshot = ClientSnapshot::initial(seed, history);
        let (observations, observation_receiver) = mpsc::channel(OBSERVATION_CAPACITY);
        let sequence = Arc::new(AtomicU64::new(snapshot.sequence));
        let forwarder = tokio::spawn(forward_observations(
            events,
            observations,
            sequence,
            runtime_exit,
        ));
        Self {
            owner: EmbeddedOwner { runtime },
            observer: EmbeddedObserver {
                snapshot,
                observations: observation_receiver,
                forwarder: Some(forwarder),
                exit: None,
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

    pub(crate) async fn next_event(&mut self) -> Result<AgentEvent, EmbeddedObservationError> {
        let observation = self.observer.next().await?;
        Ok(match observation.event {
            ClientEvent::Runtime(event) => *event,
            ClientEvent::Managed(_) => AgentEvent::CommandRejected {
                reason: "managed observation reached a native runtime client".to_owned(),
            },
            ClientEvent::PayloadOmitted {
                kind,
                encoded_bytes,
                limit,
            } => AgentEvent::CommandRejected {
                reason: format!(
                    "frontend omitted oversized {kind} observation ({encoded_bytes} bytes; limit {limit})"
                ),
            },
        })
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

    pub(crate) async fn next(&mut self) -> Result<ClientObservation, EmbeddedObservationError> {
        if let Some(observation) = self.observations.recv().await {
            return Ok(observation);
        }
        let exit = match self.exit {
            Some(exit) => exit,
            None => {
                let exit = match self.forwarder.take() {
                    Some(forwarder) => forwarder
                        .await
                        .unwrap_or(ObservationStreamExit::ForwarderPanicked),
                    None => ObservationStreamExit::ForwarderPanicked,
                };
                self.exit = Some(exit);
                if matches!(
                    exit,
                    ObservationStreamExit::Runtime(RuntimeExit::Panicked)
                        | ObservationStreamExit::RuntimeTaskLost
                        | ObservationStreamExit::ForwarderPanicked
                ) {
                    crate::diagnostics::record_task_panic("foreground-runtime");
                } else if matches!(
                    exit,
                    ObservationStreamExit::ObserverClosed | ObservationStreamExit::ObserverStalled
                ) {
                    crate::diagnostics::emit(crate::diagnostics::DiagnosticFact::new(
                        crate::config::DiagnosticLevel::Warn,
                        crate::config::DiagnosticTarget::Frontend,
                        crate::diagnostics::EventKind::FrontendDisconnected,
                        crate::diagnostics::EventOutcome::Unavailable,
                    ));
                }
                exit
            }
        };
        Err(EmbeddedObservationError(exit))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationStreamExit {
    Runtime(RuntimeExit),
    RuntimeTaskLost,
    ObserverClosed,
    ObserverStalled,
    ForwarderPanicked,
}

#[derive(Debug)]
pub(crate) struct EmbeddedObservationError(ObservationStreamExit);

impl fmt::Display for EmbeddedObservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ObservationStreamExit::Runtime(RuntimeExit::ShutdownRequested) => {
                write!(f, "Xana's foreground runtime shut down")
            }
            ObservationStreamExit::Runtime(RuntimeExit::ControllerDropped) => {
                write!(f, "Xana's foreground runtime controller was dropped")
            }
            ObservationStreamExit::Runtime(RuntimeExit::Panicked) => write!(
                f,
                "Xana's foreground runtime panicked; durable operation state remains recoverable"
            ),
            ObservationStreamExit::RuntimeTaskLost => {
                write!(
                    f,
                    "Xana's foreground runtime task ended without an exit status"
                )
            }
            ObservationStreamExit::ObserverClosed => {
                write!(f, "Xana's embedded observation stream was closed")
            }
            ObservationStreamExit::ObserverStalled => write!(
                f,
                "Xana's frontend did not receive a critical runtime observation within {} seconds",
                CRITICAL_OBSERVATION_GRACE.as_secs()
            ),
            ObservationStreamExit::ForwarderPanicked => {
                write!(f, "Xana's embedded observation forwarder panicked")
            }
        }
    }
}

impl Error for EmbeddedObservationError {}

async fn forward_observations(
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
    observations: mpsc::Sender<ClientObservation>,
    sequence: Arc<AtomicU64>,
    mut runtime_exit: watch::Receiver<Option<RuntimeExit>>,
) -> ObservationStreamExit {
    while let Some(event) = events.recv().await {
        let replaceable = event.is_replaceable_observation();
        let next = sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let observation = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: next,
            event: ClientEvent::bounded(event),
        };
        match observations.try_send(observation) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) if replaceable => {}
            Err(mpsc::error::TrySendError::Full(observation)) => {
                match tokio::time::timeout(
                    CRITICAL_OBSERVATION_GRACE,
                    observations.send(observation),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => return ObservationStreamExit::ObserverClosed,
                    Err(_) => return ObservationStreamExit::ObserverStalled,
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return ObservationStreamExit::ObserverClosed;
            }
        }
    }

    loop {
        if let Some(exit) = *runtime_exit.borrow() {
            return ObservationStreamExit::Runtime(exit);
        }
        if runtime_exit.changed().await.is_err() {
            return ObservationStreamExit::RuntimeTaskLost;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{OperationId, StepId},
        message::{Message, Role},
        native_runtime::{OperationOutcome, OperationState},
    };

    #[tokio::test]
    async fn critical_observations_survive_a_replaceable_delta_burst() {
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (observations, mut observation_receiver) = mpsc::channel(OBSERVATION_CAPACITY);
        let (runtime_exit, runtime_exit_receiver) = watch::channel(None);
        let operation_id = OperationId::new();
        let step_id = StepId::new();
        let sequence = Arc::new(AtomicU64::new(0));
        let forwarder = tokio::spawn(forward_observations(
            event_receiver,
            observations,
            Arc::clone(&sequence),
            runtime_exit_receiver,
        ));

        for _ in 0..(OBSERVATION_CAPACITY + 64) {
            events
                .send(AgentEvent::AssistantTextDelta {
                    operation_id,
                    step_id,
                    text: "x".to_owned(),
                })
                .unwrap();
        }
        events
            .send(AgentEvent::AssistantMessage {
                operation_id,
                message: Message::text(Role::Assistant, "complete response"),
            })
            .unwrap();
        events
            .send(AgentEvent::OperationStateChanged {
                operation_id,
                state: OperationState::Finished(OperationOutcome::Completed),
            })
            .unwrap();
        drop(events);
        runtime_exit
            .send(Some(RuntimeExit::ShutdownRequested))
            .unwrap();

        // Do not begin draining until the producer has deterministically
        // encountered the bounded queue, even under a busy parallel test run.
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while sequence.load(Ordering::Relaxed) <= OBSERVATION_CAPACITY as u64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("forwarder reaches observation capacity");

        let mut saw_final = false;
        let mut saw_completed = false;
        let mut delivered = 0_usize;
        while let Some(observation) = observation_receiver.recv().await {
            delivered += 1;
            saw_final |= matches!(
                observation.event,
                ClientEvent::Runtime(ref event)
                    if matches!(event.as_ref(), AgentEvent::AssistantMessage { .. })
            );
            saw_completed |= matches!(
                observation.event,
                ClientEvent::Runtime(ref event)
                    if matches!(
                        event.as_ref(),
                        AgentEvent::OperationStateChanged {
                            state: OperationState::Finished(OperationOutcome::Completed),
                            ..
                        }
                    )
            );
        }
        forwarder.await.unwrap();

        assert!(
            saw_final,
            "delta overflow detached before the final message"
        );
        assert!(
            saw_completed,
            "delta overflow detached before the terminal operation state"
        );
        assert!(
            delivered < OBSERVATION_CAPACITY + 66,
            "replaceable deltas were not shed under queue pressure"
        );
    }

    #[tokio::test]
    async fn runtime_panic_remains_a_distinct_stream_exit() {
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (observations, _observation_receiver) = mpsc::channel(OBSERVATION_CAPACITY);
        let (runtime_exit, runtime_exit_receiver) = watch::channel(None);
        drop(events);
        runtime_exit.send(Some(RuntimeExit::Panicked)).unwrap();

        let exit = forward_observations(
            event_receiver,
            observations,
            Arc::new(AtomicU64::new(0)),
            runtime_exit_receiver,
        )
        .await;

        assert_eq!(exit, ObservationStreamExit::Runtime(RuntimeExit::Panicked));
    }
}
