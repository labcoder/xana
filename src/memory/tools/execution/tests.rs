use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn aborted_tool_cancels_its_worker_and_cleanup_joins_it() {
    let cleanup = DeferredCleanup::default();
    let cancellation = CancellationToken::new();
    let (started, start) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = Arc::clone(&finished);
    let worker_cleanup = cleanup.clone();
    let worker_cancel = cancellation.clone();
    let waiter = tokio::spawn(async move {
        run(&worker_cleanup, worker_cancel, move || {
            let _ = started.send(());
            wait.recv().unwrap();
            worker_finished.store(true, Ordering::SeqCst);
            Ok("finished".into())
        })
        .await
    });
    start.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(cancellation.is_cancelled());
    let drain = tokio::spawn(async move { cleanup.drain().await });
    tokio::task::yield_now().await;
    assert!(!drain.is_finished());
    assert!(!finished.load(Ordering::SeqCst));
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap();
    assert!(finished.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closed_cleanup_owner_never_starts_store_work() {
    let cleanup = DeferredCleanup::default();
    cleanup.drain().await;
    let result = run(&cleanup, CancellationToken::new(), || {
        panic!("unowned work must not run")
    })
    .await;
    assert!(result.unwrap_err().contains("nothing was changed"));
}
