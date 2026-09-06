//! One admitted effect and one shutdown joiner. Caller cancellation cannot drop
//! the task that owns cleanup and its durable result.
use super::*;

#[derive(Default)]
pub(super) struct State {
    busy: bool,
    pub(super) stopping: bool,
    shutdown_active: bool,
    pub(super) shutdown_result: Option<Result<(), BrowserError>>,
    pub(super) cancelled: Option<CancellationToken>,
}

impl BrowserOwner {
    pub(crate) async fn execute(
        &self,
        plan: BrowserPlan,
        operation: OperationId,
    ) -> Result<BrowserReceipt, BrowserError> {
        let cancelled = CancellationToken::new();
        let (task, live_stop) = {
            let mut state = self.inner.dispatch.lock().expect("browser dispatch");
            let inspect_failure = matches!(plan.request, BrowserRequest::Close {})
                && self.snapshot().state == "cleanup_failed"
                && !state.shutdown_active;
            if state.busy || (state.stopping && !inspect_failure) {
                return Err(BrowserError::Busy);
            }
            state.busy = true;
            state.cancelled = Some(cancelled.clone());
            let live_stop = self
                .inner
                .control_stop
                .lock()
                .expect("browser owner")
                .clone();
            let owner = self.clone();
            let stop = cancelled.clone();
            (
                self.inner.effects.spawn(async move {
                    let _admission = Admission(owner.clone());
                    owner.execute_owned(plan, operation, stop).await
                }),
                live_stop,
            )
        };
        let mut caller = DispatchGuard {
            cancelled,
            live_stop,
            armed: true,
        };
        let result = task.await.map_err(|_| BrowserError::Process)?;
        caller.armed = false;
        result
    }

    pub(super) async fn join_shutdown(&self) -> Result<(), BrowserError> {
        {
            let mut state = self.inner.dispatch.lock().expect("browser dispatch");
            if !state.shutdown_active && state.shutdown_result.is_none() {
                state.shutdown_active = true;
                state.shutdown_result = None;
                let owner = self.clone();
                self.inner.effects.spawn(async move {
                    // Never join effects from this worker: it belongs to that
                    // tracker. Holding admission closed makes the lock wait
                    // bounded by the existing transport/cleanup deadlines.
                    let mut slot = owner.inner.session.lock().await;
                    let result = if slot.is_none() {
                        owner.cleanup_status()
                    } else {
                        owner
                            .terminal_receipt(&mut slot, "controller shutdown")
                            .await
                            .and_then(|receipt| {
                                if receipt.acknowledged {
                                    Ok(())
                                } else {
                                    Err(BrowserError::Process)
                                }
                            })
                    };
                    let mut state = owner.inner.dispatch.lock().expect("browser dispatch");
                    state.shutdown_result = Some(result);
                    state.shutdown_active = false;
                    // Do not reopen admission until an external waiter joins
                    // the full tracker. Otherwise a newly launched watchdog
                    // could enter that same wait and delay shutdown indefinitely.
                    let mut snapshot = owner.inner.snapshot.lock().expect("browser owner");
                    if snapshot.state != "cleanup_failed" {
                        snapshot.state = "shutdown_pending".into();
                    }
                });
            }
        }
        tokio::time::timeout(Duration::from_secs(8), self.inner.effects.wait())
            .await
            .map_err(|_| BrowserError::TimedOut)?;
        let mut state = self.inner.dispatch.lock().expect("browser dispatch");
        let result = state.shutdown_result.unwrap_or(Err(BrowserError::Process));
        if result.is_ok() {
            state.stopping = false;
            let mut snapshot = self.inner.snapshot.lock().expect("browser owner");
            if matches!(snapshot.state.as_str(), "stopping" | "shutdown_pending") {
                snapshot.state = "closed".into();
            }
        }
        result
    }
}

struct Admission(BrowserOwner);
impl Drop for Admission {
    fn drop(&mut self) {
        let mut state = self.0.inner.dispatch.lock().expect("browser dispatch");
        state.busy = false;
        state.cancelled = None;
        if std::thread::panicking() {
            let mut snapshot = self.0.inner.snapshot.lock().expect("browser owner");
            if snapshot.task.is_some() {
                snapshot.state = "cleanup_failed".into();
            }
            state.stopping = true;
        }
    }
}
