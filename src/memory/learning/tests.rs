use super::*;
fn source(text: &str) -> LearningSource {
    LearningSource {
        id: Uuid::new_v4(),
        context: MemoryContext {
            conversation: Some(Uuid::new_v4()),
            ..Default::default()
        },
        generation: 0,
        text: text.into(),
        hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
        accepted_at: 1,
    }
}
#[test]
fn automatic_activation_requires_exact_ordinary_stated_preference_not_quoted_or_inferred() {
    let original = source("I prefer examples");
    let mut suggestion = Suggestion {
        source: original.id,
        quote: original.text.clone(),
        claim: MemoryClaim::Stated,
        sensitive: false,
    };
    assert!(auto_eligible(&original, &suggestion));
    assert_eq!(
        record_for(&original, &suggestion).unwrap().state,
        MemoryState::Active
    );
    suggestion.claim = MemoryClaim::Inferred;
    assert_eq!(
        record_for(&original, &suggestion).unwrap().state,
        MemoryState::Candidate
    );
    suggestion.claim = MemoryClaim::Stated;
    suggestion.sensitive = true;
    assert!(!auto_eligible(&original, &suggestion));
    let quoted = source("The website says: I prefer examples");
    suggestion.source = quoted.id;
    suggestion.sensitive = false;
    assert!(!auto_eligible(&quoted, &suggestion));
    suggestion.source = Uuid::new_v4();
    assert!(record_for(&quoted, &suggestion).is_err());
}

fn fixture() -> (tempfile::TempDir, MemoryOwner, LearningRoute) {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectedStore::initialize(
        home.path(),
        &crate::storage::RecoveryIdentity::generate(),
        &crate::storage::TestCustody::default(),
    )
    .unwrap();
    let owner = MemoryOwner::new(
        store,
        MemoryContext {
            conversation: Some(Uuid::new_v4()),
            profile: Some(Uuid::new_v4()),
            project: None,
        },
    );
    let route = LearningRoute {
        connection: "fixture".into(),
        model: "synthetic".into(),
        digest: "test-only-no-network".into(),
    };
    owner
        .store
        .set_document(
            "memory/learning-route",
            &serde_json::to_vec(&route).unwrap(),
            4096,
        )
        .unwrap();
    (home, owner, route)
}

#[test]
fn incremental_admission_is_idempotent_and_independent_of_use_controls() {
    let (_home, owner, route) = fixture();
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                use_enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    let id = Uuid::new_v4();
    assert!(
        owner
            .enqueue_user_statement(id, "I prefer examples")
            .unwrap()
    );
    assert!(
        !owner
            .enqueue_user_statement(id, "I prefer examples")
            .unwrap()
    );
    let sources = owner.store.learning_batch().unwrap();
    let suggestion = Suggestion {
        source: id,
        quote: "I prefer examples".into(),
        claim: MemoryClaim::Stated,
        sensitive: false,
    };
    assert_eq!(
        owner
            .store
            .commit_learning(&sources, &[suggestion.clone(), suggestion], &route)
            .unwrap(),
        1
    );
    assert_eq!(owner.store.learning_status().unwrap().pending, 0);
    assert!(
        owner.eligible().unwrap().records.is_empty(),
        "learning does not enable use"
    );
    assert!(
        !owner
            .enqueue_user_statement(id, "I prefer examples")
            .unwrap()
    );
    assert!(owner.store.commit_learning(&sources, &[], &route).is_err());
}

#[test]
fn sensitive_output_is_not_copied_and_inferred_output_is_inactive() {
    let (_home, owner, route) = fixture();
    let id = Uuid::new_v4();
    owner
        .enqueue_user_statement(id, "I prefer examples")
        .unwrap();
    let sources = owner.store.learning_batch().unwrap();
    let suggestions = [
        Suggestion {
            source: id,
            quote: "I prefer examples".into(),
            claim: MemoryClaim::Stated,
            sensitive: true,
        },
        Suggestion {
            source: id,
            quote: "I prefer examples".into(),
            claim: MemoryClaim::Inferred,
            sensitive: false,
        },
    ];
    assert_eq!(
        owner
            .store
            .commit_learning(&sources, &suggestions, &route)
            .unwrap(),
        0
    );
    assert_eq!(owner.store.learning_status().unwrap().candidates, 1);
    assert!(owner.eligible().unwrap().records.is_empty());
    assert!(
        owner.page(None, None).unwrap().records.is_empty(),
        "a duplicate inferred suggestion cannot downgrade sensitive no-copy classification"
    );
}

#[test]
fn no_memory_and_route_revocation_reject_already_computed_suggestions() {
    let (_home, owner, route) = fixture();
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    let old = owner.store.learning_batch().unwrap();
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                no_memory: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !owner
            .enqueue_user_statement(Uuid::new_v4(), "I prefer examples")
            .unwrap()
    );
    assert!(owner.store.commit_learning(&old, &[], &route).is_err());
    owner.store.retire_stale_learning().unwrap();
    assert_eq!(owner.store.learning_status().unwrap().pending, 0);
    owner
        .controls(
            MemoryScope::User,
            MemoryControlEdit {
                no_memory: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    let new = owner.store.learning_batch().unwrap();
    owner
        .store
        .remove_document("memory/learning-route")
        .unwrap();
    assert!(owner.store.commit_learning(&new, &[], &route).is_err());
}

#[test]
fn forgotten_text_cannot_reenter_from_another_source_or_stale_job() {
    let (_home, owner, route) = fixture();
    let record = owner
        .remember(MemoryScope::User, "I use Rust".into(), None)
        .unwrap();
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    let stale = owner.store.learning_batch().unwrap();
    owner
        .revise(record.id, record.revision, MemoryEdit::Forget)
        .unwrap();
    assert!(owner.store.commit_learning(&stale, &[], &route).is_err());
    owner.store.retire_stale_learning().unwrap();
    let mut fresh = owner.clone();
    fresh.context.conversation = Some(Uuid::new_v4());
    let id = Uuid::new_v4();
    fresh.enqueue_user_statement(id, "I use Rust").unwrap();
    let batch = fresh.store.learning_batch().unwrap();
    assert_eq!(
        fresh
            .store
            .commit_learning(
                &batch,
                &[Suggestion {
                    source: id,
                    quote: "I use Rust".into(),
                    claim: MemoryClaim::Stated,
                    sensitive: false
                }],
                &route
            )
            .unwrap(),
        0
    );
}

struct FakeHelper {
    calls: std::sync::atomic::AtomicUsize,
    change: Option<MemoryOwner>,
    wait: bool,
    entered: tokio::sync::Notify,
}
impl ConversationalProvider for FakeHelper {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [crate::message::Message],
        tools: &'a [&'a crate::tool::ToolDefinition],
        _: crate::identity::StepId,
        sink: &'a dyn crate::provider::DeltaSink,
    ) -> futures::future::BoxFuture<
        'a,
        std::result::Result<crate::message::Message, crate::provider::ProviderError>,
    > {
        Box::pin(async move {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.entered.notify_one();
            assert!(tools.is_empty());
            assert_eq!(messages.len(), 2);
            if self.wait {
                return std::future::pending().await;
            }
            if let Some(owner) = &self.change {
                owner
                    .controls(
                        MemoryScope::User,
                        MemoryControlEdit {
                            no_memory: Some(true),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            let crate::message::ContentBlock::Text(json) = &messages[1].content[0] else {
                panic!("source data")
            };
            let sources: Vec<LearningSource> = serde_json::from_str(json).unwrap();
            let suggestions = sources
                .into_iter()
                .map(|source| Suggestion {
                    source: source.id,
                    quote: source.text,
                    claim: MemoryClaim::Stated,
                    sensitive: false,
                })
                .collect::<Vec<_>>();
            sink.usage(crate::provider::ProviderUsage {
                total_tokens: Some(100),
                ..Default::default()
            });
            Ok(crate::message::Message::text(
                crate::message::Role::Assistant,
                serde_json::to_string(&suggestions).unwrap(),
            ))
        })
    }
}
fn helper(change: Option<MemoryOwner>, wait: bool) -> Arc<FakeHelper> {
    Arc::new(FakeHelper {
        calls: std::sync::atomic::AtomicUsize::new(0),
        change,
        wait,
        entered: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn real_learning_path_batches_one_call_and_accounts_actual_usage() {
    let (_home, owner, route) = fixture();
    let provider = helper(None, false);
    let worker = LearningWorker {
        store: owner.store.clone(),
        route,
        provider: provider.clone(),
        validate_route: Arc::new(|_| Ok(())),
    };
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    assert_eq!(
        worker
            .process(false, &tokio_util::sync::CancellationToken::new())
            .await
            .unwrap(),
        0
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(
        worker
            .process(true, &tokio_util::sync::CancellationToken::new())
            .await
            .unwrap(),
        1
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(owner.eligible().unwrap().records.len(), 1);
    let candidates = owner.candidate_page(None, None).unwrap();
    assert_eq!(candidates.records.len(), 1);
    assert_eq!(
        candidates.records[0].state,
        crate::memory::candidates::CandidateState::AutoApplied
    );
    assert!(
        owner
            .candidate(candidates.records[0].id)
            .unwrap()
            .stale_reason
            .is_none()
    );
    let receipts = owner.store.usage_page(None, None, None).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].receipt.as_ref().unwrap().total_tokens,
        Some(100)
    );
}

#[tokio::test]
async fn budget_denial_has_no_provider_call_and_inflight_controls_prevent_commit() {
    let (_home, owner, route) = fixture();
    let provider = helper(Some(owner.clone()), false);
    let worker = LearningWorker {
        store: owner.store.clone(),
        route,
        provider: provider.clone(),
        validate_route: Arc::new(|_| Ok(())),
    };
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    owner
        .store
        .update_usage_policy(|policy| policy.background_daily_tokens = 1)
        .unwrap();
    assert!(
        worker
            .process(true, &tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(owner.store.learning_status().unwrap().pending, 1);
    owner
        .store
        .update_usage_policy(|policy| policy.background_daily_tokens = 32768)
        .unwrap();
    assert!(
        worker
            .process(true, &tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(owner.eligible().unwrap().records.is_empty());
}

#[tokio::test]
async fn foreground_preempts_learning_and_settles_uncertain_usage_before_lane_release() {
    let (_home, owner, route) = fixture();
    owner
        .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
        .unwrap();
    let provider = helper(None, true);
    let worker = LearningWorker {
        store: owner.store.clone(),
        route,
        provider: provider.clone(),
        validate_route: Arc::new(|_| Ok(())),
    };
    let task = tokio::spawn(async move {
        worker
            .process(true, &tokio_util::sync::CancellationToken::new())
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.entered.notified(),
    )
    .await
    .unwrap();
    let foreground = owner.store.foreground_lease().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(owner.store.learning_status().unwrap().pending, 1);
    assert_eq!(
        owner.store.usage_page(None, None, None).unwrap()[0]
            .receipt
            .as_ref()
            .unwrap()
            .outcome,
        crate::usage_budget::Outcome::Interrupted
    );
    drop(foreground);
    assert!(owner.store.background_lease().unwrap().is_some());
}

#[tokio::test]
async fn live_route_revocation_blocks_dispatch_and_rechecks_before_commit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for allowed_checks in [0, 1] {
        let (_home, owner, route) = fixture();
        owner
            .enqueue_user_statement(Uuid::new_v4(), "I use Rust")
            .unwrap();
        let provider = helper(None, false);
        let checks = Arc::new(AtomicUsize::new(0));
        let validate_route = {
            let checks = checks.clone();
            Arc::new(move |_: &LearningRoute| {
                ensure!(
                    checks.fetch_add(1, Ordering::Relaxed) < allowed_checks,
                    "synthetic live route revoked"
                );
                Ok(())
            })
        };
        let worker = LearningWorker {
            store: owner.store.clone(),
            route,
            provider: provider.clone(),
            validate_route,
        };
        assert!(
            worker
                .process(true, &tokio_util::sync::CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), allowed_checks);
        assert_eq!(owner.store.learning_status().unwrap().pending, 1);
        assert!(owner.eligible().unwrap().records.is_empty());
        assert_eq!(
            owner.store.usage_page(None, None, None).unwrap().len(),
            allowed_checks
        );
    }
}
