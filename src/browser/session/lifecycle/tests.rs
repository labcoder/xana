use super::*;
use crate::storage::{RecoveryIdentity, TestCustody};

fn owner() -> (tempfile::TempDir, BrowserOwner) {
    let root = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(root.path().as_os_str().to_owned())).unwrap();
    let store = ProtectedStore::initialize(
        paths.data_dir(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    (
        root,
        BrowserOwner::with_executable(paths, store, PrincipalId::new(), None, true),
    )
}

#[tokio::test]
async fn shutdown_fences_new_admission_before_its_worker_runs() {
    let (_root, owner) = owner();
    let plan = owner.plan(BrowserRequest::Close {}).unwrap();
    owner.request_shutdown();
    assert_eq!(
        owner.execute(plan, OperationId::new()).await.unwrap_err(),
        BrowserError::Busy
    );
    owner.shutdown().await.unwrap();
    let receipt = owner
        .execute(
            owner.plan(BrowserRequest::Close {}).unwrap(),
            OperationId::new(),
        )
        .await
        .unwrap();
    assert!(receipt.acknowledged);
}

#[tokio::test]
async fn repeated_shutdown_joins_one_cleanup_without_new_effects() {
    let (_root, owner) = owner();
    let (a, b) = tokio::join!(owner.shutdown(), owner.shutdown());
    assert_eq!(a, Ok(()));
    assert_eq!(b, Ok(()));
    assert_eq!(owner.snapshot().state, "closed");
    assert!(owner.receipts(64).await.unwrap().is_empty());
}

#[tokio::test]
async fn dropped_close_caller_does_not_cancel_receipt_and_shutdown_joins_it() {
    let (_root, mut owner) = owner();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    *Arc::get_mut(&mut owner.inner)
        .unwrap()
        .receipt_gate
        .get_mut()
        .unwrap() = Some((entered, wait));
    let plan = owner.plan(BrowserRequest::Close {}).unwrap();
    let caller = {
        let owner = owner.clone();
        let plan = plan.clone();
        tokio::spawn(async move { owner.execute(plan, OperationId::new()).await })
    };
    ready.await.unwrap();
    assert_eq!(
        owner.execute(plan, OperationId::new()).await.unwrap_err(),
        BrowserError::Busy
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let cleanup = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    tokio::task::yield_now().await;
    assert!(!cleanup.is_finished());
    release.send(()).unwrap();
    cleanup.await.unwrap().unwrap();
    let receipts = owner.receipts(64).await.unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(
        receipts[0].acknowledged,
        "completed close lost its final receipt"
    );
    assert_eq!(owner.snapshot().state, "closed");
}

#[tokio::test]
async fn dropping_shutdown_waiter_does_not_drop_the_cleanup_worker() {
    let (_root, mut owner) = owner();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    *Arc::get_mut(&mut owner.inner)
        .unwrap()
        .receipt_gate
        .get_mut()
        .unwrap() = Some((entered, wait));
    let caller = {
        let owner = owner.clone();
        tokio::spawn(async move {
            owner
                .execute(
                    owner.plan(BrowserRequest::Close {}).unwrap(),
                    OperationId::new(),
                )
                .await
        })
    };
    ready.await.unwrap();
    let cleanup = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    tokio::task::yield_now().await;
    cleanup.abort();
    let _ = cleanup.await;
    release.send(()).unwrap();
    caller.await.unwrap().unwrap();
    owner.inner.effects.wait().await;
    assert_eq!(owner.snapshot().state, "shutdown_pending");
    assert_eq!(
        owner
            .execute(
                owner.plan(BrowserRequest::Close {}).unwrap(),
                OperationId::new()
            )
            .await
            .unwrap_err(),
        BrowserError::Busy
    );
    owner.shutdown().await.unwrap();
    assert_eq!(owner.snapshot().state, "closed");
    assert!(owner.receipts(64).await.unwrap()[0].acknowledged);
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "explicit installed Edge qualification; fresh disposable profile only"]
async fn dropped_startup_is_joined_and_its_profile_and_receipt_are_accounted_for() {
    cancelled_startup(false).await;
    cancelled_startup(true).await;
}

#[cfg(windows)]
async fn cancelled_startup(stale_identity: bool) {
    let (_root, mut owner) = owner();
    let (entered, ready) = tokio::sync::oneshot::channel();
    {
        let inner = Arc::get_mut(&mut owner.inner).unwrap();
        inner.executable = Some(OwnedBrowser::discover().expect("installed qualified Edge"));
        inner.snapshot.get_mut().unwrap().available = true;
        inner.fixture = Some((
            EgressPolicy::fixture("https://fixture.invalid", None),
            format!("{}=", "A".repeat(43)),
        ));
        *inner.startup_gate.get_mut().unwrap() = Some(entered);
        inner.stale_cleanup_fixture = stale_identity;
    }
    let plan = owner
        .plan(BrowserRequest::Launch {
            origins: vec!["https://fixture.invalid".into()],
        })
        .unwrap();
    let caller = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.execute(plan, OperationId::new()).await })
    };
    let task = tokio::time::timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    let profile = owner
        .paths()
        .cache_dir()
        .join("browser")
        .join(task.to_string());
    assert!(profile.is_dir());
    assert_eq!(owner.snapshot().task, Some(task));
    assert_eq!(owner.snapshot().state, "starting");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let cleanup = owner.shutdown().await;
    if stale_identity {
        assert_eq!(cleanup, Err(BrowserError::Process));
        assert!(
            profile.exists(),
            "must not remove a different profile identity"
        );
        assert_eq!(owner.snapshot().state, "cleanup_failed");
        assert_eq!(owner.snapshot().task, Some(task));
        assert!(matches!(
            owner.plan(BrowserRequest::Launch {
                origins: vec!["https://fixture.invalid".into()]
            }),
            Err(BrowserError::Process)
        ));
    } else {
        cleanup.unwrap();
        assert!(
            !profile.exists(),
            "cancelled launch left plaintext browser profile"
        );
        assert_eq!(owner.snapshot().state, "closed");
    }
    let receipts = owner.receipts(64).await.unwrap();
    let receipt = receipts
        .iter()
        .find(|receipt| receipt.task == Some(task))
        .unwrap();
    assert!(!receipt.acknowledged);
    assert!(receipt.outcome.contains(if stale_identity {
        "Process"
    } else {
        "Cancelled"
    }));
}
