use super::protocol::{
    ControllerSnapshot, ControllerState, HostEvent, HostObservation, HostSnapshot,
    LOCAL_HOST_PROTOCOL_VERSION,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use uuid::Uuid;

pub(crate) const OBSERVER_QUEUE_CAPACITY: usize = 256;

#[derive(Clone)]
pub(crate) struct ObservationHub {
    state: Arc<Mutex<HubState>>,
    artifacts: Option<super::artifact_access::ArtifactAccess>,
}

struct HubState {
    snapshot: HostSnapshot,
    subscribers: HashMap<Uuid, mpsc::Sender<HostObservation>>,
    controller: Option<ControllerLease>,
}

struct ControllerLease {
    client_id: Uuid,
    conversation: String,
    reconnect_hash: [u8; 32],
    state: ControllerState,
    generation: u64,
}

pub(crate) struct Subscription {
    pub(crate) snapshot: HostSnapshot,
    pub(crate) observations: mpsc::Receiver<HostObservation>,
}

impl ObservationHub {
    #[cfg(test)]
    pub(crate) fn new(snapshot: HostSnapshot) -> Self {
        Self::with_artifacts(snapshot, None)
    }

    pub(crate) fn with_artifacts(
        snapshot: HostSnapshot,
        artifacts: Option<super::artifact_access::ArtifactAccess>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(HubState {
                snapshot,
                subscribers: HashMap::new(),
                controller: None,
            })),
            artifacts,
        }
    }

    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> Result<Subscription, String> {
        self.subscribe_as(Uuid::new_v4())
    }

    pub(crate) fn subscribe_as(&self, client_id: Uuid) -> Result<Subscription, String> {
        let (sender, observations) = mpsc::channel(OBSERVER_QUEUE_CAPACITY);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        let snapshot = state.snapshot.clone();
        state.subscribers.insert(client_id, sender);
        Ok(Subscription {
            snapshot,
            observations,
        })
    }

    pub(crate) fn unsubscribe(&self, client_id: Uuid) {
        if let Ok(mut state) = self.state.lock() {
            state.subscribers.remove(&client_id);
        }
    }

    pub(crate) fn publish(&self, event: HostEvent) -> Result<u64, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        Ok(publish_locked(&mut state, event))
    }

    pub(crate) fn publish_frontend(
        &self,
        event: crate::frontend::ClientEvent,
    ) -> Result<u64, String> {
        if let Some(artifacts) = &self.artifacts {
            artifacts.observe(&event);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        let sequence = state.snapshot.sequence.saturating_add(1);
        if let Some(frontend) = &mut state.snapshot.frontend {
            frontend.apply(&event, sequence);
        }
        Ok(publish_locked(&mut state, HostEvent::Frontend(event)))
    }

    pub(crate) fn fetch_artifact(
        &self,
        request_id: super::protocol::ArtifactRequestId,
        artifact_id: crate::identity::ArtifactId,
        max_preview_bytes: usize,
    ) -> super::protocol::ArtifactResult {
        self.artifacts.as_ref().map_or_else(
            || {
                super::protocol::ArtifactResult::rejected(
                    request_id,
                    "this host has no authorized artifact catalog",
                )
            },
            |artifacts| artifacts.fetch(request_id, artifact_id, max_preview_bytes),
        )
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.subscribers.len())
    }

    pub(crate) fn acquire_controller(
        &self,
        client_id: Uuid,
        conversation: &str,
        takeover: bool,
    ) -> Result<String, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        if state.snapshot.controllable_conversation.as_deref() != Some(conversation) {
            return Err("the requested conversation is not controlled by this host".into());
        }
        if let Some(controller) = &state.controller
            && !takeover
        {
            return Err(format!(
                "{} already has a controller; request explicit takeover or wait for release",
                controller.conversation
            ));
        }
        let reason = if state.controller.is_some() {
            "explicit takeover"
        } else {
            "explicit acquisition"
        };
        let reconnect = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let snapshot = ControllerSnapshot {
            conversation: conversation.to_owned(),
            state: ControllerState::Connected,
        };
        state.controller = Some(ControllerLease {
            client_id,
            conversation: conversation.to_owned(),
            reconnect_hash: *blake3::hash(reconnect.as_bytes()).as_bytes(),
            state: ControllerState::Connected,
            generation: state
                .controller
                .as_ref()
                .map_or(1, |controller| controller.generation.saturating_add(1)),
        });
        state.snapshot.controller = Some(snapshot.clone());
        publish_locked(
            &mut state,
            HostEvent::ControllerChanged {
                controller: Some(snapshot),
                reason: reason.to_owned(),
            },
        );
        Ok(reconnect)
    }

    pub(crate) fn reconnect_controller(
        &self,
        client_id: Uuid,
        reconnect: &str,
    ) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        let Some(controller) = &mut state.controller else {
            return Err("controller reconnect capability is invalid or expired".into());
        };
        if controller.state != ControllerState::Reconnecting
            || !constant_time_equal(
                blake3::hash(reconnect.as_bytes()).as_bytes(),
                &controller.reconnect_hash,
            )
        {
            return Err("controller reconnect capability is invalid or expired".into());
        }
        controller.client_id = client_id;
        controller.state = ControllerState::Connected;
        controller.generation = controller.generation.saturating_add(1);
        let snapshot = ControllerSnapshot {
            conversation: controller.conversation.clone(),
            state: ControllerState::Connected,
        };
        state.snapshot.controller = Some(snapshot.clone());
        publish_locked(
            &mut state,
            HostEvent::ControllerChanged {
                controller: Some(snapshot),
                reason: "controller reconnected".into(),
            },
        );
        Ok(())
    }

    pub(crate) fn is_controller(&self, client_id: Uuid) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.controller.as_ref().is_some_and(|controller| {
                controller.client_id == client_id && controller.state == ControllerState::Connected
            })
        })
    }

    pub(crate) fn disconnect_controller(&self, client_id: Uuid) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        let controller = state.controller.as_mut()?;
        if controller.client_id != client_id || controller.state != ControllerState::Connected {
            return None;
        }
        controller.state = ControllerState::Reconnecting;
        controller.generation = controller.generation.saturating_add(1);
        let generation = controller.generation;
        let snapshot = ControllerSnapshot {
            conversation: controller.conversation.clone(),
            state: ControllerState::Reconnecting,
        };
        state.snapshot.controller = Some(snapshot.clone());
        publish_locked(
            &mut state,
            HostEvent::ControllerChanged {
                controller: Some(snapshot),
                reason: "controller disconnected; bounded reconnect grace started".into(),
            },
        );
        Some(generation)
    }

    pub(crate) fn expire_controller(&self, generation: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let expired = state.controller.as_ref().is_some_and(|controller| {
            controller.state == ControllerState::Reconnecting && controller.generation == generation
        });
        if !expired {
            return false;
        }
        state.controller = None;
        state.snapshot.controller = None;
        publish_locked(
            &mut state,
            HostEvent::ControllerChanged {
                controller: None,
                reason: "controller reconnect grace expired".into(),
            },
        );
        true
    }

    pub(crate) fn release_controller(&self, client_id: Uuid) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state
            .controller
            .as_ref()
            .is_some_and(|controller| controller.client_id == client_id)
        {
            return false;
        }
        state.controller = None;
        state.snapshot.controller = None;
        publish_locked(
            &mut state,
            HostEvent::ControllerChanged {
                controller: None,
                reason: "controller released authority".into(),
            },
        );
        true
    }
}

fn publish_locked(state: &mut HubState, event: HostEvent) -> u64 {
    let sequence = state.snapshot.sequence.saturating_add(1);
    state.snapshot.sequence = sequence;
    let observation = HostObservation {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        sequence,
        event,
    };
    state
        .subscribers
        .retain(|_, subscriber| subscriber.try_send(observation.clone()).is_ok());
    sequence
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |different, (left, right)| different | (left ^ right))
        == 0
}
