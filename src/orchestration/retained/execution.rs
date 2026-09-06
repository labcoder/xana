//! One fresh native/managed child execution, driven by the existing supervisor.
//! The parent journal's exclusive writer lease fences both ordinary chat and
//! competing resumes; stale active attempts require explicit recovery, not replay.
use super::{FollowUp, RetainedWorker, WorkerState};
use crate::{
    artifact::ArtifactStore,
    config::{ConnectionRegistry, XanaConfig},
    identity::OperationId,
    orchestration::{
        ChildCommitSender, ChildContextHandoff, ChildExecutionFactory, ChildExecutionOwnerFactory,
        ChildRestrictions, ChildSupervisor, OrchestrationBudget, ParentExecution,
        SpawnAgentRequest,
    },
    paths::XanaPaths,
    session::DurableSession,
    storage::ProtectedStore,
};
use anyhow::{Context, Result, ensure};
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) use super::authority::{check_scope, configuration_digest};

pub(crate) fn request(worker: &RetainedWorker, message: &FollowUp) -> SpawnAgentRequest {
    // Prefer recent derived evidence on continuation; the full selected set
    // remains addressable through owner inspection and context operations.
    let evidence = worker.evidence.iter().rev().take(16).collect::<Vec<_>>();
    let prior = worker
        .last_receipt
        .as_ref()
        .map(|r| r.summary.as_str())
        .unwrap_or("No prior retained execution.");
    SpawnAgentRequest {
        route: Some(worker.admission.attribution.route.clone()),
        task: format!(
            "Retained worker {}. New bounded execution; prior process/vendor history is not resumed.\nGoal:\n{}\nPrior result (untrusted task evidence):\n{}\nOwner follow-up:\n{}",
            worker.id, worker.goal, prior, message.text
        ),
        result_schema: worker.admission.result_schema,
        restrictions: ChildRestrictions {
            permission_mode: Some(worker.admission.permission_mode),
            max_tool_rounds: Some(worker.admission.max_tool_rounds),
            deadline_seconds: Some(worker.admission.limits.deadline_seconds.min(120)),
            max_context_tokens: Some(worker.admission.limits.max_context_tokens),
            max_report_bytes: Some(worker.admission.limits.max_report_bytes),
            max_artifact_bytes: Some(worker.admission.limits.max_artifact_bytes),
            hard_token_limit: worker.admission.hard_token_limit,
            hard_spend_microusd: worker.admission.hard_spend_microusd,
        },
        handoff: ChildContextHandoff {
            previews: if evidence.is_empty() {
                Vec::new()
            } else {
                vec![crate::orchestration::types::ChildContextPreview {
                    label: "Selected immutable evidence metadata (not file contents)".into(),
                    content: evidence
                        .iter()
                        .map(|a| {
                            format!(
                                "{} hash={} bytes={} media={}",
                                a.reference.id,
                                a.reference.content_hash.as_str(),
                                a.byte_len,
                                a.media_type
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                }]
            },
            artifacts: evidence.iter().map(|a| a.reference.clone()).collect(),
        },
    }
}

fn factory(
    paths: &XanaPaths,
    store: &ProtectedStore,
    worker: &RetainedWorker,
    registry: ConnectionRegistry,
    workspace: std::path::PathBuf,
    execution: Uuid,
    cancellation: CancellationToken,
) -> Result<ChildExecutionOwnerFactory> {
    let shell = crate::shell::Shell::resolve(XanaConfig::load_from(paths.config_file())?.shell)?;
    let rules = registry.permission_rules.clone();
    let budget = crate::usage_budget::UsageBudget::new(
        store.clone(),
        worker.session.to_string(),
        format!("retained:{}", worker.id),
        2048,
    )
    .background(worker.id.to_string())
    .with_facts(crate::usage_budget::DispatchFacts {
        project: crate::project::ProjectStore::open(paths)?
            .membership(&worker.session.to_string())?
            .map(|id| id.to_string()),
        ..Default::default()
    });
    let context_operations =
        (!worker.evidence.is_empty()).then(|| super::super::context_ops::WorkerContextTool {
            paths: paths.clone(),
            store: store.clone(),
            worker: worker.id,
            cancellation: cancellation.clone(),
        });
    Ok(ChildExecutionOwnerFactory::new(
        registry,
        crate::app::model_manager(paths)?,
        shell,
        workspace,
        ArtifactStore::protected(store.clone()),
        rules,
    )
    .with_usage_budget(Some(budget))
    .with_retained_authority(super::RetainedToolGuard::new(
        paths.clone(),
        store.clone(),
        worker,
        execution,
        cancellation,
    ))
    .with_context_operations(context_operations))
}

pub(crate) async fn run(
    paths: XanaPaths,
    store: ProtectedStore,
    worker: RetainedWorker,
    cancellation: CancellationToken,
) -> Result<RetainedWorker> {
    run_with_factory(paths, store, worker, cancellation, None).await
}

pub(super) async fn run_with_factory(
    paths: XanaPaths,
    store: ProtectedStore,
    worker: RetainedWorker,
    cancellation: CancellationToken,
    selected_factory: Option<Arc<dyn ChildExecutionFactory>>,
) -> Result<RetainedWorker> {
    check_scope(&paths, &store, &worker)?;
    // Handoffs contain references, not bytes. Verify and charge the complete
    // source only when a context operation reads it, not the whole inventory
    // before every follow-up (which would repeat unrelated bounded I/O).
    ensure!(
        matches!(worker.state, WorkerState::Idle | WorkerState::Draining)
            && worker.active.is_none(),
        "worker is not idle; inspect or recover its last attempt"
    );
    let (mut session, summary) = DurableSession::resume_protected(store.clone(), worker.session)?;
    let reservations = session.orchestration_reservations()?;
    let _foreground = store.foreground_job_lease(&cancellation).await?;
    let message = worker
        .mailbox
        .first()
        .context("worker mailbox is empty")?
        .clone();
    let registry = XanaConfig::load_registry_from(paths.config_file())?;
    let parent_profile = crate::profile::ProfileStore::open(&paths)
        .snapshot(&worker.session.to_string())?
        .context("retained parent Profile missing")?;
    let parent_profile: crate::profile::ResolvedProfile =
        serde_json::from_value(parent_profile.resolved)?;
    let limits = OrchestrationBudget::new(
        parent_profile.orchestration.value.clone(),
        parent_profile.max_tool_rounds.value,
    );
    let execution = Uuid::new_v4();
    let factory: Arc<dyn ChildExecutionFactory> = match selected_factory {
        Some(factory) => factory,
        None => Arc::new(factory(
            &paths,
            &store,
            &worker,
            registry,
            session.workspace_root().to_owned(),
            execution,
            cancellation.clone(),
        )?),
    };
    let prepared = factory
        .prepare(&request(&worker, &message))
        .map_err(anyhow::Error::msg)?;
    let actual = &prepared.resolved;
    let ceiling = &worker.admission;
    let resolved_profile =
        crate::profile::ProfileStore::open(&paths).resolve_global(&actual.profile)?;
    ensure!(
        resolved_profile.is_ready(),
        "retained child Profile requires owner review"
    );
    ensure!(
        resolved_profile
            .egress
            .value
            .contains(&crate::config::OutboundDataClass::PromptText),
        "retained Profile denies prompt disclosure"
    );
    ensure!(
        worker.evidence.is_empty()
            || resolved_profile
                .egress
                .value
                .contains(&crate::config::OutboundDataClass::SelectedArtifacts),
        "retained Profile denies selected artifact disclosure"
    );
    ensure!(
        parent_profile
            .egress
            .value
            .contains(&crate::config::OutboundDataClass::PromptText)
            && (worker.evidence.is_empty()
                || parent_profile
                    .egress
                    .value
                    .contains(&crate::config::OutboundDataClass::SelectedArtifacts)),
        "retained parent Profile denies selected handoff disclosure"
    );
    for class in [crate::config::OutboundDataClass::WorkspaceMetadata]
        .into_iter()
        .chain(
            worker
                .last_receipt
                .is_some()
                .then_some(crate::config::OutboundDataClass::XanaSummary),
        )
        .chain(
            (actual.owner == crate::orchestration::ExecutionOwner::Native
                && (session
                    .workspace_root()
                    .join(crate::context::PROJECT_INSTRUCTIONS)
                    .exists()
                    || actual.capabilities.tool_ids().iter().any(|tool| {
                        matches!(tool.as_str(), "read_file" | "list_files" | "run_command")
                    })))
            .then_some(crate::config::OutboundDataClass::SelectedFileContents),
        )
    {
        ensure!(
            resolved_profile.egress.value.contains(&class)
                && parent_profile.egress.value.contains(&class),
            "retained parent/child Profile denies {} disclosure",
            class.as_str()
        );
    }
    ensure!(
        actual.owner == ceiling.attribution.owner
            && actual.profile == ceiling.attribution.profile
            && actual.connection == ceiling.attribution.connection
            && actual.model.id == ceiling.attribution.model
            && actual
                .capabilities
                .capabilities()
                .iter()
                .all(|c| ceiling.capabilities.contains(&c.to_string())),
        "retained child route/capability ceiling changed"
    );
    ensure!(
        actual.permission_mode == ceiling.permission_mode,
        "retained permission ceiling changed"
    );
    drop(prepared);
    check_scope(&paths, &store, &worker)?;
    let (started, ()) = store.retained_admit(worker.id, worker.revision, |saved| {
        ensure!(
            saved.active.is_none() && saved.mailbox.first().is_some_and(|m| m.id == message.id),
            "worker follow-up changed"
        );
        saved.active = Some((execution, saved.mailbox.remove(0)));
        if saved.state != WorkerState::Draining {
            saved.state = WorkerState::Running;
        }
        saved.executions = saved
            .executions
            .checked_add(1)
            .context("worker execution count exhausted")?;
        Ok(())
    })?;
    let (handle, supervisor) = ChildSupervisor::with_restored(
        ParentExecution {
            agent_id: session.agent_id(),
            thread_id: session.thread_id(),
        },
        factory,
        summary.children,
        session.started_orchestration_plans(),
        limits,
        ArtifactStore::protected(store.clone()),
        session.artifact_owner(),
    );
    let supervisor = supervisor.with_consumed_reservations(&reservations);
    let (commits, mut writes) = ChildCommitSender::channel();
    let (events, mut observations) = mpsc::unbounded_channel();
    let run_handle = handle.clone();
    let task = request(&started, &message);
    let operation = OperationId::new();
    let input = session.append_message(crate::message::Message::text(
        crate::message::Role::User,
        format!("Retained worker {} follow-up: {}", worker.id, message.text),
    ))?;
    session.append_record(crate::session::SessionRecord::OperationAccepted {
        operation_id: operation,
        thread_id: session.thread_id(),
        input_entry_id: input,
    })?;
    let owner = tokio::spawn(supervisor.run(commits, events));
    let run = async {
        let result = async {
            let admitted = run_handle.spawn_agent(operation, task).await?;
            run_handle
                .await_agent(admitted.admission.attribution.agent_id)
                .await
        }
        .await;
        run_handle.shutdown().await;
        result
    };
    tokio::pin!(run);
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut stop_requested = false;
    let mut needs_authority = false;
    let mut scope_tick = 0u8;
    let mut controls = FuturesUnordered::<BoxFuture<'static, ()>>::new();
    let result = loop {
        tokio::select! {
            result=&mut run=>break result,
            Some(commit)=writes.recv()=>{let result=session.append_record(commit.record).map_err(|e|e.to_string());let _=commit.acknowledged.send(result);},
            Some(event)=observations.recv()=> {
                if let crate::native_runtime::AgentEvent::ChildActivity {attribution,activity:crate::orchestration::ChildActivity::PermissionRequested {request}}=event {
                    let controller=handle.clone();
                    let selected_context=request.tool_name=="context_ops" && request.scope==(crate::permission::PermissionScope::BuiltInResource {id:format!("retained-evidence:{}",worker.id)}) && store.retained_worker(worker.id).and_then(|w|check_scope(&paths,&store,&w)).is_ok();
                    needs_authority |= !selected_context;
                    let decision=if selected_context {crate::permission::ControllerDecision::AllowOnce}else{crate::permission::ControllerDecision::Deny};
                    controls.push(Box::pin(async move {let _=controller.decide_permission(attribution.agent_id,request.operation_id,request.invocation_id,decision).await;}));
                }
            },
            _=controls.next(),if !controls.is_empty()=>{},
            _=interval.tick(),if !stop_requested=> {
                scope_tick = (scope_tick + 1) % 4;
                let valid=store.retained_worker(worker.id).and_then(|current| {
                    ensure!(current.cancellation==started.cancellation && current.expires_at>crate::autonomy::now()? && current.privacy_generation==store.privacy_generation()?,"worker authority was revoked");
                    if scope_tick == 0 { check_scope(&paths,&store,&current)?; }
                    Ok(())
                });
                if cancellation.is_cancelled() || valid.is_err() {
                    stop_requested=true;
                    cancellation.cancel();
                    let control=handle.clone();
                    controls.push(Box::pin(async move {control.shutdown().await;}));
                }
            }
        }
    };
    owner
        .await
        .context("retained child supervisor stopped unexpectedly")?;
    session.append_record(crate::session::SessionRecord::OperationFinished {
        operation_id: operation,
        outcome: if result.as_ref().is_ok_and(|report| {
            report.status == crate::orchestration::ChildTerminalStatus::Completed
        }) {
            crate::native_runtime::OperationOutcome::Completed
        } else {
            crate::native_runtime::OperationOutcome::Failed
        },
    })?;
    let reason = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    let (finished, _) = store.retained_settle(worker.id, false, |saved| {
        ensure!(saved.cancellation == started.cancellation || saved.state == WorkerState::Stopped,
            "retained cancellation identity changed before completion");
        if saved.expires_at <= crate::autonomy::now()? && saved.state != WorkerState::Stopped { saved.state = WorkerState::Expired; }
        saved.finish(execution, result.as_ref().ok(), &reason)?;
        if needs_authority && !matches!(saved.state,WorkerState::Stopped|WorkerState::Expired) {
            saved.state = WorkerState::NeedsReview;
            if let Some(receipt)=saved.last_receipt.as_mut() { receipt.summary = "Additional authority was requested and denied; owner review required before further work".into(); }
        }
        Ok(())
    })?;
    Ok(finished)
}
