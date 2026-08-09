use super::*;
use crate::{
    config::{OrchestrationLimits, PermissionMode, ProviderKind},
    context::ContextBudget,
    identity::{OrchestrationPlanId, StepId, ToolInvocationId},
    message::{ContentBlock, Role, ToolCall, ToolResult},
    model::{DescriptorSource, ModelDescriptor},
    operation::{BoundaryObserver, CrashSite},
    orchestration::{
        AwaitAgentOptions, AwaitAgentOutcome, ChildAttribution, ChildExecution,
        ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome, ChildExecutionOutput,
        ChildLifecycle, ChildReport, ChildResultSchema, ChildSupervisor, ChildTerminalStatus,
        ChildUsage, CollectAgentsOptions, CollectionEntryState, CollectionFailurePolicy,
        EnforcementCapabilities, ExecutionOwner, OrchestrationBudget, OrchestrationPlan,
        OrchestrationPlanStep, OrchestrationPlanStepResult, ParentExecution, PlanHandleRef,
        PreparedChild, ResolvedAgentConfig, SpawnAgentRequest, apply_spawn_restrictions,
    },
    permission::{
        ControllerDecision, PermissionAuditFact, PermissionPolicy, PermissionRequest,
        PermissionScope, PolicyDecision,
    },
    prompt::{PromptAssembler, PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
    provider::{ConversationalProvider, DeltaSink, ProviderError},
    session::{DurableSession, SessionRecord, SessionStore, reduce},
    tool::{ToolDefinition, ToolRegistry},
};
use futures::future::BoxFuture;
use std::{
    collections::BTreeSet,
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::tempdir;
use tokio::sync::{Notify, Semaphore};

type CapturedRequests = Arc<Mutex<Vec<Vec<Message>>>>;
type CompletionFlag = Arc<AtomicBool>;

struct QueueTransport {
    responses: Mutex<VecDeque<Result<Message, String>>>,
    requests: CapturedRequests,
    completed: CompletionFlag,
    deltas: Vec<String>,
}

impl ConversationalProvider for QueueTransport {
    fn stream_message<'a>(
        &'a self,
        messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        step_id: StepId,
        deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(messages.to_vec());
            for text in &self.deltas {
                deltas.text_delta(step_id, text);
            }
            let result = self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or_else(|| Err("script exhausted".to_owned()))
                .map_err(ProviderError::new);
            self.completed.store(true, Ordering::SeqCst);
            result
        })
    }
}

#[tokio::test]
async fn root_tool_delegates_one_durable_child_without_an_intermediate_model_turn() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let session =
        DurableSession::create(directory.path(), workspace.clone()).expect("durable session");
    let session_id = session.session_id();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (supervisor_handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: session.agent_id(),
            thread_id: session.thread_id(),
        },
        Arc::new(ImmediateChildFactory {
            workspace: workspace.clone(),
            requests: Arc::clone(&requests),
        }),
    );
    let root_responses = vec![
        Ok(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "delegate-1".to_owned(),
                name: "delegate_agent".to_owned(),
                arguments: serde_json::json!({
                    "route": "worker",
                    "task": "inspect the bounded seam"
                }),
            })],
        }),
        Ok(Message::text(Role::Assistant, "root collected report")),
    ];
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider = QueueTransport {
        responses: Mutex::new(root_responses.into()),
        requests: Arc::clone(&captured),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let mut tools = ToolRegistry::new();
    tools
        .enable_child_delegation(supervisor_handle.clone())
        .expect("delegation tool");
    let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
    let assembler = PromptAssembler::new(
        definitions,
        PromptEnvironment {
            operating_system: "test".to_owned(),
            working_directory: workspace.clone(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 16_384,
            conversation_reserve_tokens: 4_096,
        },
    );
    let prompt = assembler.assemble(&[]).expect("root prompt");
    let agent = Agent::new(Box::new(provider), tools, workspace.clone(), prompt, 2);
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent_with_supervisor(
        agent,
        policy,
        true,
        session,
        assembler,
        supervisor_handle,
        supervisor,
    )
    .expect("persistent runtime with child supervisor");
    let operation_id = OperationId::new();

    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "delegate once".to_owned(),
        })
        .await
        .expect("submit root turn");

    let mut lifecycle = Vec::new();
    let mut terminal_report = None;
    loop {
        let event = runtime.next_event().await.expect("runtime event");
        match event {
            AgentEvent::ChildLifecycleChanged {
                attribution,
                lifecycle: state,
            } => {
                assert_eq!(attribution.parent_operation_id, operation_id);
                assert_eq!(attribution.route, "worker");
                assert_eq!(attribution.connection, "scripted");
                assert_eq!(attribution.model, "child-model");
                lifecycle.push(state);
            }
            AgentEvent::ChildReportCommitted { report } => {
                terminal_report = Some(report);
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(OperationOutcome::Completed),
            } if actual == operation_id => break,
            _ => {}
        }
    }

    assert_eq!(
        lifecycle,
        vec![
            ChildLifecycle::Admitted,
            ChildLifecycle::Queued,
            ChildLifecycle::Running,
            ChildLifecycle::Completed,
        ]
    );
    let report = terminal_report.expect("terminal child report");
    assert_eq!(report.output.as_deref(), Some("child result"));
    assert_eq!(
        requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        &[SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: "inspect the bounded seam".to_owned(),
            result_schema: Default::default(),
            restrictions: Default::default(),
        }]
    );
    {
        let provider_requests = captured
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(provider_requests.len(), 2);
        assert!(matches!(
            provider_requests[1].last(),
            Some(Message {
                role: Role::Tool,
                content,
            }) if matches!(content.as_slice(), [ContentBlock::ToolResult(result)] if result.output.contains("child result"))
        ));
    }

    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("shutdown runtime");
    let (_, restored) =
        DurableSession::inspect_restored(directory.path(), session_id).expect("inspect session");
    assert_eq!(restored.children.len(), 1);
    let child = restored.children.values().next().expect("restored child");
    assert_eq!(child.handle.lifecycle, ChildLifecycle::Completed);
    assert_eq!(
        child
            .report
            .as_ref()
            .and_then(|report| report.output.as_deref()),
        Some("child result")
    );
}

struct BlockingTransport {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct ImmediateChildFactory {
    workspace: std::path::PathBuf,
    requests: Arc<Mutex<Vec<SpawnAgentRequest>>>,
}

struct ImmediateChildExecution;

struct BarrierChildFactory {
    workspace: std::path::PathBuf,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct BarrierChildExecution {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct CountingBarrierChildFactory {
    workspace: std::path::PathBuf,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    maximum_running: Arc<AtomicUsize>,
}

struct CountingBarrierChildExecution {
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
    running: Arc<AtomicUsize>,
    maximum_running: Arc<AtomicUsize>,
}

struct PermissionChildFactory {
    workspace: std::path::PathBuf,
    effect_ran: Arc<AtomicBool>,
}

struct PermissionChildExecution {
    effect_ran: Arc<AtomicBool>,
}

struct ScriptedOutcomeChildFactory {
    workspace: std::path::PathBuf,
}

struct ScriptedOutcomeChildExecution {
    task: String,
}

impl ChildExecutionFactory for ImmediateChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        let resolved = restricted_scripted_child_config(request)?;
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            resolved,
            policy,
            Box::new(ImmediateChildExecution),
        ))
    }
}

impl ChildExecutionFactory for BarrierChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
            policy,
            Box::new(BarrierChildExecution {
                started: Arc::clone(&self.started),
                release: Arc::clone(&self.release),
            }),
        ))
    }
}

impl ChildExecutionFactory for CountingBarrierChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
            policy,
            Box::new(CountingBarrierChildExecution {
                started: Arc::clone(&self.started),
                release: Arc::clone(&self.release),
                running: Arc::clone(&self.running),
                maximum_running: Arc::clone(&self.maximum_running),
            }),
        ))
    }
}

impl ChildExecutionFactory for PermissionChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let policy = PermissionPolicy::new(PolicyDecision::Ask, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
            policy,
            Box::new(PermissionChildExecution {
                effect_ran: Arc::clone(&self.effect_ran),
            }),
        ))
    }
}

impl ChildExecutionFactory for ScriptedOutcomeChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        if request.route.as_deref() == Some("missing") {
            return Err("unknown test route missing".to_owned());
        }
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
            policy,
            Box::new(ScriptedOutcomeChildExecution {
                task: request.task.clone(),
            }),
        ))
    }
}

impl ChildExecution for BarrierChildExecution {
    fn run(
        self: Box<Self>,
        _context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "released child".to_owned(),
                usage: ChildUsage::Unknown,
            })
        })
    }
}

impl ChildExecution for CountingBarrierChildExecution {
    fn run(
        self: Box<Self>,
        _context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_running.fetch_max(running, Ordering::SeqCst);
            self.started.add_permits(1);
            let permit = self
                .release
                .acquire()
                .await
                .expect("release semaphore open");
            permit.forget();
            self.running.fetch_sub(1, Ordering::SeqCst);
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "released counted child".to_owned(),
                usage: ChildUsage::Unknown,
            })
        })
    }
}

impl ChildExecution for PermissionChildExecution {
    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            let authorization = context
                .permissions
                .authorize(PermissionRequest {
                    operation_id: context.operation_id,
                    invocation_id: ToolInvocationId::new(),
                    tool_name: "test_effect".to_owned(),
                    effect_class: crate::tool::EffectClass::Execute,
                    final_arguments: serde_json::json!({"action": "test"}),
                    scope: PermissionScope::Unscoped,
                })
                .await;
            if matches!(
                authorization,
                Ok(crate::permission::Authorization::Allowed(_))
            ) {
                self.effect_ran.store(true, Ordering::SeqCst);
            }
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "permission resolved".to_owned(),
                usage: ChildUsage::Unknown,
            })
        })
    }
}

impl ChildExecution for ScriptedOutcomeChildExecution {
    fn run(
        self: Box<Self>,
        _context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            let Some((kind, value)) = self.task.split_once(':') else {
                return ChildExecutionOutcome::Failed("invalid test script".to_owned());
            };
            match kind {
                "complete" => ChildExecutionOutcome::Completed(ChildExecutionOutput {
                    text: value.to_owned(),
                    usage: ChildUsage::Unknown,
                }),
                "delay" => {
                    let (milliseconds, output) = value
                        .split_once(':')
                        .expect("delay test script contains output");
                    tokio::time::sleep(Duration::from_millis(
                        milliseconds.parse().expect("delay is an integer"),
                    ))
                    .await;
                    ChildExecutionOutcome::Completed(ChildExecutionOutput {
                        text: output.to_owned(),
                        usage: ChildUsage::Unknown,
                    })
                }
                "fail" => ChildExecutionOutcome::Failed(value.to_owned()),
                "pending" => std::future::pending().await,
                other => ChildExecutionOutcome::Failed(format!("unknown test outcome {other}")),
            }
        })
    }
}

fn scripted_child_config(request: &SpawnAgentRequest) -> ResolvedAgentConfig {
    ResolvedAgentConfig {
        route: request.route.clone().unwrap_or_else(|| "worker".to_owned()),
        profile: "worker".to_owned(),
        connection: "scripted".to_owned(),
        provider_kind: ProviderKind::OpenAiCompat,
        owner: ExecutionOwner::Native,
        model: ModelDescriptor {
            id: "child-model".to_owned(),
            display_name: "Child Model".to_owned(),
            input_modalities: BTreeSet::from(["text".to_owned()]),
            tools: Some(false),
            reasoning: Some(false),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            context_tokens: Some(8_192),
            max_output_tokens: Some(1_024),
            source: DescriptorSource::Configured,
            is_default: false,
        },
        reasoning_effort: None,
        reasoning_summary: None,
        capabilities: xana_core::AgentCapabilitySnapshot::new(BTreeSet::new(), BTreeSet::new()),
        permission_mode: PermissionMode::Allow,
        max_tool_rounds: 2,
        orchestration: OrchestrationLimits::default(),
        hard_token_limit: None,
        hard_spend_microusd: None,
    }
}

fn restricted_scripted_child_config(
    request: &SpawnAgentRequest,
) -> Result<ResolvedAgentConfig, String> {
    let mut resolved = scripted_child_config(request);
    apply_spawn_restrictions(
        &mut resolved,
        &request.restrictions,
        EnforcementCapabilities {
            hard_tokens: false,
            hard_spend: false,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(resolved)
}

impl ChildExecution for ImmediateChildExecution {
    fn run(
        self: Box<Self>,
        _context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async {
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "child result".to_owned(),
                usage: ChildUsage::Unknown,
            })
        })
    }
}

impl ConversationalProvider for BlockingTransport {
    fn stream_message<'a>(
        &'a self,
        _messages: &'a [Message],
        _tools: &'a [&'a ToolDefinition],
        _step_id: StepId,
        _deltas: &'a dyn DeltaSink,
    ) -> BoxFuture<'a, Result<Message, ProviderError>> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Message::text(Role::Assistant, "released"))
        })
    }
}

fn make_agent(provider: Box<dyn ConversationalProvider>) -> Agent {
    let tools = ToolRegistry::new();
    let definitions = tools.definitions();
    let workspace = std::env::current_dir().expect("current directory");
    let environment = PromptEnvironment {
        operating_system: "test".to_owned(),
        working_directory: workspace.clone(),
        configured_shell: "test shell".to_owned(),
        surface: PromptSurface::Cli,
    };
    let prompt = assemble_snapshot(PromptInputs {
        tool_definitions: &definitions,
        environment: &environment,
        product_documentation: None,
        project_sources: &[],
        budget: ContextBudget {
            total_tokens: 16_384,
            conversation_reserve_tokens: 4_096,
        },
    })
    .expect("test prompt");
    Agent::new(provider, tools, workspace, prompt, 2)
}

fn spawn_runtime(agent: Agent) -> RuntimeHandle {
    let workspace = std::env::current_dir().expect("current directory");
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).expect("allow policy");
    RuntimeHandle::spawn(agent, policy, true)
}

fn queue_agent(
    responses: Vec<Result<Message, String>>,
    deltas: Vec<String>,
) -> (Agent, CapturedRequests, CompletionFlag) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));
    let transport = QueueTransport {
        responses: Mutex::new(responses.into()),
        requests: Arc::clone(&requests),
        completed: Arc::clone(&completed),
        deltas,
    };
    (make_agent(Box::new(transport)), requests, completed)
}

fn persistent_agent(
    provider: Box<dyn ConversationalProvider>,
    workspace: std::path::PathBuf,
) -> (Agent, PromptAssembler) {
    let tools = ToolRegistry::new();
    let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
    let assembler = PromptAssembler::new(
        definitions,
        PromptEnvironment {
            operating_system: "test".to_owned(),
            working_directory: workspace.clone(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 16_384,
            conversation_reserve_tokens: 4_096,
        },
    );
    let prompt = assembler.assemble(&[]).expect("base prompt");
    (Agent::new(provider, tools, workspace, prompt, 2), assembler)
}

async fn receive_finished(
    handle: &mut RuntimeHandle,
    operation_id: OperationId,
) -> OperationOutcome {
    loop {
        match handle.next_event().await.expect("runtime event") {
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(outcome),
            } if actual == operation_id => return outcome,
            _ => {}
        }
    }
}

#[tokio::test]
async fn dropping_one_await_does_not_detach_the_supervised_child() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let limits = OrchestrationLimits {
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
        OrchestrationBudget::for_tests(limits, 8),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "wait at the barrier".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;
    let child_id = admitted.admission.attribution.agent_id;
    let abandoned = tokio::spawn({
        let handle = handle.clone();
        async move { handle.await_agent(child_id).await }
    });
    tokio::task::yield_now().await;
    abandoned.abort();
    let _ = abandoned.await;

    release.notify_one();
    let report = handle
        .await_agent(child_id)
        .await
        .expect("later await succeeds");
    assert_eq!(report.output.as_deref(), Some("released child"));
    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn running_cancellation_is_observed_once_and_repeated_reads_are_idempotent() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release,
        }),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let committed = Arc::clone(&records);
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "cancel at the barrier".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;
    let agent_id = admitted.admission.attribution.agent_id;

    let first = handle.cancel_agent(agent_id).await.expect("cancel child");
    let repeated = handle
        .cancel_agent(agent_id)
        .await
        .expect("repeat cancellation");
    assert!(first.newly_requested);
    assert!(!repeated.newly_requested);

    let report = handle.await_agent(agent_id).await.expect("cancel report");
    assert_eq!(report.status, ChildTerminalStatus::Cancelled);
    assert_eq!(
        handle.await_agent(agent_id).await.expect("repeat report"),
        report
    );
    let inspection = handle.inspect_agent(agent_id).await.expect("inspect child");
    assert_eq!(inspection.handle.lifecycle, ChildLifecycle::Cancelled);
    assert_eq!(inspection.report.as_ref(), Some(&report));

    tokio::task::yield_now().await;
    let terminal_events = std::iter::from_fn(|| event_receiver.try_recv().ok())
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ChildReportCommitted { .. }
                    | AgentEvent::ChildLifecycleChanged {
                        lifecycle: ChildLifecycle::Cancelled,
                        ..
                    }
            )
        })
        .count();
    assert_eq!(terminal_events, 2, "one lifecycle and one report event");
    assert_eq!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|record| matches!(record, SessionRecord::ChildReportCommitted { .. }))
            .count(),
        1
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn queued_cancellation_never_starts_the_child_and_releases_its_slot_once() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let limits = OrchestrationLimits {
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
        OrchestrationBudget::for_tests(limits, 8),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let first = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "occupy the only running slot".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("first child");
    started.notified().await;
    let second = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "remain queued".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("second child");
    let second_id = second.admission.attribution.agent_id;
    assert_eq!(second.lifecycle, ChildLifecycle::Queued);

    let receipt = handle
        .cancel_agent(second_id)
        .await
        .expect("cancel queued child");
    assert!(receipt.newly_requested);
    assert_eq!(receipt.handle.lifecycle, ChildLifecycle::Cancelled);
    assert_eq!(
        handle
            .await_agent(second_id)
            .await
            .expect("queued report")
            .status,
        ChildTerminalStatus::Cancelled
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), started.notified())
            .await
            .is_err(),
        "the cancelled queued execution must never start"
    );

    release.notify_one();
    assert_eq!(
        handle
            .await_agent(first.admission.attribution.agent_id)
            .await
            .expect("first report")
            .status,
        ChildTerminalStatus::Completed
    );
    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn atomic_batch_runs_in_input_order_without_exceeding_parallel_capacity() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let maximum_running = Arc::new(AtomicUsize::new(0));
    let limits = OrchestrationLimits {
        max_fan_out: 4,
        max_descendants: 4,
        max_concurrency: 2,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(CountingBarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            running: Arc::clone(&running),
            maximum_running: Arc::clone(&maximum_running),
        }),
        OrchestrationBudget::for_tests(limits, 2),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let committed = Arc::clone(&records);
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let operation_id = OperationId::new();
    let requests = (0..4)
        .map(|index| SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: format!("parallel child {index}"),
            result_schema: Default::default(),
            restrictions: Default::default(),
        })
        .collect::<Vec<_>>();

    let handles = handle
        .spawn_many(operation_id, requests)
        .await
        .expect("admit full batch");
    assert_eq!(handles.len(), 4);
    assert!(
        handles
            .iter()
            .all(|handle| handle.lifecycle == ChildLifecycle::Queued)
    );
    let first_starts = started.acquire_many(2).await.expect("two starts");
    first_starts.forget();
    assert_eq!(running.load(Ordering::SeqCst), 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), started.acquire())
            .await
            .is_err(),
        "a third child must remain queued"
    );

    release.add_permits(4);
    let remaining_starts = started.acquire_many(2).await.expect("remaining starts");
    remaining_starts.forget();
    for child in &handles {
        let report = handle
            .await_agent(child.admission.attribution.agent_id)
            .await
            .expect("collect child");
        assert_eq!(report.status, ChildTerminalStatus::Completed);
    }
    assert_eq!(maximum_running.load(Ordering::SeqCst), 2);
    assert_eq!(running.load(Ordering::SeqCst), 0);
    assert_eq!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|record| matches!(record, SessionRecord::ChildrenBatchAdmitted { .. }))
            .count(),
        1
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn collection_preserves_input_order_mixed_failures_and_fail_fast_evidence() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ScriptedOutcomeChildFactory { workspace }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let request = |task: &str| SpawnAgentRequest {
        route: Some("worker".to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
    };
    let children = handle
        .spawn_many(
            OperationId::new(),
            vec![
                request("delay:30:first"),
                request("fail:second failed"),
                request("delay:10:third"),
            ],
        )
        .await
        .expect("mixed batch");
    let ids = children
        .iter()
        .map(|child| child.admission.attribution.agent_id)
        .collect::<Vec<_>>();
    let result = handle
        .collect_agents(ids.clone(), CollectAgentsOptions::default())
        .await
        .expect("continue-on-error collection");
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.attribution.agent_id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![
            Some(ChildTerminalStatus::Completed),
            Some(ChildTerminalStatus::Failed),
            Some(ChildTerminalStatus::Completed),
        ]
    );
    assert!(result.complete);
    assert!(
        result
            .entries
            .iter()
            .all(|entry| entry.usage == ChildUsage::Unknown)
    );
    assert_eq!(
        handle
            .collect_agents(ids, CollectAgentsOptions::default())
            .await
            .expect("idempotent repeat"),
        result
    );

    let fail_fast = handle
        .spawn_many(
            OperationId::new(),
            vec![
                request("fail:stop now"),
                request("pending:one"),
                request("pending:two"),
            ],
        )
        .await
        .expect("fail-fast batch");
    let failed_id = fail_fast[0].admission.attribution.agent_id;
    let _ = handle
        .await_agent(failed_id)
        .await
        .expect("failed child report");
    let fail_fast_ids = fail_fast
        .iter()
        .map(|child| child.admission.attribution.agent_id)
        .collect::<Vec<_>>();
    let result = handle
        .collect_agents(
            fail_fast_ids,
            CollectAgentsOptions {
                failure_policy: CollectionFailurePolicy::FailFast,
                cancel_remaining: true,
                ..Default::default()
            },
        )
        .await
        .expect("fail-fast collection");
    assert_eq!(result.entries[0].status, Some(ChildTerminalStatus::Failed));
    assert!(result.entries[1..].iter().all(|entry| {
        entry.state == CollectionEntryState::SkippedAfterFailure && entry.cancellation_requested
    }));
    assert!(!result.complete);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn validated_plan_uses_the_supervisor_once_and_preserves_collection_order() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ScriptedOutcomeChildFactory { workspace }),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let committed = Arc::clone(&records);
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let spawn_request = |task: &str| SpawnAgentRequest {
        route: Some("worker".to_owned()),
        task: task.to_owned(),
        result_schema: ChildResultSchema::Summary,
        restrictions: Default::default(),
    };
    let plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![
            OrchestrationPlanStep::Spawn {
                id: "parallel".to_owned(),
                requests: vec![
                    spawn_request("delay:30:first"),
                    spawn_request("delay:10:second"),
                    spawn_request("delay:20:third"),
                ],
            },
            OrchestrationPlanStep::Collect {
                id: "ordered".to_owned(),
                handles: vec![
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 2,
                    },
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 0,
                    },
                    PlanHandleRef {
                        step: "parallel".to_owned(),
                        index: 1,
                    },
                ],
                timeout_ms: None,
                failure_policy: CollectionFailurePolicy::ContinueOnError,
                cancel_remaining: false,
                cancel_on_timeout: false,
            },
        ],
    };
    let operation_id = OperationId::new();
    let first_validation = handle
        .validate_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("first pure validation");
    let second_validation = handle
        .validate_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("repeat pure validation");
    assert_eq!(first_validation, second_validation);
    let mut invalid_route = plan.clone();
    invalid_route.plan_id = OrchestrationPlanId::new();
    let OrchestrationPlanStep::Spawn { requests, .. } = &mut invalid_route.steps[0] else {
        unreachable!("fixture begins with spawn");
    };
    requests[0].route = Some("missing".to_owned());
    assert!(
        handle
            .validate_orchestration_plan(operation_id, invalid_route)
            .await
            .is_err()
    );
    let over_budget = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![OrchestrationPlanStep::Spawn {
            id: "too_wide".to_owned(),
            requests: (0..9)
                .map(|index| spawn_request(&format!("complete:{index}")))
                .collect(),
        }],
    };
    assert!(matches!(
        handle
            .validate_orchestration_plan(operation_id, over_budget)
            .await,
        Err(crate::orchestration::SupervisorError::Budget(_))
    ));
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty(),
        "validation must commit no records"
    );
    assert!(handle.list_agents().await.expect("children").is_empty());

    let result = handle
        .execute_orchestration_plan(operation_id, plan.clone())
        .await
        .expect("execute plan");
    let OrchestrationPlanStepResult::Spawn { handles, .. } = &result.steps[0] else {
        panic!("first result is spawn");
    };
    assert!(handles.iter().enumerate().all(|(index, handle)| {
        handle.admission.plan.as_ref().is_some_and(|attribution| {
            attribution.plan_id == plan.plan_id
                && attribution.step_id == "parallel"
                && attribution.output_index == index
        })
    }));
    let expected = [
        handles[2].admission.attribution.agent_id,
        handles[0].admission.attribution.agent_id,
        handles[1].admission.attribution.agent_id,
    ];
    let OrchestrationPlanStepResult::Collect { result, .. } = &result.steps[1] else {
        panic!("second result is collection");
    };
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.attribution.agent_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(result.entries.iter().all(|entry| {
        entry.status == Some(ChildTerminalStatus::Completed) && entry.usage == ChildUsage::Unknown
    }));
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|record| matches!(record, SessionRecord::OrchestrationPlanStarted { .. }))
    );
    assert!(matches!(
        handle.execute_orchestration_plan(operation_id, plan).await,
        Err(crate::orchestration::SupervisorError::DuplicatePlan(_))
    ));

    let cancel_plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![
            OrchestrationPlanStep::Spawn {
                id: "pending".to_owned(),
                requests: vec![spawn_request("pending:forever")],
            },
            OrchestrationPlanStep::Cancel {
                id: "stop".to_owned(),
                handle: PlanHandleRef {
                    step: "pending".to_owned(),
                    index: 0,
                },
            },
            OrchestrationPlanStep::Await {
                id: "stopped".to_owned(),
                handle: PlanHandleRef {
                    step: "pending".to_owned(),
                    index: 0,
                },
                timeout_ms: None,
                cancel_on_timeout: false,
            },
        ],
    };
    let cancellation = handle
        .execute_orchestration_plan(OperationId::new(), cancel_plan)
        .await
        .expect("cancel plan");
    let OrchestrationPlanStepResult::Await { outcome, .. } = &cancellation.steps[2] else {
        panic!("final cancellation evidence is await");
    };
    assert!(matches!(
        outcome,
        AwaitAgentOutcome::Report(report)
            if report.status == ChildTerminalStatus::Cancelled
    ));

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn plan_start_persistence_failure_admits_no_child() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ScriptedOutcomeChildFactory { workspace }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        if let Some(command) = commit_receiver.recv().await {
            assert!(matches!(
                command.record,
                SessionRecord::OrchestrationPlanStarted { .. }
            ));
            let _ = command
                .acknowledged
                .send(Err("injected plan start failure".to_owned()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let plan = OrchestrationPlan {
        version: 1,
        plan_id: OrchestrationPlanId::new(),
        steps: vec![OrchestrationPlanStep::Spawn {
            id: "never".to_owned(),
            requests: vec![SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "complete:never admitted".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: Default::default(),
            }],
        }],
    };
    assert!(matches!(
        handle
            .execute_orchestration_plan(OperationId::new(), plan)
            .await,
        Err(crate::orchestration::SupervisorError::Durability(_))
    ));
    assert!(handle.list_agents().await.expect("children").is_empty());

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn report_overflow_is_registered_before_commit_and_missing_bytes_are_attributed() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let artifact_root = directory.path().join("artifacts");
    let artifacts = crate::artifact::ArtifactStore::new(artifact_root.clone());
    let (handle, supervisor) = ChildSupervisor::new_with_budget_and_artifacts(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ImmediateChildFactory {
            workspace,
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        OrchestrationBudget::new(OrchestrationLimits::default(), 8),
        artifacts,
        crate::identity::PrincipalId::new(),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let committed = Arc::clone(&records);
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let child = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "overflow report".to_owned(),
                result_schema: ChildResultSchema::Summary,
                restrictions: crate::orchestration::ChildRestrictions {
                    max_report_bytes: Some("child result".len() - 1),
                    ..Default::default()
                },
            },
        )
        .await
        .expect("overflow child");
    let child_id = child.admission.attribution.agent_id;
    let report = handle.await_agent(child_id).await.expect("artifact report");
    let crate::orchestration::ChildReportReference::Artifact { artifact, .. } = &report.reference
    else {
        panic!("one byte over the inline limit must use an artifact");
    };
    let artifact_path = {
        let records = records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let artifact_index = records
            .iter()
            .position(|record| matches!(record, SessionRecord::ArtifactRegistered { .. }))
            .expect("artifact registration");
        let report_index = records
            .iter()
            .position(|record| matches!(record, SessionRecord::ChildReportCommitted { .. }))
            .expect("report commit");
        assert!(artifact_index < report_index);
        artifact_root.join(artifact.content_hash.as_str())
    };
    std::fs::remove_file(artifact_path).expect("remove artifact bytes");

    let collected = handle
        .collect_agents(vec![child_id], CollectAgentsOptions::default())
        .await
        .expect("attributed collection result");
    assert_eq!(
        collected.entries[0].state,
        CollectionEntryState::ArtifactUnavailable
    );
    assert_eq!(collected.entries[0].attribution.agent_id, child_id);

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn invalid_or_oversized_batch_creates_no_child_record_or_event() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let requests_seen = Arc::new(Mutex::new(Vec::new()));
    let limits = OrchestrationLimits {
        max_fan_out: 2,
        max_descendants: 2,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(ImmediateChildFactory {
            workspace,
            requests: Arc::clone(&requests_seen),
        }),
        OrchestrationBudget::for_tests(limits, 2),
    );
    let records = Arc::new(Mutex::new(Vec::new()));
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let committed = Arc::clone(&records);
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            committed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(command.record);
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));

    let oversized = (0..3)
        .map(|index| SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: format!("too many {index}"),
            result_schema: Default::default(),
            restrictions: Default::default(),
        })
        .collect();
    assert!(matches!(
        handle.spawn_many(OperationId::new(), oversized).await,
        Err(crate::orchestration::SupervisorError::Budget(
            crate::orchestration::BudgetError::FanOut { .. }
        ))
    ));
    assert!(
        handle
            .spawn_many(
                OperationId::new(),
                vec![
                    SpawnAgentRequest {
                        route: Some("worker".to_owned()),
                        task: "valid first member".to_owned(),
                        result_schema: Default::default(),
                        restrictions: Default::default(),
                    },
                    SpawnAgentRequest {
                        route: Some("worker".to_owned()),
                        task: "  ".to_owned(),
                        result_schema: Default::default(),
                        restrictions: Default::default(),
                    },
                ],
            )
            .await
            .is_err()
    );
    tokio::task::yield_now().await;
    assert!(
        records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    );
    assert!(event_receiver.try_recv().is_err());
    assert!(
        handle
            .list_agents()
            .await
            .expect("list children")
            .is_empty()
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn competing_batch_callers_share_one_atomic_descendant_ledger() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let running = Arc::new(AtomicUsize::new(0));
    let maximum_running = Arc::new(AtomicUsize::new(0));
    let limits = OrchestrationLimits {
        max_fan_out: 2,
        max_descendants: 2,
        max_concurrency: 1,
        ..Default::default()
    };
    let (handle, supervisor) = ChildSupervisor::new_with_budget(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(CountingBarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            running,
            maximum_running,
        }),
        OrchestrationBudget::for_tests(limits, 2),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let batch = |label: &str| {
        (0..2)
            .map(|index| SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: format!("{label} {index}"),
                result_schema: Default::default(),
                restrictions: Default::default(),
            })
            .collect::<Vec<_>>()
    };
    let first_handle = handle.clone();
    let second_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .spawn_many(OperationId::new(), batch("first"))
            .await
    });
    let second = tokio::spawn(async move {
        second_handle
            .spawn_many(OperationId::new(), batch("second"))
            .await
    });
    let first = first.await.expect("first caller task");
    let second = second.await.expect("second caller task");
    let winner = match (first, second) {
        (Ok(handles), Err(crate::orchestration::SupervisorError::Budget(_)))
        | (Err(crate::orchestration::SupervisorError::Budget(_)), Ok(handles)) => handles,
        outcomes => panic!("exactly one caller must reserve the shared ledger: {outcomes:?}"),
    };
    assert_eq!(winner.len(), 2);
    let first_start = started.acquire().await.expect("one running child");
    first_start.forget();
    release.add_permits(2);
    let second_start = started.acquire().await.expect("queued child starts fairly");
    second_start.forget();
    for child in winner {
        assert_eq!(
            handle
                .await_agent(child.admission.attribution.agent_id)
                .await
                .expect("winner report")
                .status,
            ChildTerminalStatus::Completed
        );
    }

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn admission_deadline_cancels_running_work_and_releases_its_reservation() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let timed = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "observe the admission deadline".to_owned(),
                result_schema: Default::default(),
                restrictions: crate::orchestration::ChildRestrictions {
                    deadline_seconds: Some(1),
                    ..Default::default()
                },
            },
        )
        .await
        .expect("admit timed child");
    started.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        handle
            .await_agent(timed.admission.attribution.agent_id)
            .await
            .expect("deadline report")
            .status,
        ChildTerminalStatus::Cancelled
    );

    let replacement = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "reuse released reservation".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("replacement admission");
    started.notified().await;
    release.notify_one();
    assert_eq!(
        handle
            .await_agent(replacement.admission.attribution.agent_id)
            .await
            .expect("replacement report")
            .status,
        ChildTerminalStatus::Completed
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn await_timeout_is_not_cancellation_unless_explicitly_requested() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(BarrierChildFactory {
            workspace,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, _event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let first = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "time out without cancelling".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("first child");
    started.notified().await;
    let first_id = first.admission.attribution.agent_id;
    let collected_timeout = handle
        .collect_agents(
            vec![first_id],
            CollectAgentsOptions {
                timeout: Some(Duration::ZERO),
                ..Default::default()
            },
        )
        .await
        .expect("collection timeout");
    assert_eq!(
        collected_timeout.entries[0].state,
        CollectionEntryState::TimedOut
    );
    assert!(!collected_timeout.entries[0].cancellation_requested);
    assert_eq!(
        handle
            .await_agent_with(
                first_id,
                AwaitAgentOptions {
                    timeout: Some(Duration::ZERO),
                    cancel_on_timeout: false,
                },
            )
            .await
            .expect("timeout outcome"),
        AwaitAgentOutcome::TimedOut {
            agent_id: first_id,
            cancellation_requested: false,
        }
    );
    assert_eq!(
        handle
            .inspect_agent(first_id)
            .await
            .expect("still supervised")
            .handle
            .lifecycle,
        ChildLifecycle::Running
    );
    release.notify_one();
    assert_eq!(
        handle
            .await_agent(first_id)
            .await
            .expect("later collection")
            .status,
        ChildTerminalStatus::Completed
    );

    let second = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "time out and cancel".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("second child");
    started.notified().await;
    let second_id = second.admission.attribution.agent_id;
    assert_eq!(
        handle
            .await_agent_with(
                second_id,
                AwaitAgentOptions {
                    timeout: Some(Duration::ZERO),
                    cancel_on_timeout: true,
                },
            )
            .await
            .expect("cancel-on-timeout outcome"),
        AwaitAgentOutcome::TimedOut {
            agent_id: second_id,
            cancellation_requested: true,
        }
    );
    assert_eq!(
        handle
            .await_agent(second_id)
            .await
            .expect("cancel terminal")
            .status,
        ChildTerminalStatus::Cancelled
    );

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn cancelling_a_child_closes_pending_permission_before_any_effect() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let effect_ran = Arc::new(AtomicBool::new(false));
    let (handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
            thread_id: crate::identity::ThreadId::new(),
        },
        Arc::new(PermissionChildFactory {
            workspace,
            effect_ran: Arc::clone(&effect_ran),
        }),
    );
    let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
    let commit_task = tokio::spawn(async move {
        while let Some(command) = commit_receiver.recv().await {
            let _ = command.acknowledged.send(Ok(()));
        }
    });
    let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervisor_task = tokio::spawn(supervisor.run(commits, events));
    let admitted = handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "request one effect".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("admit permission child");
    let agent_id = admitted.admission.attribution.agent_id;
    let request = loop {
        if let AgentEvent::ChildActivity {
            activity: crate::orchestration::ChildActivity::PermissionRequested { request },
            ..
        } = event_receiver.recv().await.expect("child permission event")
        {
            break request;
        }
    };

    assert!(
        handle
            .cancel_agent(agent_id)
            .await
            .expect("cancel permission child")
            .newly_requested
    );
    assert!(
        handle
            .decide_permission(
                agent_id,
                request.operation_id,
                request.invocation_id,
                ControllerDecision::AllowOnce,
            )
            .await
            .is_err(),
        "a cancellation-closed permission request cannot later authorize an effect"
    );
    assert_eq!(
        handle
            .await_agent(agent_id)
            .await
            .expect("cancel report")
            .status,
        ChildTerminalStatus::Cancelled
    );
    assert!(!effect_ran.load(Ordering::SeqCst));

    handle.shutdown().await;
    supervisor_task.await.expect("supervisor task");
    commit_task.await.expect("commit task");
}

#[tokio::test]
async fn runtime_shutdown_observes_child_terminal_commit_before_stopping() {
    let directory = tempdir().expect("temporary directory");
    let workspace = directory
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let session =
        DurableSession::create(directory.path(), workspace.clone()).expect("durable session");
    let session_id = session.session_id();
    let started = Arc::new(Notify::new());
    let (supervisor_handle, supervisor) = ChildSupervisor::new(
        ParentExecution {
            agent_id: session.agent_id(),
            thread_id: session.thread_id(),
        },
        Arc::new(BarrierChildFactory {
            workspace: workspace.clone(),
            started: Arc::clone(&started),
            release: Arc::new(Notify::new()),
        }),
    );
    let provider = QueueTransport {
        responses: Mutex::new(VecDeque::new()),
        requests: Arc::new(Mutex::new(Vec::new())),
        completed: Arc::new(AtomicBool::new(false)),
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace.clone());
    let policy =
        PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace).expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent_with_supervisor(
        agent,
        policy,
        true,
        session,
        assembler,
        supervisor_handle.clone(),
        supervisor,
    )
    .expect("persistent runtime");
    let admitted = supervisor_handle
        .spawn_agent(
            OperationId::new(),
            SpawnAgentRequest {
                route: Some("worker".to_owned()),
                task: "stop with the runtime".to_owned(),
                result_schema: Default::default(),
                restrictions: Default::default(),
            },
        )
        .await
        .expect("admit child");
    started.notified().await;

    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("request shutdown");
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.next_event().await.is_some() {}
    })
    .await
    .expect("runtime shutdown is bounded");

    let (_, restored) =
        DurableSession::inspect_restored(directory.path(), session_id).expect("inspect session");
    let child = restored
        .children
        .get(&admitted.admission.attribution.agent_id)
        .expect("durable child");
    assert_eq!(child.handle.lifecycle, ChildLifecycle::Cancelled);
    assert_eq!(
        child.report.as_ref().map(|report| report.status),
        Some(ChildTerminalStatus::Cancelled)
    );
}

#[tokio::test]
async fn child_events_never_claim_a_transition_whose_commit_failed() {
    for fail_at in 1..=4 {
        let directory = tempdir().expect("temporary directory");
        let workspace = directory
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (handle, supervisor) = ChildSupervisor::new(
            ParentExecution {
                agent_id: crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
                thread_id: crate::identity::ThreadId::new(),
            },
            Arc::new(ImmediateChildFactory {
                workspace,
                requests,
            }),
        );
        let (commits, mut commit_receiver) = crate::orchestration::ChildCommitSender::channel();
        let commit_task = tokio::spawn(async move {
            let mut count = 0;
            while let Some(command) = commit_receiver.recv().await {
                count += 1;
                let result = if count == fail_at {
                    Err(format!("injected commit failure {fail_at}"))
                } else {
                    Ok(())
                };
                let _ = command.acknowledged.send(result);
            }
        });
        let (events, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let supervisor_task = tokio::spawn(supervisor.run(commits, events));

        let spawn = handle
            .spawn_agent(
                OperationId::new(),
                SpawnAgentRequest {
                    route: Some("worker".to_owned()),
                    task: "commit ordering".to_owned(),
                    result_schema: Default::default(),
                    restrictions: Default::default(),
                },
            )
            .await;
        if fail_at <= 2 {
            assert!(spawn.is_err(), "startup commit {fail_at} should fail");
        } else {
            let child_id = spawn
                .expect("running/report commits happen after admission")
                .admission
                .attribution
                .agent_id;
            assert!(handle.await_agent(child_id).await.is_err());
        }
        tokio::task::yield_now().await;
        let emitted = std::iter::from_fn(|| event_receiver.try_recv().ok()).collect::<Vec<_>>();
        let lifecycle_count = emitted
            .iter()
            .filter(|event| matches!(event, AgentEvent::ChildLifecycleChanged { .. }))
            .count();
        assert_eq!(lifecycle_count, fail_at - 1, "commit {fail_at}");
        assert!(!emitted.iter().any(|event| matches!(
            event,
            AgentEvent::ChildReportCommitted { .. }
                | AgentEvent::ChildLifecycleChanged {
                    lifecycle: ChildLifecycle::Completed
                        | ChildLifecycle::Failed
                        | ChildLifecycle::Cancelled
                        | ChildLifecycle::Interrupted,
                    ..
                }
        )));

        handle.shutdown().await;
        supervisor_task.await.expect("supervisor task");
        commit_task.await.expect("commit task");
    }
}

#[test]
fn commands_and_events_round_trip_through_json() {
    let operation_id = OperationId::new();
    let step_id = StepId::new();
    let invocation_id = ToolInvocationId::new();
    let result_id = crate::identity::ToolResultId::new();
    let message = Message::text(Role::Assistant, "hello");
    let child_attribution = ChildAttribution::new(
        crate::identity::AgentId::new(),
        crate::identity::AgentId::for_session(crate::identity::SessionId::new()),
        operation_id,
        crate::identity::ThreadId::new(),
        &scripted_child_config(&SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: "task".to_owned(),
            result_schema: Default::default(),
            restrictions: Default::default(),
        }),
    );
    let commands = vec![
        RuntimeCommand::SubmitTurn {
            operation_id,
            input: "hello".to_owned(),
        },
        RuntimeCommand::ClearConversation,
        RuntimeCommand::ResumeOperation {
            session_id: crate::identity::SessionId::new(),
            operation_id,
        },
        RuntimeCommand::DecidePermission {
            operation_id,
            invocation_id,
            decision: ControllerDecision::AllowOnce,
        },
        RuntimeCommand::DecideChildPermission {
            agent_id: child_attribution.agent_id,
            operation_id: child_attribution.operation_id,
            invocation_id,
            decision: ControllerDecision::Deny,
        },
        RuntimeCommand::ListChildren,
        RuntimeCommand::InspectChild {
            agent_id: child_attribution.agent_id,
        },
        RuntimeCommand::CancelChild {
            agent_id: child_attribution.agent_id,
        },
        RuntimeCommand::Shutdown,
    ];
    let events = vec![
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        },
        AgentEvent::ChildLifecycleChanged {
            attribution: child_attribution.clone(),
            lifecycle: ChildLifecycle::Running,
        },
        AgentEvent::ChildReportCommitted {
            report: ChildReport::completed(
                child_attribution,
                "done".to_owned(),
                ChildUsage::Unknown,
            ),
        },
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Suspended,
        },
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(OperationOutcome::Completed),
        },
        AgentEvent::AssistantTextDelta {
            operation_id,
            step_id,
            text: "hel".to_owned(),
        },
        AgentEvent::PermissionRequested {
            request: PermissionRequest {
                operation_id,
                invocation_id,
                tool_name: "read_file".to_owned(),
                effect_class: crate::tool::EffectClass::Read,
                final_arguments: serde_json::json!({"path": "README.md"}),
                scope: PermissionScope::Unscoped,
            },
        },
        AgentEvent::PermissionAudited {
            fact: PermissionAuditFact {
                request: PermissionRequest {
                    operation_id,
                    invocation_id,
                    tool_name: "read_file".to_owned(),
                    effect_class: crate::tool::EffectClass::Read,
                    final_arguments: serde_json::json!({"path": "README.md"}),
                    scope: PermissionScope::Unscoped,
                },
                policy_evaluation: PolicyDecision::Ask,
                controller_decision: Some(ControllerDecision::AllowOnce),
                effective: PolicyDecision::Allow,
            },
        },
        AgentEvent::InvocationIntentCommitted {
            intent: crate::operation::InvocationIntent {
                operation_id,
                step_id,
                invocation_id,
                result_id,
                model_call_id: "call-1".to_owned(),
                target: crate::operation::InvocationTarget::Tool {
                    name: "read_file".to_owned(),
                    contract_version: 1,
                },
                final_arguments: serde_json::json!({"path": "README.md"}),
                permission: PermissionAuditFact {
                    request: PermissionRequest {
                        operation_id,
                        invocation_id,
                        tool_name: "read_file".to_owned(),
                        effect_class: crate::tool::EffectClass::Read,
                        final_arguments: serde_json::json!({"path": "README.md"}),
                        scope: PermissionScope::Unscoped,
                    },
                    policy_evaluation: PolicyDecision::Allow,
                    controller_decision: None,
                    effective: PolicyDecision::Allow,
                },
                saved_replay_safety: crate::tool::ReplaySafety::Safe,
            },
        },
        AgentEvent::InvocationResultCommitted {
            result: crate::operation::InvocationResultRecord {
                operation_id,
                invocation_id,
                result_id,
                outcome: crate::operation::InvocationOutcome::Completed {
                    output: crate::operation::DurableValueRef::InlineJson(serde_json::json!(
                        "contents"
                    )),
                },
            },
        },
        AgentEvent::ToolFinished {
            operation_id,
            invocation_id,
            result: Message::tool_result(ToolResult::success("call-1", "ok")),
        },
        AgentEvent::AssistantMessage {
            operation_id,
            message,
        },
        AgentEvent::OperationFailed {
            operation_id,
            reason: "provider unavailable".to_owned(),
        },
        AgentEvent::ConversationCleared,
        AgentEvent::CommandRejected {
            reason: "busy".to_owned(),
        },
    ];

    for command in commands {
        let encoded = serde_json::to_string(&command).expect("command JSON");
        assert_eq!(
            serde_json::from_str::<RuntimeCommand>(&encoded).expect("decoded command"),
            command
        );
    }
    for event in events {
        let encoded = serde_json::to_string(&event).expect("event JSON");
        assert_eq!(
            serde_json::from_str::<AgentEvent>(&encoded).expect("decoded event"),
            event
        );
    }
}

#[tokio::test]
async fn runtime_owns_history_across_turns() {
    let (agent, requests, _) = queue_agent(
        vec![
            Ok(Message::text(Role::Assistant, "first answer")),
            Ok(Message::text(Role::Assistant, "second answer")),
        ],
        Vec::new(),
    );
    let mut runtime = spawn_runtime(agent);
    let first = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: first,
            input: "first question".to_owned(),
        })
        .await
        .expect("first command");
    assert_eq!(
        receive_finished(&mut runtime, first).await,
        OperationOutcome::Completed
    );

    let second = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: second,
            input: "second question".to_owned(),
        })
        .await
        .expect("second command");
    assert_eq!(
        receive_finished(&mut runtime, second).await,
        OperationOutcome::Completed
    );

    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1][1], Message::text(Role::User, "first question"));
    assert_eq!(
        requests[1][2],
        Message::text(Role::Assistant, "first answer")
    );
    assert_eq!(requests[1][3], Message::text(Role::User, "second question"));
}

#[tokio::test]
async fn persistent_runtime_commits_conversation_before_final_events() {
    let data = tempdir().expect("Xana data tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));
    let provider = QueueTransport {
        responses: Mutex::new(vec![Ok(Message::text(Role::Assistant, "durable answer"))].into()),
        requests: Arc::clone(&requests),
        completed,
        deltas: Vec::new(),
    };
    let (agent, assembler) = persistent_agent(Box::new(provider), workspace_root.clone());
    let session = DurableSession::create(data.path(), workspace_root.clone())
        .expect("create durable session");
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace_root)
        .expect("allow policy");
    let mut runtime = RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler)
        .expect("persistent runtime");
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "durable question".to_owned(),
        })
        .await
        .expect("submit durable turn");

    let mut saw_assistant = false;
    loop {
        let event = runtime.next_event().await.expect("runtime event");
        match event {
            AgentEvent::AssistantMessage { .. } => {
                let loaded = SessionStore::inspect(&session_path).expect("inspect live session");
                assert_eq!(loaded.records[0].session_id, session_id);
                assert!(matches!(
                    loaded.records[0].record,
                    crate::session::SessionRecord::SessionCreated { .. }
                ));
                let restored = reduce(&loaded.records).expect("reduce committed records");
                assert_eq!(
                    restored.conversation_path().expect("conversation path"),
                    vec![
                        Message::text(Role::User, "durable question"),
                        Message::text(Role::Assistant, "durable answer"),
                    ]
                );
                saw_assistant = true;
            }
            AgentEvent::OperationStateChanged {
                operation_id: actual,
                state: OperationState::Finished(OperationOutcome::Completed),
            } if actual == operation_id => break,
            _ => {}
        }
    }
    assert!(saw_assistant);
}

#[tokio::test]
async fn clear_resets_runtime_history() {
    let (agent, requests, _) = queue_agent(
        vec![
            Ok(Message::text(Role::Assistant, "first answer")),
            Ok(Message::text(Role::Assistant, "fresh answer")),
        ],
        Vec::new(),
    );
    let mut runtime = spawn_runtime(agent);
    let first = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: first,
            input: "remember me".to_owned(),
        })
        .await
        .expect("first turn");
    receive_finished(&mut runtime, first).await;
    runtime
        .send(RuntimeCommand::ClearConversation)
        .await
        .expect("clear command");
    assert_eq!(
        runtime.next_event().await,
        Some(AgentEvent::ConversationCleared)
    );

    let second = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: second,
            input: "fresh start".to_owned(),
        })
        .await
        .expect("second turn");
    receive_finished(&mut runtime, second).await;

    let requests = requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests[1].len(), 2);
    assert_eq!(requests[1][1], Message::text(Role::User, "fresh start"));
}

#[tokio::test]
async fn active_root_turn_rejects_a_second_submission_and_shutdown_interrupts() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = make_agent(Box::new(BlockingTransport {
        started: Arc::clone(&started),
        release,
    }));
    let mut runtime = spawn_runtime(agent);
    let active = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: active,
            input: "wait".to_owned(),
        })
        .await
        .expect("active turn");
    assert!(matches!(
        runtime.next_event().await,
        Some(AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Running,
        }) if operation_id == active
    ));
    started.notified().await;

    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "too soon".to_owned(),
        })
        .await
        .expect("second command transport");
    assert!(matches!(
        runtime.next_event().await,
        Some(AgentEvent::CommandRejected { reason }) if reason.contains("already active")
    ));
    runtime
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("shutdown command");
    assert_eq!(
        runtime.next_event().await,
        Some(AgentEvent::OperationStateChanged {
            operation_id: active,
            state: OperationState::Finished(OperationOutcome::Interrupted),
        })
    );
}

#[tokio::test]
async fn deltas_keep_operation_and_step_identity() {
    let (agent, _, _) = queue_agent(
        vec![Ok(Message::text(Role::Assistant, "hello"))],
        vec!["hel".to_owned(), "lo".to_owned()],
    );
    let mut runtime = spawn_runtime(agent);
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "hello".to_owned(),
        })
        .await
        .expect("turn");
    let running = runtime.next_event().await.expect("running");
    let first = runtime.next_event().await.expect("first delta");
    let second = runtime.next_event().await.expect("second delta");

    assert!(
        matches!(running, AgentEvent::OperationStateChanged { operation_id: actual, state: OperationState::Running } if actual == operation_id)
    );
    let (first_step, second_step) = match (first, second) {
        (
            AgentEvent::AssistantTextDelta {
                operation_id: first_operation,
                step_id: first_step,
                text: first_text,
            },
            AgentEvent::AssistantTextDelta {
                operation_id: second_operation,
                step_id: second_step,
                text: second_text,
            },
        ) => {
            assert_eq!(first_operation, operation_id);
            assert_eq!(second_operation, operation_id);
            assert_eq!(first_text, "hel");
            assert_eq!(second_text, "lo");
            (first_step, second_step)
        }
        events => panic!("unexpected delta events: {events:?}"),
    };
    assert_eq!(first_step, second_step);
    assert_eq!(
        receive_finished(&mut runtime, operation_id).await,
        OperationOutcome::Completed
    );
}

#[tokio::test]
async fn dropped_event_receiver_does_not_fail_operation() {
    let (agent, _, completed) = queue_agent(
        vec![Ok(Message::text(Role::Assistant, "still completes"))],
        vec!["still ".to_owned(), "completes".to_owned()],
    );
    let RuntimeHandle { commands, events } = spawn_runtime(agent);
    drop(events);
    commands
        .send(RuntimeCommand::SubmitTurn {
            operation_id: OperationId::new(),
            input: "continue without observer".to_owned(),
        })
        .await
        .expect("runtime accepts command");
    while !completed.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    commands
        .send(RuntimeCommand::Shutdown)
        .await
        .expect("runtime remains available after passive event loss");
}

#[tokio::test]
async fn failures_always_end_with_a_terminal_failed_state() {
    let (agent, _, _) = queue_agent(vec![Err("provider failed".to_owned())], Vec::new());
    let mut runtime = spawn_runtime(agent);
    let operation_id = OperationId::new();
    runtime
        .send(RuntimeCommand::SubmitTurn {
            operation_id,
            input: "fail".to_owned(),
        })
        .await
        .expect("turn");
    assert_eq!(
        receive_finished(&mut runtime, operation_id).await,
        OperationOutcome::Failed
    );
}

#[test]
fn events_are_passive_observations_not_commands() {
    fn observe(_event: AgentEvent) {}
    fn command(_command: RuntimeCommand) {}

    observe(AgentEvent::CommandRejected {
        reason: "observation only".to_owned(),
    });
    command(RuntimeCommand::ClearConversation);

    let result = Message::tool_result(ToolResult::error("call", "failed"));
    assert!(matches!(
        result.content.as_slice(),
        [ContentBlock::ToolResult(_)]
    ));
}

struct RuntimeCrashObserver {
    target: CrashSite,
    path: std::path::PathBuf,
    snapshot: Mutex<Option<Vec<crate::session::SessionRecord>>>,
}

impl BoundaryObserver for RuntimeCrashObserver {
    fn reached(&self, site: CrashSite) -> anyhow::Result<()> {
        if site == self.target {
            let loaded = SessionStore::inspect(&self.path)?;
            *self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(
                loaded
                    .records
                    .into_iter()
                    .map(|record| record.record)
                    .collect(),
            );
            anyhow::bail!("injected runtime crash at {site:?}");
        }
        Ok(())
    }
}

#[tokio::test]
async fn runtime_crash_sites_commit_acceptance_step_and_conversation_in_order() {
    for site in [
        CrashSite::AfterOperationAccepted,
        CrashSite::AfterStepStarted,
        CrashSite::AfterConversationResult,
    ] {
        let data = tempdir().expect("Xana data tempdir");
        let workspace = tempdir().expect("workspace tempdir");
        std::fs::write(workspace.path().join("note.txt"), "durable bytes")
            .expect("write readable fixture");
        let workspace_root = workspace.path().canonicalize().expect("workspace root");
        let session = DurableSession::create(data.path(), workspace_root.clone())
            .expect("create durable session");
        let path = session.path().to_owned();
        let observer = Arc::new(RuntimeCrashObserver {
            target: site,
            path,
            snapshot: Mutex::new(None),
        });
        let response = if site == CrashSite::AfterOperationAccepted {
            Message::text(Role::Assistant, "unreachable")
        } else {
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                    id: "call-read".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "note.txt"}),
                })],
            }
        };
        let provider = QueueTransport {
            responses: Mutex::new(vec![Ok(response)].into()),
            requests: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(AtomicBool::new(false)),
            deltas: Vec::new(),
        };
        let tools = ToolRegistry::builtins_for_tests().expect("builtin tools");
        let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
        let assembler = PromptAssembler::new(
            definitions,
            PromptEnvironment {
                operating_system: "test".to_owned(),
                working_directory: workspace_root.clone(),
                configured_shell: "test shell".to_owned(),
                surface: PromptSurface::Cli,
            },
            None,
            ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        );
        let prompt = assembler.assemble(&[]).expect("base prompt");
        let agent = Agent::new(Box::new(provider), tools, workspace_root.clone(), prompt, 2)
            .with_boundary_observer(observer.clone());
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &workspace_root)
            .expect("allow policy");
        let mut runtime = RuntimeHandle::spawn_persistent(agent, policy, true, session, assembler)
            .expect("persistent runtime");
        let operation_id = OperationId::new();
        runtime
            .send(RuntimeCommand::SubmitTurn {
                operation_id,
                input: "exercise crash boundary".to_owned(),
            })
            .await
            .expect("submit turn");

        if site == CrashSite::AfterOperationAccepted {
            loop {
                if matches!(
                    runtime.next_event().await,
                    Some(AgentEvent::CommandRejected { .. })
                ) {
                    break;
                }
            }
        } else {
            assert_eq!(
                receive_finished(&mut runtime, operation_id).await,
                OperationOutcome::Failed
            );
        }

        let records = observer
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("runtime crash prefix");
        let has_accepted = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::OperationAccepted {
                    operation_id: actual,
                    ..
                } if *actual == operation_id
            )
        });
        let has_step = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::StepStarted {
                    operation_id: actual,
                    ..
                } if *actual == operation_id
            )
        });
        let has_result = records.iter().any(|record| {
            matches!(
                record,
                crate::session::SessionRecord::InvocationResultAppended { result }
                    if result.operation_id == operation_id
            )
        });
        assert!(has_accepted, "{site:?}");
        assert_eq!(
            has_step,
            site != CrashSite::AfterOperationAccepted,
            "{site:?}"
        );
        assert_eq!(
            has_result,
            site == CrashSite::AfterConversationResult,
            "{site:?}"
        );
    }
}
