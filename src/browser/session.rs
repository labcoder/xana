//! One client-neutral browser task, with one controller and durable no-replay
//! receipts. Protected evidence and disposable browser storage stay distinct.

use super::{
    BrowserEffect, BrowserError, BrowserRequest, BrowserResolution, EGRESS_DISCLOSURE, MAX_ACTIONS,
    MAX_OBSERVATION_BYTES, MAX_TASK_SECONDS,
    cdp::CdpOwner,
    page::Page,
    process::OwnedBrowser,
    proxy::{EgressPolicy, Proxy},
};
use crate::{
    artifact::ArtifactRecord,
    identity::{OperationId, PrincipalId, SessionId},
    mcp::McpHttpSecurity,
    paths::XanaPaths,
    storage::ProtectedStore,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod dispatch;
mod evidence;
mod lifecycle;
mod review;

#[derive(Clone)]
pub(crate) struct BrowserOwner {
    inner: Arc<Owner>,
}
struct Owner {
    paths: XanaPaths,
    store: ProtectedStore,
    principal: PrincipalId,
    conversation: SessionId,
    executable: Option<PathBuf>,
    headless: bool,
    session: AsyncMutex<Option<Session>>,
    snapshot: Mutex<BrowserSnapshot>,
    control_stop: Mutex<Option<CancellationToken>>,
    dispatch: Mutex<dispatch::State>,
    effects: tokio_util::task::TaskTracker,
    #[cfg(test)]
    fixture: Option<(EgressPolicy, String)>,
    #[cfg(test)]
    startup_gate: Mutex<Option<tokio::sync::oneshot::Sender<Uuid>>>,
    #[cfg(test)]
    stale_cleanup_fixture: bool,
    #[cfg(test)]
    receipt_gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}
struct Session {
    id: Uuid,
    _transport: CdpOwner,
    page: Page,
    process: Option<OwnedBrowser>,
    _proxy: Proxy,
    stop: CancellationToken,
    started: Instant,
    actions: u32,
    takeover: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BrowserSnapshot {
    pub(crate) available: bool,
    pub(crate) task: Option<Uuid>,
    pub(crate) revision: u64,
    pub(crate) state: String,
    pub(crate) origins: Vec<String>,
    pub(crate) actions_remaining: u32,
    pub(crate) receipt_error: bool,
    pub(crate) disclosure: String,
}

#[derive(Clone, Debug)]
pub(crate) struct BrowserPlan {
    pub(crate) request: BrowserRequest,
    pub(crate) review: Option<Value>,
    revision: u64,
    epoch: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrowserReceipt {
    pub(crate) id: Uuid,
    pub(crate) task: Option<Uuid>,
    pub(crate) operation: OperationId,
    pub(crate) outcome: String,
    pub(crate) acknowledged: bool,
    pub(crate) snapshot: BrowserSnapshot,
    pub(crate) evidence: Option<ArtifactRecord>,
    pub(crate) observation: Option<Value>,
}

impl BrowserOwner {
    #[cfg(all(test, windows))]
    pub(super) async fn suppress_next_effect_reply_fixture(&self) {
        self.inner
            .session
            .lock()
            .await
            .as_ref()
            .unwrap()
            .page
            .suppress_next_effect_reply_fixture();
    }
    #[cfg(all(test, windows))]
    pub(super) async fn mutate_fixture_form(&self, mutation: &str) -> Result<(), BrowserError> {
        self.inner
            .session
            .lock()
            .await
            .as_ref()
            .ok_or(BrowserError::NoSession)?
            .page
            .mutate_fixture_form(mutation)
            .await
    }
    #[cfg(all(test, windows))]
    pub(super) async fn mutate_fixture_target(&self) -> Result<(), BrowserError> {
        self.inner
            .session
            .lock()
            .await
            .as_ref()
            .ok_or(BrowserError::NoSession)?
            .page
            .mutate_fixture_target()
            .await
    }
    #[cfg(all(test, windows))]
    pub(super) async fn metrics(&self) -> Result<Value, BrowserError> {
        self.inner
            .session
            .lock()
            .await
            .as_ref()
            .and_then(|live| live.process.as_ref())
            .ok_or(BrowserError::NoSession)?
            .metrics()
    }
    pub(super) fn paths(&self) -> &XanaPaths {
        &self.inner.paths
    }
    pub(crate) fn new(
        paths: XanaPaths,
        store: ProtectedStore,
        principal: PrincipalId,
        conversation: SessionId,
    ) -> Self {
        Self::with_executable(
            paths,
            store,
            principal,
            conversation,
            OwnedBrowser::discover(),
            false,
        )
    }
    pub(super) fn with_executable(
        paths: XanaPaths,
        store: ProtectedStore,
        principal: PrincipalId,
        conversation: SessionId,
        executable: Option<PathBuf>,
        headless: bool,
    ) -> Self {
        let available = executable.is_some() && cfg!(target_os = "windows");
        Self {
            inner: Arc::new(Owner {
                paths,
                store,
                principal,
                conversation,
                executable,
                headless,
                session: AsyncMutex::new(None),
                control_stop: Mutex::new(None),
                dispatch: Mutex::new(dispatch::State::default()),
                effects: {
                    let tasks = tokio_util::task::TaskTracker::new();
                    // Closed trackers can still track admitted work; wait()
                    // now means empty, with admission fenced by dispatch.
                    tasks.close();
                    tasks
                },
                snapshot: Mutex::new(BrowserSnapshot {
                    available,
                    task: None,
                    revision: 0,
                    state: "closed".into(),
                    origins: Vec::new(),
                    actions_remaining: MAX_ACTIONS,
                    receipt_error: false,
                    disclosure: EGRESS_DISCLOSURE.into(),
                }),
                #[cfg(test)]
                fixture: None,
                #[cfg(test)]
                startup_gate: Mutex::new(None),
                #[cfg(test)]
                stale_cleanup_fixture: false,
                #[cfg(test)]
                receipt_gate: Mutex::new(None),
            }),
        }
    }
    #[cfg(all(test, windows))]
    pub(super) fn native_fixture(
        mut self,
        origin: &str,
        address: std::net::SocketAddr,
        spki: String,
    ) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("fresh fixture owner");
        inner.executable = OwnedBrowser::discover();
        inner.snapshot.get_mut().unwrap().available = inner.executable.is_some();
        inner.fixture = Some((EgressPolicy::fixture(origin, Some(address)), spki));
        self
    }
    pub(crate) fn snapshot(&self) -> BrowserSnapshot {
        self.inner.snapshot.lock().expect("browser owner").clone()
    }
    #[cfg(test)]
    pub(crate) fn simulate_cleanup_failure(&self, task: Uuid) {
        let mut snapshot = self.inner.snapshot.lock().unwrap();
        snapshot.state = "cleanup_failed".into();
        snapshot.task = Some(task);
    }
    #[cfg(test)]
    pub(super) fn other_principal_fixture(&self) -> Self {
        Self::with_executable(
            self.inner.paths.clone(),
            self.inner.store.clone(),
            PrincipalId::new(),
            SessionId::new(),
            None,
            true,
        )
    }
    #[cfg(test)]
    pub(super) fn reopened_fixture(&self, principal: PrincipalId) -> Self {
        Self::with_executable(
            self.inner.paths.clone(),
            self.inner.store.clone(),
            principal,
            self.inner.conversation,
            None,
            true,
        )
    }
    #[cfg(test)]
    pub(super) fn same_principal_fixture(&self) -> Self {
        self.reopened_fixture(self.inner.principal)
    }
    #[cfg(test)]
    pub(super) fn lock_fixture(&self) {
        self.inner.store.lock().unwrap();
    }
    #[cfg(all(test, windows))]
    pub(super) fn cancel_fixture(&self) {
        self.inner
            .control_stop
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel();
    }
    #[cfg(test)]
    pub(crate) async fn hold_control_fixture(
        &self,
        ready: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        let _slot = self.inner.session.lock().await;
        let _ = ready.send(());
        let _ = release.await;
    }
    /// Revocation never waits for model approval or silently replays an input.
    /// Cancel transport first, then join exact process cleanup outside UI threads.
    pub(crate) fn request_shutdown(&self) {
        let mut dispatch = self.inner.dispatch.lock().expect("browser dispatch");
        if !dispatch.stopping {
            dispatch.shutdown_result = None;
        }
        dispatch.stopping = true;
        {
            let mut snapshot = self.inner.snapshot.lock().expect("browser owner");
            if snapshot.state != "cleanup_failed" {
                snapshot.state = "stopping".into();
            }
        }
        if let Some(stop) = &dispatch.cancelled {
            stop.cancel();
        }
        if let Some(stop) = self
            .inner
            .control_stop
            .lock()
            .expect("browser owner")
            .as_ref()
        {
            stop.cancel();
        }
    }
    pub(crate) async fn shutdown(&self) -> Result<(), BrowserError> {
        self.request_shutdown();
        self.join_shutdown().await
    }
    pub(crate) fn plan(&self, request: BrowserRequest) -> Result<BrowserPlan, BrowserError> {
        let session = self
            .inner
            .session
            .try_lock()
            .map_err(|_| BrowserError::Busy)?;
        let snapshot = self.snapshot();
        match &request {
            BrowserRequest::Launch { origins } => {
                self.cleanup_status()?;
                if !snapshot.available {
                    return Err(BrowserError::Unavailable);
                }
                if session.is_some() {
                    return Err(BrowserError::Busy);
                }
                EgressPolicy::parse(origins, McpHttpSecurity::default())?;
            }
            BrowserRequest::Close {} if session.is_none() => {}
            _ => {
                let live = session.as_ref().ok_or(BrowserError::NoSession)?;
                if live.takeover
                    && !matches!(
                        request,
                        BrowserRequest::Resume {}
                            | BrowserRequest::Close {}
                            | BrowserRequest::Takeover {}
                    )
                {
                    return Err(BrowserError::TakenOver);
                }
                if (live.started.elapsed().as_secs() >= MAX_TASK_SECONDS
                    || live.actions >= MAX_ACTIONS)
                    && !matches!(
                        request,
                        BrowserRequest::Close {} | BrowserRequest::Takeover {}
                    )
                {
                    return Err(BrowserError::Limit);
                }
            }
        }
        match &request {
            BrowserRequest::Navigate { url } if url.len() > 2048 => {
                return Err(BrowserError::InvalidInput);
            }
            BrowserRequest::Act {
                reference,
                effect,
                purpose,
            } => {
                if Uuid::parse_str(reference).is_err()
                    || purpose.trim().is_empty()
                    || purpose.len() > 1024
                {
                    return Err(BrowserError::InvalidInput);
                }
                if let BrowserEffect::Fill { text } = effect
                    && (text.len() > 4096 || text.chars().any(|c| c == '\0'))
                {
                    return Err(BrowserError::Limit);
                }
            }
            _ => {}
        }
        let epoch = if matches!(
            request,
            BrowserRequest::Close {} | BrowserRequest::Takeover {} | BrowserRequest::Resume {}
        ) {
            None
        } else {
            session.as_ref().map(|live| live.page.epoch()).transpose()?
        };
        Ok(BrowserPlan {
            review: match &request {
                BrowserRequest::Act {
                    reference, effect, ..
                } => Some(
                    session
                        .as_ref()
                        .ok_or(BrowserError::NoSession)?
                        .page
                        .preview(reference, effect)?,
                ),
                _ => None,
            },
            request,
            revision: snapshot.revision,
            epoch,
        })
    }

    async fn execute_owned(
        &self,
        plan: BrowserPlan,
        operation: OperationId,
        cancelled: CancellationToken,
    ) -> Result<BrowserReceipt, BrowserError> {
        let mut slot = self
            .inner
            .session
            .try_lock()
            .map_err(|_| BrowserError::Busy)?;
        if cancelled.is_cancelled() {
            return Err(BrowserError::Cancelled);
        }
        if self.snapshot().revision != plan.revision {
            return Err(BrowserError::Stale);
        }
        if matches!(
            plan.request,
            BrowserRequest::Launch { .. }
                | BrowserRequest::Navigate { .. }
                | BrowserRequest::Act { .. }
                | BrowserRequest::Resume {}
        ) {
            self.require_review_clear().await?;
        }
        if !matches!(
            plan.request,
            BrowserRequest::Close {} | BrowserRequest::Takeover {} | BrowserRequest::Resume {}
        ) && let (Some(live), Some(epoch)) = (slot.as_ref(), plan.epoch)
            && live.page.epoch()? != epoch
        {
            return Err(BrowserError::Stale);
        }
        let launch_id = matches!(plan.request, BrowserRequest::Launch { .. }).then(Uuid::new_v4);
        let mut receipt = BrowserReceipt {
            id: Uuid::new_v4(),
            task: launch_id.or(self.snapshot().task),
            operation,
            outcome: "intent; effect unknown until a receipt is recorded".into(),
            acknowledged: false,
            snapshot: self.snapshot(),
            evidence: None,
            observation: None,
        };
        self.persist(&receipt).await?;
        let review = if matches!(plan.request, BrowserRequest::Act { .. }) {
            Some(self.begin_review(&receipt, &plan).await?)
        } else {
            None
        };
        {
            let mut state = self.inner.snapshot.lock().expect("browser owner");
            state.revision = state.revision.checked_add(1).ok_or(BrowserError::Limit)?;
            if let Some(id) = launch_id {
                state.task = Some(id);
                state.state = "starting".into();
            }
        }
        let mut result = async {
            match plan.request {
                BrowserRequest::Launch { origins } => match self
                    .launch(
                        &origins,
                        launch_id.expect("launch identity"),
                        cancelled.clone(),
                    )
                    .await
                {
                    Ok(session) => {
                        receipt.task = Some(session.id);
                        *slot = Some(session);
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                BrowserRequest::Close {} => self.close_slot(&mut slot).await,
                request => {
                    let live = slot.as_mut().ok_or(BrowserError::NoSession)?;
                    if cancelled.is_cancelled() {
                        return Err(BrowserError::Cancelled);
                    }
                    if !matches!(request, BrowserRequest::Takeover {})
                        && (live.started.elapsed().as_secs() >= MAX_TASK_SECONDS
                            || live.actions >= MAX_ACTIONS)
                    {
                        return Err(BrowserError::Limit);
                    }
                    live.actions = live.actions.saturating_add(1);
                    let live_stop = live.stop.clone();
                    tokio::select! {
                      biased;
                      () = cancelled.cancelled() => Err(BrowserError::Cancelled),
                      () = live_stop.cancelled() => Err(BrowserError::Cancelled),
                      result = async { match request {
                        BrowserRequest::Navigate { url } => live.page.navigate(&url).await,
                        BrowserRequest::Observe {} | BrowserRequest::Resume {} => {
                            if matches!(request, BrowserRequest::Resume {}) {
                                live.page.invalidate();
                                live.takeover = false;
                            }
                            match live.page.observe().await {
                                Ok(observation) => {
                                    let value = serde_json::to_value(observation)
                                        .map_err(|_| BrowserError::Protocol)?;
                                    let bytes = serde_json::to_vec(&value)
                                        .map_err(|_| BrowserError::Protocol)?;
                                    receipt.evidence = Some(
                                        self.artifact(
                                            bytes,
                                            "application/json",
                                            MAX_OBSERVATION_BYTES,
                                        )
                                        .await?,
                                    );
                                    receipt.observation = Some(value);
                                    Ok(())
                                }
                                Err(error) => Err(error),
                            }
                        }
                        BrowserRequest::Screenshot {} => match live.page.screenshot().await {
                            Ok(bytes) => {
                                receipt.evidence = Some(
                                    self.artifact(
                                        bytes,
                                        "image/png",
                                        crate::artifact::MAX_ARTIFACT_BYTES,
                                    )
                                    .await?,
                                );
                                Ok(())
                            }
                            Err(error) => Err(error),
                        },
                        BrowserRequest::Act {
                            reference, effect, ..
                        } => live.page.act(&reference, &effect).await,
                        BrowserRequest::Takeover {} => {
                            live.page.invalidate();
                            live.takeover = true;
                            Ok(())
                        }
                        BrowserRequest::Launch { .. } | BrowserRequest::Close {} => unreachable!(),
                      } } => result,
                    }
                }
            }
        }
        .await;
        if result.is_err() && self.close_slot(&mut slot).await.is_err() {
            result = Err(BrowserError::Process);
        }
        self.publish(slot.as_ref());
        receipt.snapshot = self.snapshot();
        receipt.acknowledged = result.is_ok();
        receipt.outcome = match result {
            Ok(()) => "acknowledged; no claim of external business success".into(),
            Err(error) => error.to_string(),
        };
        #[cfg(test)]
        {
            let gate = self
                .inner
                .receipt_gate
                .lock()
                .expect("receipt fixture")
                .take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
        }
        self.persist(&receipt).await?;
        if let Some(review) = review {
            // An explicit native acknowledgement, or a definite pre-dispatch
            // rejection, settles the intent. Loss/cancellation/process failure
            // stays fenced across cleanup and restart until owner review.
            if receipt.acknowledged
                || matches!(
                    result,
                    Err(BrowserError::Stale
                        | BrowserError::InvalidInput
                        | BrowserError::UnsupportedEgress
                        | BrowserError::NoSession)
                )
            {
                self.settle_review(&review).await?;
            }
        }
        Ok(receipt)
    }
}
struct DispatchGuard {
    cancelled: CancellationToken,
    live_stop: Option<CancellationToken>,
    armed: bool,
}
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.cancel();
            if let Some(stop) = &self.live_stop {
                stop.cancel();
            }
        }
    }
}
