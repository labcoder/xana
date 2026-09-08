//! Resolve user-owned inputs at the application edge, not in Agent or a frontend.
use super::*;
use crate::native_runtime::configuration::{ExecutionRefresh, PreparedExecution};
use crate::profile::execution::ExecutionConfiguration;

pub(super) struct Refresh {
    pub(super) paths: XanaPaths,
    pub(super) browser: Option<crate::browser::BrowserOwner>,
}

pub(super) use crate::profile::execution::{inputs_digest, resolve_current as resolve};

impl ExecutionRefresh for Refresh {
    fn prepare<'a>(
        &'a self,
        session: &'a DurableSession,
        current: &'a ExecutionConfiguration,
    ) -> futures::future::BoxFuture<'a, Result<Option<PreparedExecution>>> {
        Box::pin(async move {
            let paths = self.paths.clone();
            let prior = current.profile.clone();
            let next = tokio::task::spawn_blocking(move || resolve(&paths, Some(&prior))).await??;
            if &next == current {
                return Ok(None);
            }
            anyhow::ensure!(
                !session.live_children(),
                "Settings are pending until retained child work is stopped or finished"
            );
            let composed =
                super::native::compose(&self.paths, session, next.clone(), self.browser.clone())
                    .await?;
            anyhow::ensure!(
                next.inputs_digest == inputs_digest(&self.paths)?,
                "Settings changed during preparation; retry this turn"
            );
            Ok(Some(composed.execution))
        })
    }
}
