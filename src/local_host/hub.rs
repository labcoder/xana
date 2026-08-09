use super::protocol::{HostEvent, HostObservation, HostSnapshot, LOCAL_HOST_PROTOCOL_VERSION};
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
}

struct HubState {
    snapshot: HostSnapshot,
    subscribers: HashMap<Uuid, mpsc::Sender<HostObservation>>,
}

pub(crate) struct Subscription {
    pub(crate) client_id: Uuid,
    pub(crate) snapshot: HostSnapshot,
    pub(crate) observations: mpsc::Receiver<HostObservation>,
}

impl ObservationHub {
    pub(crate) fn new(snapshot: HostSnapshot) -> Self {
        Self {
            state: Arc::new(Mutex::new(HubState {
                snapshot,
                subscribers: HashMap::new(),
            })),
        }
    }

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
            client_id,
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
        Ok(sequence)
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.subscribers.len())
    }
}
