//! Isolated SQLCipher, scheduler and native-runtime fixtures. No credential
//! lookup, real home, network provider or detached OS process is used here.
use super::*;
use crate::{
    agent::Agent,
    context::ContextBudget,
    identity::StepId,
    message::{ContentBlock, Message, Role, ToolCall},
    permission::{PermissionPolicy, PolicyDecision},
    prompt::{PromptAssembler, PromptEnvironment, PromptSurface},
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    tool::{ToolDefinition, ToolRegistry},
    usage_budget::UsageBudget,
    workspace_identity::WorkspaceIdentity,
};
use futures::future::BoxFuture;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(super) fn fixture() -> (tempfile::TempDir, ProtectedStore, TestCustody, Job) {
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let custody = TestCustody::default();
    let store = ProtectedStore::initialize(
        &home.path().join("data"),
        &RecoveryIdentity::generate(),
        &custody,
    )
    .unwrap();
    let identity = WorkspaceIdentity::resolve(&workspace).unwrap();
    let at = 1_800_000_000;
    let job = Job {
        trigger: None,
        id: Uuid::new_v4(),
        revision: 1,
        conversation: Uuid::new_v4(),
        name: "synthetic task".into(),
        scope: TaskScope {
            workspace: identity.canonical_path().into(),
            workspace_identity: identity.collision_key().into(),
            project: None,
            profile: "fixture".into(),
            profile_id: Uuid::new_v4(),
            configuration_digest: "fixture-policy".into(),
            connection: "fake".into(),
            model: "fake".into(),
            endpoint: "https://provider.invalid".into(),
        },
        action: Action::Reminder {
            text: "local receipt".into(),
        },
        budget: TaskBudget::default(),
        schedule: Schedule::Once { at },
        expires_at: at + 86400,
        authorized: true,
        state: JobState::Ready,
        next: Occurrence {
            at,
            local_date: String::new(),
            dst_adjusted: false,
        },
        not_before: at,
        occurrence: None,
        pause_after_run: false,
        last_receipt: None,
    };
    (home, store, custody, job)
}

fn terminal(job: &Job, outcome: RunOutcome, at: i64) -> RunReceipt {
    RunReceipt {
        completion: None,
        occurrence: job.occurrence.unwrap(),
        scheduled_at: job.next.at,
        finished_at: at,
        outcome,
        detail: "synthetic receipt".into(),
        coalesced: at > job.next.at,
        dst_adjusted: job.next.dst_adjusted,
    }
}

#[test]
fn durable_revision_edits_recovery_and_unknown_review_do_not_replay() {
    let (home, store, custody, job) = fixture();
    let at = job.next.at;
    store.autonomy_create(job.clone()).unwrap();
    let paused = store.autonomy_edit(job.id, 1, JobEdit::Pause, at).unwrap();
    assert!(store.autonomy_edit(job.id, 1, JobEdit::Cancel, at).is_err());
    store
        .autonomy_edit(
            job.id,
            paused.revision,
            JobEdit::Resume {
                review_unknown: false,
            },
            at,
        )
        .unwrap();
    let claimed = store.autonomy_claim(at).unwrap().unwrap();
    assert!(store.autonomy_claim(at).unwrap().is_none());
    drop(store);
    let store = ProtectedStore::open(&home.path().join("data"), &custody).unwrap();
    store.autonomy_recover(at + 1).unwrap();
    let uncertain = store.autonomy_job(job.id).unwrap();
    assert_eq!(uncertain.state, JobState::NeedsYou);
    assert_eq!(
        uncertain.last_receipt.as_ref().unwrap().outcome,
        RunOutcome::Unknown
    );
    assert!(store.autonomy_claim(at + 2).unwrap().is_none());
    assert!(
        store
            .autonomy_edit(
                job.id,
                uncertain.revision,
                JobEdit::Resume {
                    review_unknown: false
                },
                at + 2
            )
            .is_err()
    );
    store
        .autonomy_edit(
            job.id,
            uncertain.revision,
            JobEdit::Resume {
                review_unknown: true,
            },
            at + 2,
        )
        .unwrap();
    let reviewed = store.autonomy_claim(at + 2).unwrap().unwrap();
    assert_ne!(reviewed.occurrence, claimed.occurrence);
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 1);
}

#[test]
fn missed_daily_occurrences_coalesce_and_cancellation_revokes_authority() {
    let (_home, store, _custody, mut job) = fixture();
    job.schedule = Schedule::Daily {
        timezone: "Etc/UTC".into(),
        hour: 8,
        minute: 0,
    };
    job.next = job.schedule.first(job.next.at).unwrap();
    job.not_before = job.next.at;
    job.expires_at = job.next.at + 10 * 86400;
    store.autonomy_create(job.clone()).unwrap();
    let wake = job.next.at + 5 * 86400 + 60;
    let claimed = store.autonomy_claim(wake).unwrap().unwrap();
    let completed = store
        .autonomy_finish(job.id, terminal(&claimed, RunOutcome::Completed, wake))
        .unwrap();
    assert!(completed.next.at > wake);
    assert!(store.autonomy_claim(wake).unwrap().is_none());
    let next = store.autonomy_claim(completed.next.at).unwrap().unwrap();
    let cancelled = store
        .autonomy_edit(job.id, next.revision, JobEdit::Cancel, completed.next.at)
        .unwrap();
    assert_eq!(cancelled.state, JobState::CancelRequested);
    let unknown = store
        .autonomy_finish(
            job.id,
            terminal(&next, RunOutcome::Unknown, completed.next.at),
        )
        .unwrap();
    assert!(!unknown.authorized);
    assert!(
        store
            .autonomy_edit(
                job.id,
                unknown.revision,
                JobEdit::Resume {
                    review_unknown: true
                },
                completed.next.at
            )
            .is_err()
    );
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 2);
}

#[test]
fn expiry_and_host_policy_edits_are_transactional() {
    let (_home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    assert!(store.autonomy_claim(job.expires_at).unwrap().is_none());
    assert_eq!(store.autonomy_job(job.id).unwrap().state, JobState::Expired);
    assert!(
        store
            .autonomy_edit_policy(0, |policy| policy.startup_enabled = true)
            .is_err()
    );
    assert_eq!(store.autonomy_policy().unwrap(), HostPolicy::default());
    let enabled = host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    assert!(host::policy_edit(&store, 0, None, None, true, false).is_err());
    let startup =
        host::policy_edit(&store, enabled.revision, None, Some(true), false, false).unwrap();
    assert!(startup.detached_enabled && startup.startup_enabled);
}

struct NativeFixture {
    store: ProtectedStore,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    permission: PolicyDecision,
    lose_response: bool,
    memory_probe: bool,
}
struct FakeProvider {
    replies: Mutex<VecDeque<Message>>,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}
impl ConversationalProvider for FakeProvider {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(messages.to_vec());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProviderError::new("fake exhausted"))
        })
    }
}
impl runner::TaskExecutor for NativeFixture {
    fn execute<'a>(
        &'a self,
        job: &'a Job,
        cancelled: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>> {
        Box::pin(async move {
            let names = ["read_file".to_owned()].into_iter().collect();
            let mut tools = ToolRegistry::builtins_from_names(
                crate::shell::Shell::resolve(crate::shell::ShellConfig::default())?,
                &names,
            )?;
            if self.memory_probe {
                // Even an accidentally exposed foreground tool must not gain
                // owner authority from a scheduled prompt or permissive policy.
                crate::memory::tools::register(
                    &mut tools,
                    Some(crate::memory::MemoryOwner::new(
                        self.store.clone(),
                        crate::memory::MemoryContext {
                            conversation: Some(job.conversation),
                            profile: Some(job.scope.profile_id),
                            project: job.scope.project,
                        },
                    )),
                )?;
            }
            let assembler = PromptAssembler::new(
                tools.definitions().into_iter().cloned().collect(),
                PromptEnvironment {
                    connection: "fake".into(),
                    model: "fake".into(),
                    operating_system: "synthetic".into(),
                    working_directory: job.scope.workspace.clone(),
                    configured_shell: "not exposed".into(),
                    surface: PromptSurface::Cli,
                },
                None,
                ContextBudget {
                    total_tokens: 6144,
                    conversation_reserve_tokens: 1024,
                },
            );
            let mut replies = VecDeque::from([Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(if self.memory_probe {
                    ToolCall {
                        id: "memory-1".into(),
                        name: "memory_update".into(),
                        arguments: serde_json::json!({
                            "action":"remember",
                            "scope":"conversation",
                            "statement":"I prefer Rust examples",
                            "quote":"remember that I prefer Rust examples",
                            "risk":"ordinary"
                        }),
                    }
                } else {
                    ToolCall {
                        id: "read-1".into(),
                        name: "read_file".into(),
                        arguments: serde_json::json!({"path":"note.txt"}),
                    }
                })],
            }]);
            if !self.lose_response {
                replies.push_back(Message::text(Role::Assistant, "bounded synthetic result"));
            }
            let provider = FakeProvider {
                requests: self.requests.clone(),
                replies: Mutex::new(replies),
            };
            let budget = UsageBudget::new(
                self.store.clone(),
                job.conversation.to_string(),
                "scheduled-fixture".into(),
                32,
            )
            .background(job.occurrence.unwrap().to_string());
            let agent = Agent::new(
                Box::new(provider),
                tools,
                job.scope.workspace.clone(),
                assembler.assemble(&[])?,
                4,
            )
            .with_usage_budget(Some(budget));
            let policy = PermissionPolicy::new(self.permission, vec![], &job.scope.workspace)?
                .workspace_reads_only();
            runner::run_native(
                &self.store,
                job,
                cancelled,
                (agent, assembler, policy),
                || Ok(true),
            )
            .await
        })
    }
}

#[tokio::test]
async fn scheduled_native_runtime_reads_only_pre_authorized_data_and_reopens_receipts() {
    for permission in [PolicyDecision::Ask, PolicyDecision::Allow] {
        let (home, store, custody, mut job) = fixture();
        std::fs::write(
            job.scope.workspace.join("note.txt"),
            "unique synthetic workspace content",
        )
        .unwrap();
        job.action = Action::NativeTask {
            prompt: "Summarize note.txt".into(),
            workspace_reads: true,
        };
        store.autonomy_create(job.clone()).unwrap();
        host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let executor = NativeFixture {
            store: store.clone(),
            requests: requests.clone(),
            permission,
            lose_response: false,
            memory_probe: false,
        };
        let finished = tokio::time::timeout(
            Duration::from_secs(10),
            runner::tick(
                &store,
                &executor,
                &|| Ok(job.next.at),
                &CancellationToken::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(
            finished.state,
            if permission == PolicyDecision::Allow {
                JobState::Completed
            } else {
                JobState::NeedsYou
            }
        );
        let captured = serde_json::to_string(&*requests.lock().unwrap()).unwrap();
        assert_eq!(
            captured.contains("unique synthetic workspace content"),
            permission == PolicyDecision::Allow
        );
        assert!(store.background_lease().unwrap().is_some());
        let session = job.conversation.to_string().parse().unwrap();
        assert!(store.history_exists(session).unwrap());
        assert!(!store.usage_page(None, None, None).unwrap().is_empty());
        drop(executor);
        drop(store);
        let reopened = ProtectedStore::open(&home.path().join("data"), &custody).unwrap();
        assert_eq!(reopened.autonomy_receipts(job.id, 0).unwrap().len(), 1);
        assert!(reopened.history_exists(session).unwrap());
        assert!(reopened.autonomy_claim(job.next.at + 1).unwrap().is_none());
    }
}

struct CancellableFixture {
    started: Arc<AtomicUsize>,
}

#[tokio::test]
async fn scheduled_false_claim_after_failed_read_finishes_needs_you_with_budget_evidence() {
    let (_home, store, _custody, mut job) = fixture();
    job.action = Action::NativeTask {
        prompt: "Read the missing note.txt and finish".into(),
        workspace_reads: true,
    };
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let executor = NativeFixture {
        store: store.clone(),
        requests: Arc::new(Mutex::new(vec![])),
        permission: PolicyDecision::Allow,
        lose_response: false,
        memory_probe: false,
    };
    let finished = tokio::time::timeout(
        Duration::from_secs(10),
        runner::tick(
            &store,
            &executor,
            &|| Ok(job.next.at),
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(finished.state, JobState::NeedsYou);
    let receipt = finished.last_receipt.unwrap();
    assert_eq!(receipt.outcome, RunOutcome::NeedsYou);
    let evidence = receipt.completion.unwrap();
    assert!(!evidence.supported());
    assert!(evidence.budget.remaining_requests.is_some());
    assert!(evidence.budget.remaining_tokens.is_some());
    assert_eq!(evidence.budget.remaining_cost_microusd, None);
    assert_eq!(evidence.budget.remaining_millis, None);
    assert!(store.background_lease().unwrap().is_some());
}
impl runner::TaskExecutor for CancellableFixture {
    fn execute<'a>(
        &'a self,
        _job: &'a Job,
        cancelled: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            cancelled.cancelled().await;
            Ok((
                RunOutcome::Unknown,
                "fake dispatch cancelled after start".into(),
            ))
        })
    }
}
#[tokio::test]
async fn foreground_preempts_without_releasing_live_lane() {
    let (_home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let executor = CancellableFixture {
        started: started.clone(),
    };
    let clock = AtomicI64::new(job.next.at);
    let read_clock = || Ok(clock.load(Ordering::SeqCst));
    let shutdown = CancellationToken::new();
    let execution = runner::tick(&store, &executor, &read_clock, &shutdown);
    tokio::pin!(execution);
    tokio::select! {result=&mut execution=>panic!("unexpected early completion: {result:?}"),_=tokio::time::sleep(Duration::from_millis(60))=>{}}
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(store.background_lease().unwrap().is_none());
    let foreground = store.foreground_lease().unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, JobState::NeedsYou);
    drop(foreground);
    assert!(store.background_lease().unwrap().is_some());
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn owned_host_survives_observer_detach_and_refuses_a_competing_owner() {
    let (home, store, custody, mut job) = fixture();
    std::fs::write(job.scope.workspace.join("note.txt"), "synthetic host input").unwrap();
    job.action = Action::NativeTask {
        prompt: "Summarize note.txt".into(),
        workspace_reads: true,
    };
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
    let clock = AtomicI64::new(job.next.at - 1);
    let read_clock = || Ok(clock.load(Ordering::SeqCst));
    let executor = NativeFixture {
        store: store.clone(),
        requests: Arc::new(Mutex::new(vec![])),
        permission: PolicyDecision::Allow,
        lose_response: false,
        memory_probe: false,
    };
    let clients = async {
        let observer = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir())
                    .await
                {
                    Ok(observer) => break observer,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
        .await
        .unwrap();
        assert!(!observer.is_controller());
        assert_eq!(observer.snapshot().scheduled_jobs.len(), 1);
        let host_id = observer.snapshot().host_id;
        assert!(
            host::run_owned(&paths, &store, &executor, &read_clock, None)
                .await
                .is_err()
        );
        drop(observer); // No observer owns cancellation or execution authority.
        clock.store(job.next.at, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if store.autonomy_job(job.id).unwrap().state == JobState::Completed {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let observer = crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir())
            .await
            .unwrap();
        assert_eq!(observer.snapshot().host_id, host_id);
        assert_eq!(
            observer.snapshot().scheduled_jobs[0].state,
            JobState::Completed
        );
        assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 1);
        drop(observer);
        let policy = store.autonomy_policy().unwrap();
        host::policy_edit(&store, policy.revision, None, None, true, false).unwrap();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            host::run_owned(&paths, &store, &executor, &read_clock, None),
            clients
        )
    })
    .await
    .unwrap();
    served.unwrap();
    drop(executor);
    drop(store);
    let reopened = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
    assert_eq!(reopened.autonomy_receipts(job.id, 0).unwrap().len(), 1);
    assert!(reopened.autonomy_claim(job.next.at + 1).unwrap().is_none());
}

#[test]
fn schedule_pages_are_cursor_bounded() {
    let (_home, store, _custody, job) = fixture();
    for index in 0..PAGE_SIZE + 2 {
        let mut next = job.clone();
        next.id = Uuid::new_v4();
        next.conversation = Uuid::new_v4();
        next.name = format!("synthetic {index}");
        store.autonomy_create(next).unwrap();
    }
    let first = store.autonomy_page(0).unwrap();
    assert_eq!(first.len(), PAGE_SIZE);
    let second = store.autonomy_page(first.last().unwrap().0).unwrap();
    assert_eq!(second.len(), 2);
    assert!(
        first
            .iter()
            .all(|(_, one)| second.iter().all(|(_, two)| one.id != two.id))
    );
}

#[tokio::test]
async fn lost_native_response_keeps_uncertain_usage_and_never_replays() {
    let (home, store, custody, mut job) = fixture();
    std::fs::write(
        job.scope.workspace.join("note.txt"),
        "synthetic read already happened",
    )
    .unwrap();
    job.action = Action::NativeTask {
        prompt: "Read note.txt".into(),
        workspace_reads: true,
    };
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let executor = NativeFixture {
        store: store.clone(),
        requests: requests.clone(),
        permission: PolicyDecision::Allow,
        lose_response: true,
        memory_probe: false,
    };
    let finished = tokio::time::timeout(
        Duration::from_secs(10),
        runner::tick(
            &store,
            &executor,
            &|| Ok(job.next.at),
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(finished.last_receipt.unwrap().outcome, RunOutcome::Unknown);
    assert!(
        serde_json::to_string(&*requests.lock().unwrap())
            .unwrap()
            .contains("synthetic read already happened")
    );
    assert!(
        store
            .usage_page(None, None, None)
            .unwrap()
            .iter()
            .all(|record| record.charged_tokens > 0)
    );
    drop(executor);
    drop(store);
    let reopened = ProtectedStore::open(&home.path().join("data"), &custody).unwrap();
    reopened.autonomy_recover(job.next.at + 1).unwrap();
    assert!(reopened.autonomy_claim(job.next.at + 1).unwrap().is_none());
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn foreground_workspace_handoff_waits_for_the_background_terminal_commit() {
    let (_home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let executor = CancellableFixture {
        started: started.clone(),
    };
    let foreground = async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let host = crate::workspace_host::WorkspaceHost::open_protected(
            store.clone(),
            &job.scope.workspace,
        )
        .unwrap();
        let lease = host
            .acquire_foreground_root(crate::workspace_host::ConversationRef::NewNative)
            .await
            .unwrap();
        assert_eq!(
            store.autonomy_job(job.id).unwrap().state,
            JobState::NeedsYou
        );
        assert!(store.background_lease().unwrap().is_none());
        drop(lease);
    };
    let cancel = CancellationToken::new();
    let clock = || Ok(job.next.at);
    let (background, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(runner::tick(&store, &executor, &clock, &cancel), foreground)
    })
    .await
    .unwrap();
    assert_eq!(background.unwrap().unwrap().state, JobState::NeedsYou);
    assert!(store.background_lease().unwrap().is_some());
}

#[tokio::test]
async fn key_revocation_during_a_run_recovers_unknown_without_replay() {
    let (home, store, custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let started = Arc::new(AtomicUsize::new(0));
    let executor = CancellableFixture {
        started: started.clone(),
    };
    let revoke = async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        store.lock().unwrap();
    };
    let cancel = CancellationToken::new();
    let clock = || Ok(job.next.at);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(runner::tick(&store, &executor, &clock, &cancel), revoke)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    drop(store);
    let reopened = ProtectedStore::unlock(&home.path().join("data"), &custody).unwrap();
    reopened.autonomy_recover(job.next.at + 1).unwrap();
    assert_eq!(
        reopened
            .autonomy_job(job.id)
            .unwrap()
            .last_receipt
            .unwrap()
            .outcome,
        RunOutcome::Unknown
    );
    assert!(reopened.autonomy_claim(job.next.at + 1).unwrap().is_none());
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn background_native_prompt_cannot_become_an_owner_memory_control() {
    let (_home, store, _custody, mut job) = fixture();
    job.action = Action::NativeTask {
        prompt: "remember that I prefer Rust examples".into(),
        workspace_reads: false,
    };
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let executor = NativeFixture {
        store: store.clone(),
        requests: requests.clone(),
        permission: PolicyDecision::Allow,
        lose_response: false,
        memory_probe: true,
    };
    let finished = tokio::time::timeout(
        Duration::from_secs(3),
        runner::tick(
            &store,
            &executor,
            &|| Ok(job.next.at),
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(finished.state, JobState::NeedsYou);
    let captured = requests.lock().unwrap();
    assert_eq!(
        captured.len(),
        2,
        "ordinary prose reaches the normal tool loop"
    );
    let rejection = captured[1]
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult(result) if result.call_id == "memory-1" => Some(result),
            _ => None,
        })
        .expect("the attempted memory write returns observed rejection evidence");
    assert_eq!(rejection.status, crate::message::ToolResultStatus::Error);
    assert!(
        rejection
            .output
            .contains("current foreground owner's input")
    );
    let receipt = finished.last_receipt.unwrap();
    assert_eq!(receipt.outcome, RunOutcome::NeedsYou);
    assert!(!receipt.completion.unwrap().supported());
    assert!(store.memory_page(None, None).unwrap().records.is_empty());
    assert!(store.learning_batch().unwrap().is_empty());
    assert!(!store.usage_page(None, None, None).unwrap().is_empty());
}

struct DrainingFixture {
    started: AtomicUsize,
    cancel_seen: AtomicUsize,
    release: tokio::sync::Notify,
}
impl runner::TaskExecutor for DrainingFixture {
    fn execute<'a>(
        &'a self,
        _job: &'a Job,
        cancelled: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>> {
        Box::pin(async move {
            self.started.store(1, Ordering::SeqCst);
            cancelled.cancelled().await;
            self.cancel_seen.store(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok((
                RunOutcome::Unknown,
                "synthetic worker joined after stop".into(),
            ))
        })
    }
}
#[tokio::test]
async fn host_stop_retains_discovery_until_the_owned_worker_finishes() {
    let (home, store, _custody, job) = fixture();
    store.autonomy_create(job.clone()).unwrap();
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
    let executor = DrainingFixture {
        started: AtomicUsize::new(0),
        cancel_seen: AtomicUsize::new(0),
        release: tokio::sync::Notify::new(),
    };
    let clock = || Ok(job.next.at);
    let owner = async {
        while executor.started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        host::policy_edit(&store, 1, None, None, true, false).unwrap();
        while executor.cancel_seen.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let observer = crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir())
            .await
            .unwrap();
        assert!(
            host::run_owned(&paths, &store, &executor, &clock, None)
                .await
                .is_err()
        );
        assert_eq!(store.autonomy_job(job.id).unwrap().state, JobState::Running);
        assert!(store.background_lease().unwrap().is_none());
        drop(observer);
        executor.release.notify_one();
    };
    let (served, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            host::run_owned(&paths, &store, &executor, &clock, None),
            owner
        )
    })
    .await
    .unwrap();
    served.unwrap();
    assert_eq!(
        store.autonomy_job(job.id).unwrap().state,
        JobState::NeedsYou
    );
    assert!(store.background_lease().unwrap().is_some());
}

#[tokio::test]
async fn host_stop_receipt_names_active_job_and_attached_observers() {
    let (home, store, _custody, job) = fixture();
    for _ in 0..PAGE_SIZE {
        let mut future = job.clone();
        future.id = Uuid::new_v4();
        future.conversation = Uuid::new_v4();
        future.schedule = Schedule::Once {
            at: job.next.at + 3600,
        };
        future.next.at += 3600;
        future.not_before += 3600;
        store.autonomy_create(future).unwrap();
    }
    store.autonomy_create(job.clone()).unwrap();
    assert!(
        !host::summaries(&store)
            .unwrap()
            .iter()
            .any(|row| row.id == job.id)
    );
    host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let paths = crate::paths::XanaPaths::resolve(Some(home.path().as_os_str().to_owned())).unwrap();
    let executor = DrainingFixture {
        started: AtomicUsize::new(0),
        cancel_seen: AtomicUsize::new(0),
        release: tokio::sync::Notify::new(),
    };
    let clock = || Ok(job.next.at);
    let owner = async {
        while executor.started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let first = crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir())
            .await
            .unwrap();
        let second = crate::local_host::connect_observer(paths.runtime_dir(), paths.data_dir())
            .await
            .unwrap();
        host::policy_edit(&store, 1, None, None, true, false).unwrap();
        let impact = store.autonomy_stop_impact().unwrap().unwrap();
        assert_eq!(impact.policy_revision, 2);
        let active = impact.active_job.unwrap();
        assert_eq!(active.id, job.id);
        assert_eq!(active.conversation, job.conversation);
        assert_eq!(active.name, job.name);
        loop {
            let impact = store.autonomy_stop_impact().unwrap().unwrap();
            if let Some(clients) = impact.attached_clients {
                assert_eq!(clients.host_id, first.snapshot().host_id);
                assert_eq!(clients.host_generation, first.snapshot().host_generation);
                assert_eq!(clients.count, 2);
                assert_eq!(clients.identities.len(), 2);
                assert_ne!(clients.identities[0], clients.identities[1]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(store.background_lease().unwrap().is_none());
        executor.release.notify_one();
        (first, second)
    };
    let (served, observers) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            host::run_owned(&paths, &store, &executor, &clock, None),
            owner
        )
    })
    .await
    .unwrap();
    served.unwrap();
    drop(observers);
    let impact = store.autonomy_stop_impact().unwrap().unwrap();
    assert_eq!(impact.active_job.unwrap().id, job.id);
    assert_eq!(impact.attached_clients.unwrap().count, 2);
    assert_eq!(
        store.autonomy_job(job.id).unwrap().state,
        JobState::NeedsYou
    );
}
