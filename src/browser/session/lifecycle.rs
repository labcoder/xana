//! Owned native process and timer lifecycle; one exact task is cleaned once.
use super::*;

#[cfg(test)]
mod tests;

impl BrowserOwner {
    pub(super) fn publish(&self, session: Option<&Session>) {
        let mut snapshot = self.inner.snapshot.lock().expect("browser owner");
        if session.is_none() && snapshot.state == "cleanup_failed" {
            return;
        }
        snapshot.task = session.map(|live| live.id);
        snapshot.state = match session {
            Some(live) if live.takeover => "manual_takeover",
            Some(_) => "ready",
            None => "closed",
        }
        .into();
        snapshot.actions_remaining =
            session.map_or(MAX_ACTIONS, |live| MAX_ACTIONS.saturating_sub(live.actions));
        if session.is_none() {
            snapshot.origins.clear();
            *self.inner.control_stop.lock().expect("browser owner") = None;
        }
    }
    pub(super) async fn launch(
        &self,
        origins: &[String],
        id: Uuid,
        stop: CancellationToken,
    ) -> Result<Session, BrowserError> {
        let executable = self
            .inner
            .executable
            .as_ref()
            .ok_or(BrowserError::Unavailable)?;
        let policy = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(BrowserError::Cancelled),
            policy = self.resolve_policy(origins) => policy?,
        };
        *self.inner.control_stop.lock().expect("browser owner") = Some(stop.clone());
        let profile = self
            .inner
            .paths
            .cache_dir()
            .join("browser")
            .join(id.to_string());
        std::fs::create_dir_all(profile.parent().ok_or(BrowserError::Process)?)
            .map_err(|_| BrowserError::Process)?;
        std::fs::create_dir(&profile).map_err(|_| BrowserError::Process)?;
        let mut proxy = match tokio::select! {
            biased;
            () = stop.cancelled() => Err(BrowserError::Cancelled),
            proxy = Proxy::start(policy.clone(), stop.clone()) => proxy,
        } {
            Ok(proxy) => proxy,
            Err(error) => {
                self.remove_empty_profile(&profile, id)?;
                return Err(error);
            }
        };
        let process = match self.launch_process(executable, profile.clone(), proxy.address) {
            Ok(process) => process,
            Err(error) => {
                // A failed suspended launch may leave only our empty allocation.
                // Never recurse after losing process/profile ownership evidence.
                let proxy_cleanup = proxy.close().await;
                self.remove_empty_profile(&profile, id)?;
                self.record_cleanup(proxy_cleanup, id)?;
                return Err(error);
            }
        };
        #[cfg(test)]
        let process = {
            let mut process = process;
            if self.inner.stale_cleanup_fixture {
                process.invalidate_cleanup_identity_for_fixture(self.paths().cache_dir());
            }
            process
        };
        // Keep the transport outside the cancellable initialization future, so
        // failed Page::start and interrupted startup can still join its tasks.
        let mut transport = None;
        let initialized = tokio::select! {
          biased;
          () = stop.cancelled() => Err(BrowserError::Cancelled),
          result = async {
            #[cfg(test)]
            {
                let gate = self.inner.startup_gate.lock().expect("startup fixture").take();
                if let Some(entered) = gate {
                    let _ = entered.send(id);
                    // The native fixture cancels while an owned process and
                    // plaintext allocation exist, before endpoint handoff.
                    std::future::pending::<()>().await;
                }
            }
            let endpoint = process.endpoint().await?;
            transport = Some(CdpOwner::connect(&endpoint, policy.clone(), stop.clone()).await?);
            Page::start(transport.as_ref().expect("connected transport").connection.clone(), policy.clone()).await
          } => result,
        };
        let page = match initialized {
            Ok(value) => value,
            Err(error) => {
                self.inner.snapshot.lock().expect("browser owner").state = "cleaning_up".into();
                stop.cancel();
                let (process, proxy, transport) =
                    tokio::join!(process.close(), proxy.close(), async {
                        match transport.as_mut() {
                            Some(transport) => transport.close().await,
                            None => Ok(()),
                        }
                    });
                self.record_cleanup(process.and(proxy).and(transport), id)?;
                return Err(error);
            }
        };
        self.inner.snapshot.lock().expect("browser owner").origins = policy.origins;
        let owner = Arc::downgrade(&self.inner);
        let timer_stop = stop.clone();
        self.inner.effects.spawn(async move {
            tokio::select! { () = timer_stop.cancelled() => {}, () = tokio::time::sleep(Duration::from_secs(MAX_TASK_SECONDS)) => { timer_stop.cancel(); } }
            if let Some(owner) = owner.upgrade() {
                let owner = Self { inner: owner };
                loop {
                    if owner.snapshot().task != Some(id) {
                        break;
                    }
                    // Do not queue behind an intentional close and briefly
                    // steal the next task's admission lock after it completes.
                    if let Ok(mut slot) = owner.inner.session.try_lock() {
                        if slot.as_ref().is_some_and(|live| live.id == id) {
                            let _ = owner.terminal_receipt(&mut slot, "cancelled or task deadline reached; inspect any earlier uncertain effect").await;
                        }
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });
        Ok(Session {
            id,
            transport: transport.expect("initialized transport"),
            page,
            process: Some(process),
            proxy,
            stop,
            started: Instant::now(),
            actions: 0,
            takeover: false,
        })
    }
    pub(super) fn cleanup_status(&self) -> Result<(), BrowserError> {
        if self.snapshot().state == "cleanup_failed" {
            Err(BrowserError::Process)
        } else {
            Ok(())
        }
    }
    pub(super) async fn close_slot(&self, slot: &mut Option<Session>) -> Result<(), BrowserError> {
        let Some(mut live) = slot.take() else {
            return self.cleanup_status();
        };
        live.stop.cancel();
        let process = async {
            match live.process.take() {
                Some(process) => {
                    self.inner.snapshot.lock().expect("browser owner").state = "cleaning_up".into();
                    process.close().await
                }
                None => Ok(()),
            }
        };
        let (process, transport, proxy) =
            tokio::join!(process, live.transport.close(), live.proxy.close());
        let result = process.and(transport).and(proxy);
        let result = self.record_cleanup(result, live.id);
        if result.is_ok() {
            self.publish(None);
        }
        let mut snapshot = self.inner.snapshot.lock().expect("browser owner");
        snapshot.revision = snapshot.revision.saturating_add(1);
        result
    }
    fn record_cleanup(
        &self,
        result: Result<(), BrowserError>,
        id: Uuid,
    ) -> Result<(), BrowserError> {
        if result.is_err() {
            let mut snapshot = self.inner.snapshot.lock().expect("browser owner");
            snapshot.task = Some(id);
            snapshot.state = "cleanup_failed".into();
        }
        result
    }
    fn remove_empty_profile(
        &self,
        profile: &std::path::Path,
        id: Uuid,
    ) -> Result<(), BrowserError> {
        self.record_cleanup(
            std::fs::remove_dir(profile).map_err(|_| BrowserError::Process),
            id,
        )
    }
    pub(super) async fn terminal_receipt(
        &self,
        slot: &mut Option<Session>,
        reason: &str,
    ) -> Result<BrowserReceipt, BrowserError> {
        let mut receipt = BrowserReceipt {
            id: Uuid::new_v4(),
            task: self.snapshot().task,
            operation: OperationId::new(),
            outcome: format!("cleanup requested: {reason}"),
            acknowledged: false,
            snapshot: self.snapshot(),
            evidence: None,
            observation: None,
        };
        // Failed storage must never prevent revocation and process termination.
        let intent = self.persist(&receipt).await;
        let cleanup = self.close_slot(slot).await;
        receipt.snapshot = self.snapshot();
        receipt.acknowledged = cleanup.is_ok();
        receipt.outcome = if cleanup.is_ok() {
            format!("closed: {reason}")
        } else {
            format!("cleanup failed; owned profile may remain: {reason}")
        };
        self.persist(&receipt).await?;
        intent?;
        Ok(receipt)
    }
    async fn resolve_policy(&self, origins: &[String]) -> Result<EgressPolicy, BrowserError> {
        #[cfg(test)]
        if let Some((policy, _)) = &self.inner.fixture {
            return Ok(policy.clone());
        }
        EgressPolicy::resolve(origins, McpHttpSecurity::default()).await
    }
    fn launch_process(
        &self,
        executable: &std::path::Path,
        profile: PathBuf,
        address: std::net::SocketAddr,
    ) -> Result<OwnedBrowser, BrowserError> {
        #[cfg(test)]
        if let Some((_, spki)) = &self.inner.fixture {
            return OwnedBrowser::launch_fixture(
                executable,
                profile,
                address,
                self.inner.headless,
                spki,
            );
        }
        OwnedBrowser::launch(executable, profile, address, self.inner.headless)
    }
}
