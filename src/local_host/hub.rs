use super::protocol::{HostEvent, HostObservation, HostSnapshot, LOCAL_HOST_PROTOCOL_VERSION};
use crate::controller::{
    ControllerChange, ControllerClientId, ControllerDisconnectReason, ControllerExpiry,
    ControllerGrant, ControllerLeaseError, ControllerLeases, ControllerTakeoverConfirmation,
};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
    subscribers: HashMap<ControllerClientId, mpsc::Sender<HostObservation>>,
    controllers: ControllerLeases<String>,
    pending_managed_approvals: BTreeSet<Uuid>,
}

pub(crate) struct Subscription {
    pub(crate) snapshot: HostSnapshot,
    pub(crate) observations: mpsc::Receiver<HostObservation>,
}

impl ObservationHub {
    pub(crate) fn stop_clients(&self) -> Result<crate::autonomy::host::StopClients, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        if state.subscribers.len() > 32 {
            return Err("stop client snapshot exceeds the host client bound".into());
        }
        let mut identities = state.subscribers.keys().copied().collect::<Vec<_>>();
        identities.sort_unstable();
        Ok(crate::autonomy::host::StopClients {
            host_id: state.snapshot.host_id,
            host_generation: state.snapshot.host_generation,
            count: identities.len(),
            identities,
        })
    }

    pub(crate) fn replace_scheduled(
        &self,
        jobs: Vec<crate::autonomy::host::JobSummary>,
    ) -> Result<(), String> {
        if jobs.len() > crate::autonomy::PAGE_SIZE {
            return Err("schedule projection exceeds page bound".into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        if state.snapshot.scheduled_jobs != jobs {
            state.snapshot.scheduled_jobs = jobs.clone();
            publish_locked(&mut state, HostEvent::ScheduledJobsChanged { jobs });
        }
        Ok(())
    }
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
                controllers: ControllerLeases::new(),
                pending_managed_approvals: BTreeSet::new(),
            })),
            artifacts,
        }
    }

    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> Result<Subscription, String> {
        self.subscribe_as(ControllerClientId::new())
    }

    pub(crate) fn subscribe_as(
        &self,
        client_id: ControllerClientId,
    ) -> Result<Subscription, String> {
        let (sender, observations) = mpsc::channel(OBSERVER_QUEUE_CAPACITY);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        let mut snapshot = state.snapshot.clone();
        snapshot.controller = snapshot
            .controllable_conversation
            .as_ref()
            .and_then(|conversation| state.controllers.snapshot(conversation, Instant::now()));
        state.subscribers.insert(client_id, sender);
        Ok(Subscription {
            snapshot,
            observations,
        })
    }

    pub(crate) fn unsubscribe(&self, client_id: ControllerClientId) {
        if let Ok(mut state) = self.state.lock() {
            state.subscribers.remove(&client_id);
        }
    }

    pub(crate) fn publish(&self, event: HostEvent) -> Result<u64, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local-host observation lock was poisoned".to_owned())?;
        update_pending_managed_approvals(&mut state, &event);
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
        offset: u64,
        max_bytes: usize,
    ) -> super::protocol::ArtifactResult {
        self.artifacts.as_ref().map_or_else(
            || {
                super::protocol::ArtifactResult::rejected(
                    request_id,
                    "this host has no authorized artifact catalog",
                )
            },
            |artifacts| artifacts.fetch(request_id, artifact_id, offset, max_bytes),
        )
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.state.lock().map_or(0, |state| state.subscribers.len())
    }

    pub(crate) fn acquire_controller(
        &self,
        client_id: ControllerClientId,
        conversation: &str,
        takeover: Option<ControllerTakeoverConfirmation>,
    ) -> Result<ControllerGrant<String>, ControllerLeaseError<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ControllerLeaseError::NotController(conversation.to_owned()))?;
        if state.snapshot.controllable_conversation.as_deref() != Some(conversation) {
            return Err(ControllerLeaseError::NotController(conversation.to_owned()));
        }
        let pending_native = state
            .snapshot
            .frontend
            .as_ref()
            .is_some_and(|frontend| frontend.pending_approval_count > 0);
        let pending = pending_native || !state.pending_managed_approvals.is_empty();
        let grant = state.controllers.acquire(
            conversation.to_owned(),
            client_id,
            takeover,
            pending,
            Instant::now(),
        )?;
        apply_controller_change(
            &mut state,
            ControllerChange {
                conversation: conversation.to_owned(),
                controller: Some(grant.snapshot.clone()),
                change: grant.change,
            },
        );
        Ok(grant)
    }

    pub(crate) fn renew_controller(
        &self,
        client_id: ControllerClientId,
    ) -> Result<ControllerGrant<String>, ControllerLeaseError<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ControllerLeaseError::InvalidReconnect)?;
        let conversation = state
            .snapshot
            .controllable_conversation
            .clone()
            .ok_or(ControllerLeaseError::InvalidReconnect)?;
        let grant = state
            .controllers
            .renew(&conversation, client_id, Instant::now())?;
        apply_controller_change(
            &mut state,
            ControllerChange {
                conversation,
                controller: Some(grant.snapshot.clone()),
                change: grant.change,
            },
        );
        Ok(grant)
    }

    pub(crate) fn reconnect_controller(
        &self,
        client_id: ControllerClientId,
        reconnect: &str,
    ) -> Result<ControllerGrant<String>, ControllerLeaseError<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ControllerLeaseError::InvalidReconnect)?;
        let grant = state
            .controllers
            .reconnect(client_id, reconnect, Instant::now())?;
        let conversation = grant.snapshot.conversation.clone();
        apply_controller_change(
            &mut state,
            ControllerChange {
                conversation,
                controller: Some(grant.snapshot.clone()),
                change: grant.change,
            },
        );
        Ok(grant)
    }

    pub(crate) fn is_controller(&self, client_id: ControllerClientId) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .snapshot
                .controllable_conversation
                .as_ref()
                .is_some_and(|conversation| {
                    state.controllers.is_controller(conversation, client_id)
                })
        })
    }

    pub(crate) fn disconnect_controller(
        &self,
        client_id: ControllerClientId,
        reason: ControllerDisconnectReason,
        grace: Duration,
    ) -> Option<ControllerExpiry<String>> {
        let mut state = self.state.lock().ok()?;
        let (change, expiry) = state
            .controllers
            .disconnect_client(client_id, reason, Instant::now(), grace)
            .pop()?;
        apply_controller_change(&mut state, change);
        Some(expiry)
    }

    pub(crate) fn expire_controller(&self, expiry: &ControllerExpiry<String>) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(change) = state.controllers.expire(expiry, Instant::now()) else {
            return false;
        };
        apply_controller_change(&mut state, change);
        true
    }

    pub(crate) fn release_controller(
        &self,
        client_id: ControllerClientId,
    ) -> Result<(), ControllerLeaseError<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ControllerLeaseError::InvalidReconnect)?;
        let conversation = state
            .snapshot
            .controllable_conversation
            .clone()
            .ok_or(ControllerLeaseError::InvalidReconnect)?;
        let change = state.controllers.release(&conversation, client_id)?;
        apply_controller_change(&mut state, change);
        Ok(())
    }
}

fn apply_controller_change(state: &mut HubState, change: ControllerChange<String>) {
    crate::diagnostics::emit(
        crate::diagnostics::DiagnosticFact::new(
            crate::config::DiagnosticLevel::Info,
            crate::config::DiagnosticTarget::Frontend,
            crate::diagnostics::EventKind::ControllerAuthorityChanged,
            controller_diagnostic_outcome(change.change),
        )
        .correlation(&change.conversation)
        .subject(controller_change_reason(change.change)),
    );
    state.snapshot.controller = change.controller.clone();
    let reason = controller_change_reason(change.change);
    publish_locked(
        state,
        HostEvent::ControllerChanged {
            controller: change.controller,
            change: change.change,
            reason: reason.to_owned(),
        },
    );
}

fn controller_diagnostic_outcome(
    change: crate::controller::ControllerChangeKind,
) -> crate::diagnostics::EventOutcome {
    use crate::{controller::ControllerChangeKind, diagnostics::EventOutcome};
    match change {
        ControllerChangeKind::Acquired
        | ControllerChangeKind::Renewed
        | ControllerChangeKind::Reconnected
        | ControllerChangeKind::TakenOver { .. } => EventOutcome::Completed,
        ControllerChangeKind::Disconnected { .. } => EventOutcome::Unavailable,
        ControllerChangeKind::Released | ControllerChangeKind::Expired => EventOutcome::Cancelled,
    }
}

fn controller_change_reason(change: crate::controller::ControllerChangeKind) -> &'static str {
    use crate::controller::ControllerChangeKind;
    match change {
        ControllerChangeKind::Acquired => "controller authority acquired explicitly",
        ControllerChangeKind::Renewed => "controller lease renewed explicitly",
        ControllerChangeKind::Reconnected => "controller reconnected within bounded grace",
        ControllerChangeKind::TakenOver { .. } => "controller takeover confirmed explicitly",
        ControllerChangeKind::Disconnected { .. } => {
            "controller disconnected; bounded reconnect grace started"
        }
        ControllerChangeKind::Released => "controller released authority",
        ControllerChangeKind::Expired => "controller reconnect grace expired",
    }
}

fn update_pending_managed_approvals(state: &mut HubState, event: &HostEvent) {
    match event {
        HostEvent::ManagedApprovalRequested(approval) => {
            state.pending_managed_approvals.insert(approval.approval_id);
        }
        HostEvent::ManagedApprovalResolved { approval_id, .. } => {
            state.pending_managed_approvals.remove(approval_id);
        }
        HostEvent::ManagedTurnFinished { .. } => state.pending_managed_approvals.clear(),
        _ => {}
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
