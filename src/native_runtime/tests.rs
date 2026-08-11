use super::*;
use crate::{
    config::{OrchestrationLimits, PermissionMode, ProviderKind},
    context::ContextBudget,
    identity::{OrchestrationPlanId, StepId, ToolInvocationId},
    message::{ContentBlock, Role, ToolCall, ToolResult},
    model::{DescriptorSource, ModelDescriptor},
    operation::{BoundaryObserver, CrashSite},
    orchestration::{
        AwaitAgentOptions, AwaitAgentOutcome, ChildAttribution, ChildCommitSender, ChildExecution,
        ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutcome, ChildExecutionOutput,
        ChildLifecycle, ChildReport, ChildResultSchema, ChildSupervisor, ChildTerminalStatus,
        ChildUsage, CollectAgentsOptions, CollectionEntryState, CollectionFailurePolicy,
        EnforcementCapabilities, ExecutionOwner, ManagedCodexChildExecution, ManagedCodexChildSpec,
        ManagedCodexRunner, OrchestrationBudget, OrchestrationPlan, OrchestrationPlanStep,
        OrchestrationPlanStepResult, ParentExecution, PlanHandleRef, PreparedChild,
        ResolvedAgentConfig, SpawnAgentRequest, apply_spawn_restrictions, child_policy,
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

struct BlockingTransport {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct ImmediateChildFactory {
    workspace: std::path::PathBuf,
    requests: Arc<Mutex<Vec<SpawnAgentRequest>>>,
}

struct ImmediateChildExecution;

struct SinglePrepareChildFactory {
    workspace: std::path::PathBuf,
    calls: Arc<AtomicUsize>,
}

struct BarrierChildFactory {
    workspace: std::path::PathBuf,
    started: Arc<Notify>,
    release: Arc<Notify>,
    dropped: Option<Arc<AtomicUsize>>,
}

struct BarrierChildExecution {
    started: Arc<Notify>,
    release: Arc<Notify>,
    dropped: Option<Arc<AtomicUsize>>,
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

struct ActivityFloodChildFactory {
    workspace: std::path::PathBuf,
}

struct ActivityFloodChildExecution;

struct SupervisorManagedFactory {
    workspace: std::path::PathBuf,
    runner: Arc<dyn ManagedCodexRunner>,
}

#[derive(Default)]
struct SupervisorManagedRunner {
    started: Arc<Notify>,
    interruptions: Arc<AtomicUsize>,
}

impl ManagedCodexRunner for SupervisorManagedRunner {
    fn run(
        &self,
        spec: ManagedCodexChildSpec,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        self.started.notify_one();
        let cancellation = context.cancellation.clone();
        let interruptions = Arc::clone(&self.interruptions);
        Box::pin(async move {
            let _ = context.events.send(AgentEvent::ChildActivity {
                attribution: context.attribution.clone(),
                activity: crate::orchestration::ChildActivity::ManagedRuntime {
                    notification: crate::managed::codex::ManagedNotification::ThreadStarted {
                        thread_id: "thread-fake".to_owned(),
                    },
                },
            });
            if matches!(
                spec.task.as_str(),
                "cancel" | "interrupt-rejected" | "completion-wins"
            ) {
                cancellation.cancelled().await;
                interruptions.fetch_add(1, Ordering::SeqCst);
                return match spec.task.as_str() {
                    "cancel" => ChildExecutionOutcome::Cancelled(
                        "fake Codex interrupt acknowledged".to_owned(),
                    ),
                    "interrupt-rejected" => ChildExecutionOutcome::Failed(
                        "Codex app-server error -32601: method not found".to_owned(),
                    ),
                    "completion-wins" => ChildExecutionOutcome::Completed(ChildExecutionOutput {
                        text: "Codex completed before interruption took effect".to_owned(),
                        usage: ChildUsage::Unknown,
                    }),
                    _ => unreachable!("matched managed cancellation fixture"),
                };
            }
            let _ = context.events.send(AgentEvent::ChildActivity {
                attribution: context.attribution,
                activity: crate::orchestration::ChildActivity::ManagedRuntime {
                    notification: crate::managed::codex::ManagedNotification::TokenUsageUpdated {
                        thread_id: "thread-fake".to_owned(),
                        turn_id: "turn-fake".to_owned(),
                        input_tokens: 20,
                        output_tokens: 5,
                        total_tokens: 25,
                    },
                },
            });
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "managed child result".to_owned(),
                usage: ChildUsage::Measured {
                    input_tokens: Some(20),
                    output_tokens: Some(5),
                    total_tokens: Some(25),
                    requests: 1,
                    spend_microusd: None,
                },
            })
        })
    }
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

impl ChildExecutionFactory for SinglePrepareChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            return Err("the same plan child was prepared more than once".to_owned());
        }
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
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
                dropped: self.dropped.clone(),
            }),
        ))
    }
}

impl Drop for BarrierChildExecution {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped {
            dropped.fetch_add(1, Ordering::SeqCst);
        }
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

impl ChildExecutionFactory for ActivityFloodChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            restricted_scripted_child_config(request)?,
            policy,
            Box::new(ActivityFloodChildExecution),
        ))
    }
}

impl ChildExecutionFactory for SupervisorManagedFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let mut resolved = restricted_scripted_child_config(request)?;
        resolved.owner = ExecutionOwner::Codex;
        resolved.provider_kind = ProviderKind::Codex;
        resolved.connection = "codex-test".to_owned();
        resolved.model.id = "gpt-managed".to_owned();
        resolved.model.display_name = "GPT Managed".to_owned();
        resolved.capabilities =
            crate::capability::AgentCapabilitySnapshot::new(BTreeSet::new(), BTreeSet::new());
        let policy = PermissionPolicy::new(PolicyDecision::Ask, Vec::new(), &self.workspace)
            .map_err(|error| error.to_string())?;
        Ok(PreparedChild::new(
            resolved.clone(),
            policy,
            Box::new(ManagedCodexChildExecution::new(
                Arc::clone(&self.runner),
                ManagedCodexChildSpec {
                    launch: crate::managed::codex::CodexLaunchConfig {
                        program: "fake-codex".to_owned(),
                        home: None,
                    },
                    workspace: self.workspace.clone(),
                    model: resolved.model.id,
                    options: crate::managed::codex::ManagedTurnOptions {
                        reasoning_effort: None,
                        reasoning_summary: None,
                    },
                    task: request.task.clone(),
                    policy: child_policy(resolved.permission_mode).expect("managed test policy"),
                },
            )),
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

impl ChildExecution for ActivityFloodChildExecution {
    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, ChildExecutionOutcome> {
        Box::pin(async move {
            for _ in 0..5_000 {
                let _ = context.events.send(AgentEvent::AssistantTextDelta {
                    operation_id: context.operation_id,
                    step_id: StepId::new(),
                    text: "x".to_owned(),
                });
            }
            let _ = context.events.send(AgentEvent::PermissionRequested {
                request: PermissionRequest {
                    operation_id: context.operation_id,
                    invocation_id: ToolInvocationId::new(),
                    tool_name: "critical_after_flood".to_owned(),
                    effect_class: crate::tool::EffectClass::Execute,
                    final_arguments: serde_json::json!({"action":"test"}),
                    scope: PermissionScope::Unscoped,
                },
            });
            ChildExecutionOutcome::Completed(ChildExecutionOutput {
                text: "bounded activity report".to_owned(),
                usage: ChildUsage::Unknown,
            })
        })
    }
}

fn scripted_child_config(request: &SpawnAgentRequest) -> ResolvedAgentConfig {
    let route = request.route.clone().unwrap_or_else(|| "worker".to_owned());
    let (connection, model) = match route.as_str() {
        "alpha" => ("native-a", "model-a"),
        "beta" => ("native-b", "model-b"),
        _ => ("scripted", "child-model"),
    };
    ResolvedAgentConfig {
        route,
        profile: "worker".to_owned(),
        connection: connection.to_owned(),
        provider_kind: ProviderKind::OpenAiCompat,
        owner: ExecutionOwner::Native,
        model: ModelDescriptor {
            id: model.to_owned(),
            display_name: model.to_owned(),
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
        capabilities: crate::capability::AgentCapabilitySnapshot::new(
            BTreeSet::new(),
            BTreeSet::new(),
        ),
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

mod child_lifecycle;
mod core;
mod delegation;
mod hardening;
mod plans;
