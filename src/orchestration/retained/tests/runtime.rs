//! Real encrypted journal/supervisor controls; only provider execution is fake.
use super::*;
use crate::{
    app::worker_commands,
    config::XanaConfig,
    message::{Message, Role},
    orchestration::{
        AgentHandleSnapshot, ChildExecution, ChildExecutionContext, ChildExecutionFactory,
        ChildExecutionOutcome, ChildExecutionOutput, ChildLifecycle, ChildUsage, PreparedChild,
        ResolvedAgentConfig,
    },
    paths::XanaPaths,
    session::{DurableSession, SessionRecord},
};
use futures::future::BoxFuture;
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;
mod accounting;
mod authority;
mod settlement;
mod tool_guard;

struct Fixture {
    _home: tempfile::TempDir,
    paths: XanaPaths,
    store: ProtectedStore,
    worker: RetainedWorker,
    resolved: ResolvedAgentConfig,
    calls: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(home.path().into())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(
            paths.config_file(),
            r#"version = 3
default_profile = "worker"
default_child_route = "worker"
permission_mode = "deny"
[providers.local]
kind = "openai_compat"
base_url = "http://localhost:1/v1"
[providers.local.models.fixture]
tools = true
[profiles.worker]
connection = "local"
model = "fixture"
max_tool_rounds = 1
capabilities = []
egress_policy = "retained_fixture"
[egress_policies.retained_fixture]
allowed = ["prompt_text", "workspace_metadata", "xana_summary", "selected_artifacts"]
[routes.worker]
profile = "worker"
"#,
        )
        .unwrap();
        let workspace = home.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let store = ProtectedStore::initialize(
            &home.path().join("protected-fixture"),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let mut worker = super::worker();
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        let mut session = DurableSession::create_protected(
            store.clone(),
            workspace.canonicalize().unwrap(),
            worker.session,
        )
        .unwrap();
        let profile = crate::profile::ProfileStore::open(&paths)
            .resolve_global("worker")
            .unwrap();
        crate::profile::ProfileStore::open(&paths)
            .freeze(&worker.session.to_string(), &profile)
            .unwrap();
        let registry = XanaConfig::load_registry_from(paths.config_file()).unwrap();
        let models = crate::model_catalog::ModelManager::new(
            registry.clone(),
            home.path().join("cache"),
            home.path().join("selection.toml"),
        );
        let resolved = crate::orchestration::RouteResolver::new(&registry, &models)
            .resolve(Some("worker"))
            .unwrap();
        worker.admission.attribution.thread_id = session.thread_id();
        worker.admission.attribution.profile = resolved.profile.clone();
        worker.admission.limits = resolved.orchestration.clone();
        worker.scope = format!("conversation:{}", worker.session);
        worker.workspace_identity =
            crate::workspace_identity::WorkspaceIdentity::resolve(&workspace)
                .unwrap()
                .collision_key()
                .into();
        worker.configuration_digest =
            execution::configuration_digest(&paths, worker.session).unwrap();
        let input = session
            .append_message(Message::text(Role::User, "Initial worker task"))
            .unwrap();
        session
            .append_record(SessionRecord::OperationAccepted {
                operation_id: worker.admission.attribution.parent_operation_id,
                thread_id: session.thread_id(),
                input_entry_id: input,
            })
            .unwrap();
        session
            .append_record(SessionRecord::ChildAdmitted {
                handle: AgentHandleSnapshot::admitted(worker.admission.clone()),
            })
            .unwrap();
        session
            .append_record(SessionRecord::ChildLifecycleChanged {
                agent_id: worker.id,
                lifecycle: ChildLifecycle::Queued,
            })
            .unwrap();
        session
            .append_record(SessionRecord::ChildLifecycleChanged {
                agent_id: worker.id,
                lifecycle: ChildLifecycle::Running,
            })
            .unwrap();
        session
            .append_record(SessionRecord::ChildReportCommitted {
                report: crate::orchestration::ChildReport::completed(
                    worker.admission.attribution.clone(),
                    "original result".into(),
                    ChildUsage::Unknown,
                ),
            })
            .unwrap();
        session
            .append_record(SessionRecord::OperationFinished {
                operation_id: worker.admission.attribution.parent_operation_id,
                outcome: crate::native_runtime::OperationOutcome::Completed,
            })
            .unwrap();
        store.retained_create(worker.clone()).unwrap();
        drop(session);
        Self {
            _home: home,
            paths,
            store,
            worker,
            resolved,
            calls: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(tokio::sync::Notify::new()),
        }
    }
    fn command(&self, value: serde_json::Value) -> Result<serde_json::Value> {
        worker_commands::control(
            &self.paths,
            &self.store,
            serde_json::from_value(value)?,
            &CancellationToken::new(),
        )
    }
    fn queue(&mut self) -> Uuid {
        let id = Uuid::new_v4();
        self.command(serde_json::json!({"command":"follow_up","target":{"id":self.worker.id,"revision":self.worker.revision},"request_id":id,"text":"Continue using only this exact bounded task"})).unwrap();
        self.worker = self.store.retained_worker(self.worker.id).unwrap();
        id
    }
    fn factory(&self, wait: bool) -> Arc<dyn ChildExecutionFactory> {
        Arc::new(FakeFactory {
            resolved: self.resolved.clone(),
            calls: self.calls.clone(),
            started: self.started.clone(),
            workspace: self
                .store
                .history_metadata(self.worker.session)
                .unwrap()
                .workspace,
            wait,
            fail: false,
        })
    }
}
struct FakeFactory {
    resolved: ResolvedAgentConfig,
    calls: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    workspace: std::path::PathBuf,
    wait: bool,
    fail: bool,
}
impl ChildExecutionFactory for FakeFactory {
    fn prepare(
        &self,
        request: &crate::orchestration::SpawnAgentRequest,
    ) -> std::result::Result<PreparedChild, String> {
        assert!(request.task.contains("New bounded execution"));
        Ok(PreparedChild::new(
            self.resolved.clone(),
            crate::permission::PermissionPolicy::new(
                crate::permission::PolicyDecision::Deny,
                Vec::new(),
                &self.workspace,
            )
            .unwrap(),
            Box::new(FakeExecution {
                calls: self.calls.clone(),
                started: self.started.clone(),
                wait: self.wait,
                fail: self.fail,
            }),
        ))
    }
}
struct FakeExecution {
    calls: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    wait: bool,
    fail: bool,
}
impl ChildExecution for FakeExecution {
    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if self.fail {
                ChildExecutionOutcome::Failed(
                    "managed provider handle is unavailable; no session continuation".into(),
                )
            } else if self.wait {
                context.cancellation.cancelled().await;
                ChildExecutionOutcome::Cancelled("fixture cancelled".into())
            } else {
                ChildExecutionOutcome::Completed(ChildExecutionOutput {
                    evidence: None,
                    text: "bounded continuation result".into(),
                    usage: ChildUsage::Unknown,
                })
            }
        })
    }
}

#[tokio::test]
async fn completed_worker_reopens_with_lineage_budget_and_duplicate_mailbox_intact() {
    let mut f = Fixture::new();
    let message = f.queue();
    assert!(f.command(serde_json::json!({"command":"follow_up","target":{"id":f.worker.id,"revision":1},"request_id":message,"text":"Continue using only this exact bounded task"})).unwrap()["duplicate"].as_bool().unwrap());
    let factory = f.factory(false);
    f.worker = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(factory),
    )
    .await
    .unwrap();
    assert_eq!(f.worker.state, WorkerState::Idle);
    assert_eq!(f.worker.executions, 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.worker.last_receipt.as_ref().unwrap().follow_up, message);
    let (summary, _) = DurableSession::inspect_protected(&f.store, f.worker.session).unwrap();
    assert_eq!(summary.children.len(), 2);
    for _ in 0..6 {
        f.queue();
        let factory = f.factory(false);
        f.worker = execution::run_with_factory(
            f.paths.clone(),
            f.store.clone(),
            f.worker.clone(),
            CancellationToken::new(),
            Some(factory),
        )
        .await
        .unwrap();
        assert_eq!(f.worker.state, WorkerState::Idle);
    }
    f.queue();
    let factory = f.factory(false);
    f.worker = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(factory),
    )
    .await
    .unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 7);
    assert_eq!(f.worker.state, WorkerState::NeedsReview);
    assert!(
        f.worker
            .last_receipt
            .unwrap()
            .summary
            .contains("descendant")
    );
}

#[tokio::test]
async fn stop_revokes_active_worker_and_scope_changes_refuse_dispatch() {
    let mut f = Fixture::new();
    f.queue();
    let factory = f.factory(true);
    let pending = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(factory),
    );
    tokio::pin!(pending);
    tokio::select! {
        started = tokio::time::timeout(std::time::Duration::from_secs(5), f.started.notified()) => started.expect("worker should start"),
        result = &mut pending => panic!("unexpected completion {result:?}"),
    }
    let current = f.store.retained_worker(f.worker.id).unwrap();
    assert!(
        execution::run_with_factory(
            f.paths.clone(),
            f.store.clone(),
            current.clone(),
            CancellationToken::new(),
            Some(f.factory(false))
        )
        .await
        .is_err()
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.command(serde_json::json!({"command":"stop","target":{"id":current.id,"revision":current.revision}})).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.state, WorkerState::Stopped);
    assert!(result.active.is_none());
    let mut changed = Fixture::new();
    changed.queue();
    fs::write(changed.paths.config_file(), "invalid replaced config").unwrap();
    assert!(
        execution::run_with_factory(
            changed.paths.clone(),
            changed.store.clone(),
            changed.worker.clone(),
            CancellationToken::new(),
            Some(changed.factory(false))
        )
        .await
        .is_err()
    );
    assert_eq!(changed.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn lost_managed_handle_requires_review_and_never_replays_the_mailbox() {
    let mut f = Fixture::new();
    f.worker = f
        .store
        .retained_update(f.worker.id, f.worker.revision, |worker| {
            worker.admission.attribution.owner = ExecutionOwner::Codex;
            Ok(())
        })
        .unwrap()
        .0;
    f.resolved.owner = ExecutionOwner::Codex;
    f.queue();
    let factory = Arc::new(FakeFactory {
        resolved: f.resolved.clone(),
        calls: f.calls.clone(),
        started: f.started.clone(),
        workspace: f
            .store
            .history_metadata(f.worker.session)
            .unwrap()
            .workspace,
        wait: false,
        fail: true,
    });
    f.worker = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(factory),
    )
    .await
    .unwrap();
    assert_eq!(f.worker.state, WorkerState::NeedsReview);
    assert!(f.worker.mailbox.is_empty());
    assert!(f.command(serde_json::json!({"command":"recover","target":{"id":f.worker.id,"revision":f.worker.revision},"review_unknown":false})).is_err());
    f.command(serde_json::json!({"command":"recover","target":{"id":f.worker.id,"revision":f.worker.revision},"review_unknown":true})).unwrap();
    let recovered = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(recovered.state, WorkerState::Idle);
    assert!(recovered.mailbox.is_empty());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn context_owner_controls_charge_work_and_reject_scope_and_byte_budget_races() {
    let mut f = Fixture::new();
    let artifacts = crate::artifact::ArtifactStore::protected(f.store.clone());
    let (artifact, _) = artifacts
        .put(
            b"first\nmatch 42\nlast\n",
            "text/plain",
            crate::identity::PrincipalId::new(),
        )
        .unwrap();
    f.worker = f
        .store
        .retained_update(f.worker.id, f.worker.revision, |worker| {
            worker.evidence.push(artifact.clone());
            Ok(())
        })
        .unwrap()
        .0;
    let operation = serde_json::json!({"operation":"search","inputs":[{"artifact":artifact.reference,"offset":0,"length":artifact.byte_len}],"query":"42"});
    let result=f.command(serde_json::json!({"command":"context","target":{"id":f.worker.id,"revision":f.worker.revision},"operation":operation.to_string()})).unwrap();
    assert_eq!(result["state"], "completed");
    assert_eq!(result["preview"], "match 42\n");
    assert_eq!(result["model_calls"], 0);
    f.worker = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(f.worker.context_bytes, artifact.byte_len);
    assert_eq!(f.worker.evidence.len(), 2);
    let mut alien = artifact.reference;
    alien.id = crate::identity::ArtifactId::new();
    let invalid =
        serde_json::json!({"operation":"slice","input":{"artifact":alien,"offset":0,"length":1}});
    assert!(f.command(serde_json::json!({"command":"context","target":{"id":f.worker.id,"revision":f.worker.revision},"operation":invalid.to_string()})).is_err());
    let revision = f.worker.revision;
    f.store
        .retained_update(f.worker.id, revision, |worker| {
            worker.expires_at = 0;
            Ok(())
        })
        .unwrap();
    assert!(f.command(serde_json::json!({"command":"context","target":{"id":f.worker.id,"revision":revision+1},"operation":operation.to_string()})).is_err());
    assert_eq!(
        f.store.retained_worker(f.worker.id).unwrap().context_bytes,
        artifact.byte_len
    );
}

#[test]
fn unknown_context_reservation_requires_review_without_refunding_work() {
    use crate::orchestration::context_ops::{ContextWorkReceipt, ContextWorkState};
    let mut f = Fixture::new();
    let (artifact, _) = crate::artifact::ArtifactStore::protected(f.store.clone())
        .put(
            b"answer 42",
            "text/plain",
            crate::identity::PrincipalId::new(),
        )
        .unwrap();
    f.worker = f
        .store
        .retained_update(f.worker.id, f.worker.revision, |worker| {
            worker.evidence.push(artifact.clone());
            worker.context_bytes = artifact.byte_len;
            worker.context_operations = 1;
            worker.context_receipt = Some(ContextWorkReceipt {
                completion: None,
                id: Uuid::new_v4(),
                state: ContextWorkState::Reserved,
                verified_bytes: artifact.byte_len,
                selected_bytes: 9,
                input_count: 1,
                model_calls: 0,
                result: None,
                preview: String::new(),
                error: None,
            });
            Ok(())
        })
        .unwrap()
        .0;
    let operation=serde_json::json!({"operation":"slice","input":{"artifact":artifact.reference,"offset":0,"length":9}}).to_string();
    assert!(f.command(serde_json::json!({"command":"context","target":{"id":f.worker.id,"revision":f.worker.revision},"operation":operation})).is_err());
    assert_eq!(
        f.store
            .retained_worker(f.worker.id)
            .unwrap()
            .context_operations,
        1
    );
    f.command(serde_json::json!({"command":"recover","target":{"id":f.worker.id,"revision":f.worker.revision},"review_unknown":true})).unwrap();
    f.worker = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(
        f.worker.context_receipt.as_ref().unwrap().state,
        ContextWorkState::Interrupted
    );
    f.command(serde_json::json!({"command":"context","target":{"id":f.worker.id,"revision":f.worker.revision},"operation":operation})).unwrap();
    let finished = f.store.retained_worker(f.worker.id).unwrap();
    assert_eq!(finished.context_operations, 2);
    assert_eq!(finished.context_bytes, artifact.byte_len * 2);
}

#[tokio::test]
async fn expiry_during_execution_cancels_and_preserves_expired_state() {
    let mut f = Fixture::new();
    f.queue();
    let pending = execution::run_with_factory(
        f.paths.clone(),
        f.store.clone(),
        f.worker.clone(),
        CancellationToken::new(),
        Some(f.factory(true)),
    );
    tokio::pin!(pending);
    tokio::select! {
        started = tokio::time::timeout(std::time::Duration::from_secs(5), f.started.notified()) => started.expect("worker should start"),
        result = &mut pending => panic!("unexpected completion {result:?}"),
    }
    let current = f.store.retained_worker(f.worker.id).unwrap();
    f.store
        .retained_update(current.id, current.revision, |worker| {
            worker.expires_at = 0;
            Ok(())
        })
        .unwrap();
    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, WorkerState::Expired);
    assert!(finished.active.is_none());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
