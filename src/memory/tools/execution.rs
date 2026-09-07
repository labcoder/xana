//! Bounded store work remains owned when its awaiting tool future is aborted.
use crate::tool::DeferredCleanup;
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    cleanup: &DeferredCleanup,
    cancellation: CancellationToken,
    work: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
) -> Result<String, String> {
    // Only this worker's child token is cancelled, not a completed parent turn.
    let _cancel_on_drop = cancellation.drop_guard();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    // Register before starting the blocking worker. A closed/full cleanup owner
    // cannot leave an already-started effect without a join owner.
    if !cleanup.schedule(Box::pin(async move {
        let result = tokio::task::spawn_blocking(work)
            .await
            .map_err(|_| {
                "Memory worker stopped unexpectedly; inspect memory before retrying.".to_owned()
            })
            .and_then(|result| result.map_err(|error| error.to_string()));
        let _ = sender.send(result);
    })) {
        return Err(
            "Memory worker could not obtain an execution owner; nothing was changed.".into(),
        );
    }
    receiver.await.map_err(|_| {
        "Memory cleanup stopped before an outcome was delivered; inspect memory before retrying.".to_owned()
    })?
}

#[cfg(test)]
mod tests;
