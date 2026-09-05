//! One structured background lane over the existing native runtime. New
//! permission questions have no controller; they fail closed into owner review.
use super::{Action, Job, JobState, RunOutcome, RunReceipt, TaskScope};
use crate::{
    agent::Agent,
    artifact::ArtifactStore,
    config::{ConnectionConfig, OutboundDataClass, XanaConfig},
    context::ContextBudget,
    identity::{OperationId, SessionId},
    message::ContentBlock,
    native_runtime::{AgentEvent, OperationOutcome, OperationState, RuntimeCommand, RuntimeHandle},
    paths::XanaPaths,
    permission::{PermissionPolicy, PolicyDecision},
    profile::{ProfileStore, ResolvedProfile},
    prompt::{PromptAssembler, PromptEnvironment, PromptSurface},
    session::DurableSession,
    shell::{Shell, ShellConfig},
    storage::ProtectedStore,
    tool::ToolRegistry,
    usage_budget::{DispatchFacts, UsageBudget},
    workspace_host::{ConversationRef, WorkspaceHost},
    workspace_identity::WorkspaceIdentity,
};
use anyhow::{Context, Result, ensure};
use futures::future::BoxFuture;
use std::{collections::BTreeSet, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) trait TaskExecutor {
    fn execute<'a>(
        &'a self,
        job: &'a Job,
        cancelled: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>>;
}

pub(crate) struct NativeExecutor {
    pub(crate) paths: XanaPaths,
    pub(crate) store: ProtectedStore,
}

/// Resolves an exact global Profile without model/catalog refresh. The digest
/// includes the credential *reference*, never its resolved value.
pub(crate) fn resolve_scope(
    paths: &XanaPaths,
    workspace: &Path,
    profile: &str,
    project: Option<uuid::Uuid>,
) -> Result<TaskScope> {
    let identity = WorkspaceIdentity::resolve(workspace)?;
    let registry = XanaConfig::load_registry_from(paths.config_file())?;
    let resolved = ProfileStore::open(paths).resolve_global(profile)?;
    ensure!(
        resolved.is_ready(),
        "Profile is not ready; inspect its owner controls"
    );
    let connection = registry
        .connections
        .get(&resolved.connection.value)
        .context("Profile connection missing")?;
    ensure!(
        connection.kind != crate::config::ProviderKind::Codex,
        "detached managed execution is not supported; select a native Profile"
    );
    ensure!(
        resolved.reasoning_effort.value.is_none() && resolved.reasoning_summary.value.is_none(),
        "detached route cannot yet enforce explicit reasoning options"
    );
    ensure!(
        resolved.skills.value.is_empty() && resolved.identity.value.is_none(),
        "detached task Profile must not require unimplemented identity/skill layers"
    );
    let endpoint = connection
        .base_url
        .clone()
        .context("native endpoint missing")?;
    let digest = configuration_digest(&resolved, connection, &registry)?;
    if let Some(id) = project {
        let inspection =
            crate::project::ProjectStore::open(paths)?.inspect(id.to_string().parse()?)?;
        ensure!(
            inspection.project.lifecycle == crate::private_state::ProjectLifecycle::Active
                && inspection.workspace_status == crate::project::WorkspaceStatus::Available,
            "Project unavailable or archived"
        );
        ensure!(
            identity.matches(&inspection.project.canonical_workspace)?,
            "Project workspace does not match task scope"
        );
    }
    Ok(TaskScope {
        workspace: identity.canonical_path().to_owned(),
        workspace_identity: identity.collision_key().to_owned(),
        project,
        profile: profile.into(),
        profile_id: resolved.profile_id,
        configuration_digest: digest,
        connection: resolved.connection.value,
        model: resolved.model.value,
        endpoint,
    })
}

fn configuration_digest(
    resolved: &ResolvedProfile,
    connection: &ConnectionConfig,
    registry: &crate::config::ConnectionRegistry,
) -> Result<String> {
    let bytes = serde_json::to_vec(
        &serde_json::json!({"profile":resolved,"endpoint":connection.base_url,"provider":format!("{:?}",connection.kind),"credential":connection.credential,"rules":registry.permission_rules,"context":registry.context}),
    )?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

impl TaskExecutor for NativeExecutor {
    fn execute<'a>(
        &'a self,
        job: &'a Job,
        cancelled: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>> {
        Box::pin(async move {
            ensure!(
                resolve_scope(
                    &self.paths,
                    &job.scope.workspace,
                    &job.scope.profile,
                    job.scope.project
                )? == job.scope,
                "task scope, Profile, route or permission policy changed; create a newly reviewed task"
            );
            if cancelled.is_cancelled() {
                return Ok((RunOutcome::Cancelled, "Cancelled before dispatch".into()));
            }
            if let Action::Reminder { text } = &job.action {
                return Ok((RunOutcome::Completed, text.clone()));
            }
            let Action::NativeTask {
                prompt,
                workspace_reads,
            } = &job.action
            else {
                unreachable!()
            };
            if crate::memory::parse_natural(prompt).is_some() {
                return Ok((RunOutcome::NeedsYou,"Local memory controls require direct owner input; this task grants no memory mutation authority".into()));
            }
            let registry = XanaConfig::load_registry_from(self.paths.config_file())?;
            let resolved = ProfileStore::open(&self.paths).resolve_global(&job.scope.profile)?;
            let connection = registry
                .connections
                .get(&job.scope.connection)
                .context("connection missing")?;
            ensure!(
                configuration_digest(&resolved, connection, &registry)?
                    == job.scope.configuration_digest,
                "resolved dispatch configuration changed after task review"
            );
            ensure!(
                resolved
                    .egress
                    .value
                    .contains(&OutboundDataClass::PromptText),
                "Profile denies prompt disclosure"
            );
            ensure!(
                !workspace_reads
                    || resolved
                        .egress
                        .value
                        .contains(&OutboundDataClass::SelectedFileContents),
                "Profile denies workspace-file disclosure"
            );
            let parts = compose_agent(
                job,
                &resolved,
                connection,
                registry.permission_rules,
                &self.store,
            )?;
            run_native(&self.store, job, cancelled, parts, || {
                Ok(resolve_scope(
                    &self.paths,
                    &job.scope.workspace,
                    &job.scope.profile,
                    job.scope.project,
                )? == job.scope)
            })
            .await
        })
    }
}

/// The production native owner boundary, also exercised with an in-memory fake
/// provider. Configuration composition remains outside this runtime lifecycle.
pub(super) async fn run_native(
    store: &ProtectedStore,
    job: &Job,
    cancelled: CancellationToken,
    parts: (Agent, PromptAssembler, PermissionPolicy),
    validate: impl Fn() -> Result<bool>,
) -> Result<(RunOutcome, String)> {
    let (agent, assembler, policy) = parts;
    let Action::NativeTask { prompt, .. } = &job.action else {
        anyhow::bail!("not a native task")
    };
    let session_id: SessionId = job.conversation.to_string().parse()?;
    let mut session = if store.history_exists(session_id)? {
        DurableSession::resume_protected(store.clone(), session_id)?.0
    } else {
        DurableSession::create_protected(store.clone(), job.scope.workspace.clone(), session_id)?
    };
    ensure!(
        session.workspace_root() == job.scope.workspace,
        "task Conversation workspace changed"
    );
    // Each evaluation sees its fixed task and current authorized sources,
    // not prior run output. Old Conversation entries remain inspectable.
    session.clear_conversation()?;
    let memory = crate::memory::MemoryOwner::new(
        store.clone(),
        crate::memory::MemoryContext {
            conversation: Some(job.conversation),
            profile: Some(job.scope.profile_id),
            project: job.scope.project,
        },
    );
    let runtime = RuntimeHandle::spawn_persistent_background(
        agent,
        policy,
        session,
        assembler,
        Some(memory),
    )?;
    let (runtime, mut events, _, _) = runtime.into_frontend_parts();
    let result=async {
            // Revalidate after constructing every I/O owner, immediately before
            // handing the accepted turn to the runtime.
            let current=store.autonomy_job(job.id)?;
            let policy=store.autonomy_policy()?;
            let unchanged=validate()?;
            if current.occurrence!=job.occurrence || current.state!=JobState::Running || !current.authorized || policy.stop_requested || !policy.detached_enabled || !unchanged || cancelled.is_cancelled() {
                return Ok((RunOutcome::NeedsYou,"Authority changed before native dispatch".into()));
            }
            let operation_id:OperationId=job.occurrence.context("missing occurrence")?.to_string().parse()?;
            runtime.send(RuntimeCommand::SubmitTurn { operation_id,input:prompt.clone() }).await?;
            let mut detail=String::new();
            let mut permission_denied=false;
            let outcome=loop {
                tokio::select! {
                    biased;
                    _=cancelled.cancelled()=> {break RunOutcome::Unknown;}
                    event=events.recv()=>match event {
                        Some(AgentEvent::PermissionAudited { fact }) if fact.effective!=PolicyDecision::Allow => permission_denied=true,
                        Some(AgentEvent::AssistantMessage { message,.. })=> {
                            for block in message.content {if let ContentBlock::Text(text)=block {append_bounded(&mut detail,&text,16*1024);}}
                        }
                        Some(AgentEvent::RoundBudgetReached { .. })=>break RunOutcome::NeedsYou,
                        Some(AgentEvent::CommandRejected { .. })=>break RunOutcome::NeedsYou,
                        Some(AgentEvent::OperationStateChanged { state:OperationState::Finished(state),.. })=>break if permission_denied {RunOutcome::NeedsYou} else if state==OperationOutcome::Completed {RunOutcome::Completed} else {RunOutcome::Unknown},
                        None=>break RunOutcome::Unknown,
                        _=>{}
                    }
                }
            };
            Ok(match outcome {
                RunOutcome::Completed=>(outcome,detail),
                RunOutcome::NeedsYou=>(outcome,"Native task requires a permission, budget or owner decision. Inspect its task-owned Conversation; no continuation was authorized".into()),
                _=>(RunOutcome::Unknown,"Native dispatch may have consumed tokens or completed. Cancellation is not proof of non-execution; inspect the task-owned Conversation".into()),
            })
            }.await;
    // Every post-spawn error still joins the owned runtime before the
    // caller can release its workspace and background leases.
    let stopped = runtime.shutdown_owned().await;
    ensure!(stopped, "native runtime shutdown was not acknowledged");
    result
}

fn compose_agent(
    job: &Job,
    resolved: &ResolvedProfile,
    connection: &ConnectionConfig,
    rules: Vec<crate::permission::PermissionRule>,
    store: &ProtectedStore,
) -> Result<(Agent, PromptAssembler, PermissionPolicy)> {
    let Action::NativeTask {
        workspace_reads, ..
    } = job.action
    else {
        anyhow::bail!("not a native task")
    };
    let snapshot = crate::capability::resolve_builtin_capability_snapshot(
        resolved.capabilities.value.as_deref(),
    )?;
    let reads = [
        "read_file",
        "list_files",
        "find_files",
        "grep_files",
        "read_document",
        "xana_docs",
    ];
    let names = snapshot
        .tool_ids()
        .iter()
        .map(ToString::to_string)
        .filter(|name| workspace_reads && reads.contains(&name.as_str()))
        .collect::<BTreeSet<_>>();
    let tools = ToolRegistry::builtins_from_names(Shell::resolve(ShellConfig::default())?, &names)?;
    let assembler = PromptAssembler::new(
        tools.definitions().into_iter().cloned().collect(),
        PromptEnvironment {
            connection: job.scope.connection.clone(),
            model: job.scope.model.clone(),
            operating_system: std::env::consts::OS.into(),
            working_directory: job.scope.workspace.clone(),
            configured_shell: "not exposed to this task".into(),
            surface: PromptSurface::Cli,
        },
        None,
        ContextBudget {
            total_tokens: 6144,
            conversation_reserve_tokens: 1024,
        },
    );
    let prompt = assembler.assemble(&[])?;
    let (provider, _) = crate::orchestration::compose_native_provider(
        connection,
        &job.scope.model,
        ArtifactStore::protected(store.clone()),
        true,
    )
    .map_err(anyhow::Error::msg)?;
    let budget = UsageBudget::new(
        store.clone(),
        job.conversation.to_string(),
        format!("scheduled/{}", job.id),
        2048,
    )
    .background(job.occurrence.context("missing occurrence")?.to_string())
    .with_facts(DispatchFacts {
        project: job.scope.project.map(|id| id.to_string()),
        profile: Some(job.scope.profile_id.to_string()),
        owner: Some("native".into()),
        connection: Some(job.scope.connection.clone()),
        model: Some(job.scope.model.clone()),
        reasoning: None,
    });
    let policy = PermissionPolicy::new(
        resolved.permission_mode.value.into(),
        rules,
        &job.scope.workspace,
    )?
    .workspace_reads_only();
    Ok((
        Agent::new(
            provider,
            tools,
            job.scope.workspace.clone(),
            prompt,
            resolved.max_tool_rounds.value.min(4),
        )
        .with_usage_budget(Some(budget)),
        assembler,
        policy,
    ))
}

/// One clock tick; the caller owns the single same-home host lease. A busy
/// foreground workspace defers before paid dispatch and is never preempted.
pub(crate) async fn tick(
    store: &ProtectedStore,
    executor: &dyn TaskExecutor,
    clock: &dyn Fn() -> Result<i64>,
    shutdown: &CancellationToken,
) -> Result<Option<Job>> {
    let now = clock()?;
    let policy = store.autonomy_policy()?;
    if policy.stop_requested || !policy.detached_enabled || shutdown.is_cancelled() {
        return Ok(None);
    }
    let Some(background) = store.background_lease()? else {
        return Ok(None);
    };
    if background.foreground_active()? {
        return Ok(None);
    }
    let Some(job) = store.autonomy_claim(now)? else {
        return Ok(None);
    };
    let occurrence = job.occurrence.context("missing claimed occurrence")?;
    let host = WorkspaceHost::open_protected(store.clone(), &job.scope.workspace)?;
    let session_id = job.conversation.to_string().parse()?;
    let _lease = match host.acquire_background_root(ConversationRef::Native { session_id }) {
        Ok(lease) => lease,
        Err(crate::workspace_host::WorkspaceHostError::Busy(_)) => {
            store.autonomy_defer(job.id, occurrence, now.saturating_add(30))?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let cancel = shutdown.child_token();
    let deadline = tokio::time::sleep(Duration::from_secs(job.budget.run_seconds));
    tokio::pin!(deadline);
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    let execution = executor.execute(&job, cancel.clone());
    tokio::pin!(execution);
    let mut cancel_started = false;
    let result = loop {
        tokio::select! {
            result=&mut execution=>break result,
            _=&mut deadline,if !cancel_started=>{cancel_started=true;cancel.cancel();}
            _=shutdown.cancelled(),if !cancel_started=>{cancel_started=true;cancel.cancel();}
            _=background.preempted(),if !cancel_started=>{cancel_started=true;cancel.cancel();}
            _=poll.tick()=>{
                let can_continue=store.autonomy_job(job.id).and_then(|current| {
                    let policy=store.autonomy_policy()?;
                    Ok(current.state!=JobState::CancelRequested && current.authorized && current.expires_at>clock()? && !policy.stop_requested && policy.detached_enabled)
                }).unwrap_or(false);
                if !can_continue {cancel_started=true;cancel.cancel();}
            }
        }
    };
    let (outcome,detail)=result.unwrap_or_else(|_|(RunOutcome::Unknown,"Task validation or runtime failed without a conclusive terminal receipt. Inspect scope, credentials, permissions, budgets and the task-owned Conversation before retrying".into()));
    let receipt = RunReceipt {
        occurrence,
        scheduled_at: job.next.at,
        finished_at: clock()?,
        outcome,
        detail,
        coalesced: now > job.next.at.saturating_add(60),
        dst_adjusted: job.next.dst_adjusted,
    };
    Ok(Some(store.autonomy_finish(job.id, receipt)?))
}

fn append_bounded(target: &mut String, text: &str, maximum: usize) {
    let mut end = text.len().min(maximum.saturating_sub(target.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}
