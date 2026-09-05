use super::*;
use crate::{
    identity::StepId,
    message::{Message, Role},
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    storage::{RecoveryIdentity, TestCustody},
    tool::ToolDefinition,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn fixture() -> (tempfile::TempDir, MemoryOwner, LearningRoute, TestCustody) {
    let home = tempfile::tempdir().unwrap();
    let custody = TestCustody::default();
    let store =
        ProtectedStore::initialize(home.path(), &RecoveryIdentity::generate(), &custody).unwrap();
    let owner = MemoryOwner::new(
        store,
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            ..Default::default()
        },
    );
    let route = LearningRoute {
        connection: "fixture".into(),
        model: "synthetic".into(),
        digest: "no-network".into(),
    };
    owner
        .store
        .set_document(
            "memory/learning-route",
            &serde_json::to_vec(&route).unwrap(),
            4096,
        )
        .unwrap();
    (home, owner, route, custody)
}

#[derive(Default)]
struct CountingHelper(AtomicUsize);
impl ConversationalProvider for CountingHelper {
    fn stream_message<'a>(
        &'a self,
        _: &'a [Message],
        _: &'a [&'a ToolDefinition],
        _: StepId,
        _: &'a dyn DeltaSink,
    ) -> futures::future::BoxFuture<'a, std::result::Result<Message, ProviderError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Message::text(Role::Assistant, "[]"))
        })
    }
}

#[tokio::test]
async fn sixty_five_opted_out_sources_never_reach_the_helper() {
    let (_home, owner, route, _custody) = fixture();
    for _ in 0..65 {
        assert!(
            owner
                .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
                .unwrap()
        );
    }
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                learning_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let provider = Arc::new(CountingHelper::default());
    let worker = LearningWorker {
        store: owner.store.clone(),
        route,
        provider: provider.clone(),
        validate_route: Arc::new(|_| Ok(())),
    };
    assert_eq!(
        worker
            .process(true, &CancellationToken::new())
            .await
            .unwrap(),
        0
    );
    let first = owner.store.learning_status().unwrap();
    assert_eq!(first.pending, 1, "retirement remains a bounded 64-row pass");
    assert_eq!(first.excluded_after_change, 64);
    assert_eq!(first.last_retirement.unwrap().sources, 64);
    assert_eq!(provider.0.load(Ordering::Relaxed), 0);
    assert!(owner.store.usage_page(None, None, None).unwrap().is_empty());
    assert_eq!(
        worker
            .process(true, &CancellationToken::new())
            .await
            .unwrap(),
        0
    );
    let second = owner.store.learning_status().unwrap();
    assert_eq!(second.pending, 0);
    assert_eq!(second.excluded_after_change, 65);
    assert_eq!(second.last_retirement.unwrap().sources, 1);
    assert_eq!(provider.0.load(Ordering::Relaxed), 0);
    assert!(owner.store.usage_page(None, None, None).unwrap().is_empty());
}

#[test]
fn learning_batch_rechecks_eligibility_without_a_retirement_pass() {
    let (_home, owner, _route, _custody) = fixture();
    let old = Uuid::new_v4();
    owner.enqueue_user_statement(old, "I use Rust").unwrap();
    // An unrelated use-control revision also invalidates old source admission.
    // Learning remains independently enabled for newly accepted owner input.
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                use_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let fresh = Uuid::new_v4();
    assert!(
        owner
            .enqueue_user_statement(fresh, "I prefer examples")
            .unwrap()
    );
    let batch = owner.store.learning_batch().unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].id, fresh);
    assert_eq!(owner.store.learning_status().unwrap().pending, 2);
    assert_eq!(
        owner.store.learning_status().unwrap().excluded_after_change,
        0
    );
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                no_memory: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(owner.store.learning_batch().unwrap().is_empty());
}

#[test]
fn retirement_receipt_survives_reopen_without_rebasing_sources() {
    let (home, owner, _route, custody) = fixture();
    let id = Uuid::new_v4();
    let private_text = "I prefer examples";
    owner.enqueue_user_statement(id, private_text).unwrap();
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                use_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    owner.store.retire_stale_learning().unwrap();
    let status = owner.store.learning_status().unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(status.excluded_after_change, 1);
    let receipt = status.last_retirement.unwrap();
    assert_eq!(receipt.sources, 1);
    assert!(receipt.at_unix_seconds > 0);
    assert!(receipt.reason.contains("without automatic rebasing"));
    let serialized = serde_json::to_string(&receipt).unwrap();
    assert!(!serialized.contains(private_text));
    assert!(!serialized.contains(&id.to_string()));
    owner.store.retire_stale_learning().unwrap();
    assert_eq!(
        serde_json::to_string(
            &owner
                .store
                .learning_status()
                .unwrap()
                .last_retirement
                .unwrap()
        )
        .unwrap(),
        serialized,
        "a no-op retirement must not erase the last visible reason"
    );
    let context = owner.context.clone();
    drop(owner);
    let reopened = MemoryOwner::new(
        ProtectedStore::open(home.path(), &custody).unwrap(),
        context,
    );
    let status = reopened.store.learning_status().unwrap();
    assert_eq!(status.excluded_after_change, 1);
    assert_eq!(
        serde_json::to_string(&status.last_retirement.unwrap()).unwrap(),
        serialized
    );
    assert!(!reopened.enqueue_user_statement(id, private_text).unwrap());
    assert!(reopened.store.learning_batch().unwrap().is_empty());
    assert!(
        reopened
            .enqueue_user_statement(Uuid::new_v4(), private_text)
            .unwrap()
    );
    assert_eq!(reopened.store.learning_batch().unwrap().len(), 1);
}
