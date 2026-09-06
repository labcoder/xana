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
        BrowserOwner::with_executable(
            paths,
            store,
            PrincipalId::new(),
            SessionId::new(),
            None,
            true,
        ),
    )
}

async fn unresolved(owner: &BrowserOwner) -> BrowserReview {
    let receipt = BrowserReceipt {
        id: Uuid::new_v4(),
        task: Some(Uuid::new_v4()),
        operation: OperationId::new(),
        outcome: "intent before native dispatch".into(),
        acknowledged: false,
        snapshot: owner.snapshot(),
        evidence: None,
        observation: None,
    };
    let plan = BrowserPlan {
        request: BrowserRequest::Act {
            reference: Uuid::new_v4().to_string(),
            effect: BrowserEffect::Click {},
            purpose: "Submit synthetic form once".into(),
        },
        review: Some(
            serde_json::json!({"url":"https://example.com/submit","label":"Submit fixture","element":{"form":{"action":"https://example.com/submit","method":"post"}}}),
        ),
        revision: 0,
        epoch: None,
    };
    owner.persist(&receipt).await.unwrap();
    owner.begin_review(&receipt, &plan).await.unwrap()
}

#[tokio::test]
async fn unresolved_intent_survives_reconstruction_and_cleanup_until_exact_owner_review() {
    let (_root, owner) = owner();
    let review = unresolved(&owner).await;
    // Restart has a new ephemeral artifact principal, but the same Conversation.
    let reopened = owner.reopened_fixture(PrincipalId::new());
    assert_eq!(
        reopened.pending_review().await.unwrap().unwrap().receipt,
        review.receipt
    );
    assert_eq!(
        reopened.require_review_clear().await,
        Err(BrowserError::ReviewRequired)
    );
    reopened.shutdown().await.unwrap();
    assert_eq!(
        reopened.require_review_clear().await,
        Err(BrowserError::ReviewRequired)
    );
    assert_eq!(
        reopened
            .resolve(
                Uuid::new_v4(),
                review.revision,
                BrowserResolution::NotApplied
            )
            .await
            .unwrap_err(),
        BrowserError::Stale
    );
    assert_eq!(
        reopened
            .resolve(
                review.receipt,
                review.revision + 1,
                BrowserResolution::NotApplied
            )
            .await
            .unwrap_err(),
        BrowserError::Stale
    );
    let different = owner.other_principal_fixture();
    assert!(different.pending_review().await.unwrap().is_none());
    assert_eq!(
        different
            .resolve(review.receipt, review.revision, BrowserResolution::Applied)
            .await
            .unwrap_err(),
        BrowserError::Stale
    );
    let resolution = reopened
        .resolve(review.receipt, review.revision, BrowserResolution::Applied)
        .await
        .unwrap();
    assert!(resolution.acknowledged);
    assert!(resolution.outcome.contains("no automatic retry"));
    assert!(owner.require_review_clear().await.is_ok());
    assert_eq!(
        reopened
            .resolve(review.receipt, review.revision, BrowserResolution::Applied)
            .await
            .unwrap_err(),
        BrowserError::Stale
    );
}

#[tokio::test]
async fn unresolved_intent_blocks_dispatch_even_if_a_new_plan_was_already_prepared() {
    let (_root, owner) = owner();
    let review = unresolved(&owner).await;
    let stale_process_plan = BrowserPlan {
        request: BrowserRequest::Launch {
            origins: vec!["https://example.com".into()],
        },
        review: None,
        revision: owner.snapshot().revision,
        epoch: None,
    };
    assert_eq!(
        owner
            .execute(stale_process_plan, OperationId::new())
            .await
            .unwrap_err(),
        BrowserError::ReviewRequired
    );
    // Repeated no-op closes cannot evict the separately retained fence.
    for _ in 0..65 {
        owner
            .execute(
                owner.plan(BrowserRequest::Close {}).unwrap(),
                OperationId::new(),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        owner.pending_review().await.unwrap().unwrap().receipt,
        review.receipt
    );
    assert!(serde_json::from_value::<BrowserRequest>(serde_json::json!({"op":"resolve","receipt":review.receipt,"revision":review.revision,"outcome":"not_applied"})).is_err());
}

#[tokio::test]
async fn concurrent_intents_use_compare_exchange_instead_of_overwriting_a_fence() {
    let (_root, owner) = owner();
    let (previous, state) = owner.read_review().await.unwrap();
    let review = unresolved(&owner).await;
    assert_eq!(
        owner.replace_review(previous, state).await,
        Err(BrowserError::Stale)
    );
    assert_eq!(
        owner.pending_review().await.unwrap().unwrap().receipt,
        review.receipt
    );
}

#[tokio::test]
async fn corrupt_or_locked_review_state_never_restores_authority() {
    let (_root, owner) = owner();
    owner
        .inner
        .store
        .set_document(
            &format!("browser/review-state/{}", owner.inner.conversation),
            b"malformed",
            MAX_STATE_BYTES,
        )
        .unwrap();
    assert_eq!(
        owner.require_review_clear().await,
        Err(BrowserError::Storage)
    );
    owner.lock_fixture();
    assert!(owner.pending_review().await.is_err());
}
