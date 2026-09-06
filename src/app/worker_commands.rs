//! Shared typed retained-worker controls for owner clients and CLI composition.
use crate::{
    artifact::ArtifactStore,
    cli::{WorkerArgs, WorkerCommand},
    identity::AgentId,
    orchestration::context_ops::ContextWorkState,
    orchestration::retained::{self, FollowUp, RetainedWorker, WorkerState, execution},
    paths::XanaPaths,
    session::DurableSession,
    storage::ProtectedStore,
    workspace_identity::WorkspaceIdentity,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) async fn run(args: WorkerArgs, paths: &XanaPaths) -> Result<()> {
    let cancellation = CancellationToken::new();
    let task = execute(paths.clone(), args.command, cancellation.clone());
    tokio::pin!(task);
    let result = tokio::select! {result=&mut task=>result, _=tokio::signal::ctrl_c()=>{cancellation.cancel();task.await}}?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn store(paths: &XanaPaths) -> Result<ProtectedStore> {
    ProtectedStore::configured(paths.data_dir())?
        .context("retained workers require unlocked protected storage")
}

pub(crate) async fn execute(
    paths: XanaPaths,
    command: WorkerCommand,
    cancellation: CancellationToken,
) -> Result<Value> {
    let store = store(&paths)?;
    if let WorkerCommand::Run { target } = &command {
        let worker = store.retained_worker(target.id)?;
        ensure!(
            worker.revision == target.revision,
            "worker changed; refresh its revision"
        );
        return Ok(json!(
            execution::run(paths, store, worker, cancellation).await?
        ));
    }
    tokio::task::spawn_blocking(move || control(&paths, &store, command, &cancellation))
        .await
        .context("worker control stopped")?
}

pub(crate) fn control(
    paths: &XanaPaths,
    store: &ProtectedStore,
    command: WorkerCommand,
    cancellation: &CancellationToken,
) -> Result<Value> {
    ensure!(!cancellation.is_cancelled(), "worker control cancelled");
    Ok(match command {
        WorkerCommand::Retain {
            session,
            agent,
            goal,
            expires,
            evidence,
            authorize,
        } => {
            ensure!(
                authorize,
                "retaining work requires explicit --authorize after reviewing its scope and route"
            );
            retained::validate_text(&goal)?;
            let expires_at = expires.parse::<jiff::Timestamp>()?.as_second();
            ensure!(
                expires_at > crate::autonomy::now()?,
                "worker expiry must be in the future"
            );
            let (_, restored) = DurableSession::inspect_protected(store, session)?;
            let inspection = restored
                .children
                .get(&agent)
                .context("child is not part of this parent Conversation")?;
            ensure!(
                inspection.handle.lifecycle.is_terminal(),
                "only a terminal child can be retained"
            );
            let admission = inspection.handle.admission.clone();
            ensure!(
                admission.attribution.parent_agent_id == AgentId::for_session(session),
                "only original root-owned children can be retained; recursion is not enabled"
            );
            ensure!(
                evidence.len() <= 16,
                "initial evidence exceeds 16 references"
            );
            let mut artifacts = Vec::new();
            for id in evidence {
                let artifact = restored
                    .artifacts
                    .get(&id)
                    .context("evidence is not registered to this Conversation")?;
                ArtifactStore::protected(store.clone()).verify_reference(
                    &artifact.reference,
                    artifact.byte_len,
                    crate::artifact::MAX_ARTIFACT_BYTES,
                )?;
                artifacts.push(artifact.clone());
            }
            let worker = RetainedWorker {
                version: 1,
                id: agent,
                revision: 1,
                session,
                goal,
                admission,
                scope: (crate::recall::RecallOwner {
                    store: store.clone(),
                    paths: paths.clone(),
                    conversation: session,
                })
                .scope(session)?,
                workspace_identity: WorkspaceIdentity::resolve(&restored.workspace_root)?
                    .collision_key()
                    .into(),
                configuration_digest: execution::configuration_digest(paths, session)?,
                privacy_generation: store.privacy_generation()?,
                expires_at,
                state: WorkerState::Idle,
                mailbox: Vec::new(),
                accepted_requests: Vec::new(),
                active: None,
                cancellation: Uuid::new_v4(),
                executions: 0,
                context_bytes: 0,
                context_operations: 0,
                context_receipt: None,
                evidence: artifacts,
                last_receipt: None,
            };
            execution::check_scope(paths, store, &worker)?;
            store.retained_create(worker.clone())?;
            json!(worker)
        }
        WorkerCommand::List { after } => {
            json!({"workers":store.retained_page(after)?,"page_limit":32})
        }
        WorkerCommand::Inspect { id } => json!(store.retained_worker(id)?),
        WorkerCommand::FollowUp {
            target,
            request_id,
            text,
        } => {
            retained::validate_text(&text)?;
            ensure!(
                !request_id.is_nil(),
                "follow-up request identity is required"
            );
            let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
            let current = store.retained_worker(target.id)?;
            if let Some((_, prior)) = current
                .accepted_requests
                .iter()
                .find(|(id, _)| *id == request_id)
            {
                ensure!(
                    prior == &hash,
                    "follow-up id was already used with different content"
                );
                return Ok(json!({"worker":current,"duplicate":true}));
            }
            execution::check_scope(paths, store, &current)?;
            let (worker, ()) = store.retained_admit(target.id, target.revision, |worker| {
                ensure!(
                    matches!(worker.state, WorkerState::Idle | WorkerState::Running),
                    "worker is draining or stopped"
                );
                ensure!(
                    worker.mailbox.len() < retained::MAILBOX_LIMIT
                        && worker.accepted_requests.len() < 64,
                    "worker mailbox or lifetime follow-up limit reached"
                );
                worker.mailbox.push(FollowUp {
                    id: request_id,
                    text,
                });
                worker.accepted_requests.push((request_id, hash));
                Ok(())
            })?;
            json!(worker)
        }
        WorkerCommand::Drain { target } => json!(
            store
                .retained_update(target.id, target.revision, |worker| {
                    ensure!(
                        matches!(
                            worker.state,
                            WorkerState::Idle | WorkerState::Running | WorkerState::Draining
                        ),
                        "worker is not active"
                    );
                    worker.state = if worker.active.is_none() && worker.mailbox.is_empty() {
                        WorkerState::Stopped
                    } else {
                        WorkerState::Draining
                    };
                    Ok(())
                })?
                .0
        ),
        WorkerCommand::Stop { target } => json!(
            store
                .retained_update(target.id, target.revision, |worker| {
                    worker.state = WorkerState::Stopped;
                    worker.cancellation = Uuid::new_v4();
                    Ok(())
                })?
                .0
        ),
        WorkerCommand::Recover {
            target,
            review_unknown,
        } => {
            ensure!(
                review_unknown,
                "recovery requires --review-unknown; no interrupted message is replayed"
            );
            let current = store.retained_worker(target.id)?;
            let (_session, _) = DurableSession::resume_protected(store.clone(), current.session)?;
            json!(
                store
                    .retained_update(target.id, target.revision, |worker| {
                        ensure!(
                            matches!(
                                worker.state,
                                WorkerState::Running
                                    | WorkerState::NeedsReview
                                    | WorkerState::Draining
                            ) || (worker.state == WorkerState::Idle
                                && worker
                                    .context_receipt
                                    .as_ref()
                                    .is_some_and(|r| r.state == ContextWorkState::Reserved)),
                            "worker does not require recovery"
                        );
                        if let Some((id, _)) = worker.active.as_ref() {
                            worker.finish(
                                *id,
                                None,
                                "Owner reviewed interrupted/unknown work; no replay",
                            )?;
                        }
                        if let Some(receipt) = worker
                            .context_receipt
                            .as_mut()
                            .filter(|r| r.state == ContextWorkState::Reserved)
                        {
                            receipt.state = ContextWorkState::Interrupted;
                        }
                        worker.state = WorkerState::Idle;
                        Ok(())
                    })?
                    .0
            )
        }
        WorkerCommand::Context { target, operation } => {
            ensure!(
                operation.len() <= 16 * 1024,
                "context operation request exceeds its bound"
            );
            let operation = serde_json::from_str(&operation)?;
            json!(crate::orchestration::context_ops::execute(
                paths,
                store,
                target.id,
                target.revision,
                operation,
                cancellation
            )?)
        }
        WorkerCommand::Run { .. } => anyhow::bail!("worker run requires the async owner path"),
    })
}
