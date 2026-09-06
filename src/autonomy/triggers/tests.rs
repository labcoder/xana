use super::*;
use crate::autonomy::{self, JobEdit, RunOutcome, RunReceipt, Schedule};
use futures::future::BoxFuture;
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

struct CountingExecutor(AtomicUsize);
impl autonomy::runner::TaskExecutor for CountingExecutor {
    fn execute<'a>(
        &'a self,
        job: &'a Job,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<(RunOutcome, String)>> {
        Box::pin(async move {
            validate_dispatch(job)?;
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok((
                RunOutcome::Completed,
                "fixed authorized task completed".into(),
            ))
        })
    }
}
fn selected(store: &ProtectedStore, mut job: Job) -> Job {
    job.trigger = Some(Trigger::Files(
        files::FileTrigger::create(store, &job.scope.workspace, &job.scope.workspace, &[]).unwrap(),
    ));
    job.schedule = Schedule::Triggered { poll_seconds: 5 };
    job
}

#[tokio::test]
async fn selected_changes_debounce_sleep_and_restart_without_duplicate_dispatch() {
    let (home, store, custody, base) = autonomy::tests::fixture();
    let root = base.scope.workspace.clone();
    fs::write(root.join("note.txt"), "original").unwrap();
    let job = selected(&store, base);
    let at = job.next.at;
    store.autonomy_create(job.clone()).unwrap();
    autonomy::host::policy_edit(&store, 0, Some(true), None, false, false).unwrap();
    let executor = CountingExecutor(AtomicUsize::new(0));
    let cancelled = CancellationToken::new();
    assert!(
        autonomy::runner::tick(&store, &executor, &|| Ok(at), &cancelled)
            .await
            .unwrap()
            .is_none()
    );
    fs::rename(root.join("note.txt"), root.join("renamed.txt")).unwrap();
    assert!(
        autonomy::runner::tick(&store, &executor, &|| Ok(at + 5), &cancelled)
            .await
            .unwrap()
            .is_none()
    );
    fs::write(root.join("renamed.txt"), "storm continues").unwrap();
    assert!(
        autonomy::runner::tick(&store, &executor, &|| Ok(at + 10), &cancelled)
            .await
            .unwrap()
            .is_none()
    );
    drop(store);
    let store = ProtectedStore::open(&home.path().join("data"), &custody).unwrap();
    store.autonomy_recover(at + 300).unwrap();
    let completed = autonomy::runner::tick(&store, &executor, &|| Ok(at + 300), &cancelled)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.state, JobState::Ready);
    assert_eq!(executor.0.load(Ordering::SeqCst), 1);
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 1);
    assert!(
        autonomy::runner::tick(&store, &executor, &|| Ok(at + 305), &cancelled)
            .await
            .unwrap()
            .is_none()
    );
    fs::remove_file(root.join("renamed.txt")).unwrap();
    refresh_due(&store, at + 310, &cancelled).await.unwrap();
    refresh_due(&store, at + 315, &cancelled).await.unwrap();
    assert!(store.autonomy_claim(at + 315).unwrap().is_some());
}

#[tokio::test]
async fn watcher_overflow_root_replacement_and_late_observation_fail_closed() {
    let (_home, store, _custody, base) = autonomy::tests::fixture();
    let job = selected(&store, base);
    let at = job.next.at;
    store.autonomy_create(job.clone()).unwrap();
    for index in 0..257 {
        fs::write(job.scope.workspace.join(format!("{index}.txt")), "x").unwrap();
    }
    refresh_due(&store, at, &CancellationToken::new())
        .await
        .unwrap();
    let failed = store.autonomy_job(job.id).unwrap();
    assert_eq!(failed.state, JobState::NeedsYou);
    assert!(store.autonomy_claim(at + 100).unwrap().is_none());
    assert!(
        store
            .autonomy_edit(
                job.id,
                failed.revision,
                JobEdit::Resume {
                    review_unknown: false
                },
                at + 100
            )
            .is_err()
    );
    let mut late = failed.clone();
    late.state = JobState::Ready;
    let cancelled = store
        .autonomy_edit(job.id, failed.revision, JobEdit::Cancel, at + 100)
        .unwrap();
    store.autonomy_observed(late).unwrap();
    assert_eq!(store.autonomy_job(job.id).unwrap(), cancelled);
    let old = job.scope.workspace.with_extension("replaced");
    fs::rename(&job.scope.workspace, &old).unwrap();
    fs::create_dir(&job.scope.workspace).unwrap();
    let Trigger::Files(watch) = job.trigger.unwrap() else {
        unreachable!()
    };
    assert!(files::validate_root(&watch, &job.scope.workspace).is_err());
}

#[test]
fn source_receipt_completion_and_pending_guard_are_transactional() {
    let (_home, store, _custody, base) = autonomy::tests::fixture();
    let mut job = selected(&store, base);
    let at = job.next.at;
    store.autonomy_create(job.clone()).unwrap();
    assert!(store.autonomy_claim(at).unwrap().is_none());
    job.trigger.as_mut().unwrap().observation_mut().pending = true;
    store.autonomy_observed(job.clone()).unwrap();
    let claimed = store.autonomy_claim(at).unwrap().unwrap();
    let receipt = RunReceipt {
        completion: None,
        occurrence: claimed.occurrence.unwrap(),
        scheduled_at: at,
        finished_at: at + 1,
        outcome: RunOutcome::Completed,
        detail: "one attributed event".into(),
        coalesced: false,
        dst_adjusted: false,
    };
    let done = store.autonomy_finish(job.id, receipt.clone()).unwrap();
    assert!(!done.trigger.unwrap().observation().pending);
    assert!(store.autonomy_finish(job.id, receipt).is_err());
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 1);
}

#[test]
fn selected_scope_refuses_state_overlap_and_outside_roots() {
    let (home, store, _custody, job) = autonomy::tests::fixture();
    assert!(files::FileTrigger::create(&store, home.path(), &job.scope.workspace, &[]).is_err());
    assert!(
        files::FileTrigger::create(
            &store,
            &job.scope.workspace,
            &job.scope.workspace,
            std::slice::from_ref(&job.scope.workspace)
        )
        .is_err()
    );
}

#[test]
fn source_observations_cannot_edit_frozen_task_or_source_authority() {
    let (_home, store, _custody, base) = autonomy::tests::fixture();
    let job = selected(&store, base);
    store.autonomy_create(job.clone()).unwrap();
    let mut altered = job.clone();
    altered.expires_at += 1;
    assert!(store.autonomy_observed(altered).is_err());
    let mut altered = job.clone();
    altered.budget.conservative_tokens /= 2;
    assert!(store.autonomy_observed(altered).is_err());
    let mut altered = job.clone();
    let Some(Trigger::Files(watch)) = altered.trigger.as_mut() else {
        unreachable!()
    };
    watch.root = watch.root.join("other-root");
    assert!(store.autonomy_observed(altered).is_err());
    assert_eq!(store.autonomy_job(job.id).unwrap(), job);
}

#[tokio::test]
async fn source_failure_receipt_survives_first_page_attention_baseline() {
    let (_home, store, _custody, base) = autonomy::tests::fixture();
    for _ in 0..17 {
        let mut future = base.clone();
        future.id = uuid::Uuid::new_v4();
        future.conversation = uuid::Uuid::new_v4();
        future.next.at += 100;
        future.not_before += 100;
        future.schedule = Schedule::Once { at: future.next.at };
        store.autonomy_create(future).unwrap();
    }
    let job = selected(&store, base);
    store.autonomy_create(job.clone()).unwrap();
    let mut observer = autonomy::supervision::attention::AttentionObserver::default();
    assert!(observer.poll(&store).unwrap().is_empty());
    for index in 0..257 {
        fs::write(job.scope.workspace.join(format!("{index}.txt")), "x").unwrap();
    }
    refresh_due(&store, job.next.at, &CancellationToken::new())
        .await
        .unwrap();
    let notes = observer.poll(&store).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].task, job.id.to_string());
    assert_eq!(
        notes[0].kind,
        autonomy::supervision::attention::BackgroundAttentionKind::NeedsYou
    );
    assert!(observer.poll(&store).unwrap().is_empty());
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 1);
}

#[tokio::test]
async fn protected_tool_completion_suppresses_exact_own_write_but_not_later_user_edit() {
    use crate::{
        identity::{OperationId, StepId, ToolInvocationId, ToolResultId},
        message::{Message, Role, ToolCall},
        operation::{
            DurableValueRef, InvocationIntent, InvocationOutcome, InvocationResultRecord,
            InvocationTarget,
        },
        permission::{PermissionAuditFact, PolicyDecision},
        session::{DurableSession, SessionRecord},
        shell::{Shell, ShellConfig},
        tool::{ToolExecutionContext, ToolRegistry},
    };
    for remove_before_result in [false, true] {
        let (_home, store, _custody, base) = autonomy::tests::fixture();
        let path = base.scope.workspace.join("note.txt");
        fs::write(&path, "original").unwrap();
        let job = selected(&store, base);
        store.autonomy_create(job.clone()).unwrap();
        let mut session = DurableSession::create_protected(
            store.clone(),
            job.scope.workspace.clone(),
            job.conversation.to_string().parse().unwrap(),
        )
        .unwrap();
        let operation_id = OperationId::new();
        let input_entry_id = session
            .append_message(Message::text(Role::User, "update the note"))
            .unwrap();
        session
            .append_record(SessionRecord::OperationAccepted {
                operation_id,
                thread_id: session.thread_id(),
                input_entry_id,
            })
            .unwrap();
        let assistant_entry_id = session
            .append_message(Message::text(Role::Assistant, "updating"))
            .unwrap();
        let step_id = StepId::new();
        session
            .append_record(SessionRecord::StepStarted {
                operation_id,
                step_id,
                assistant_entry_id,
            })
            .unwrap();
        let tools = ToolRegistry::builtins_from_names(
            Shell::resolve(ShellConfig::default()).unwrap(),
            &["write_file".to_owned()].into_iter().collect(),
        )
        .unwrap();
        let call = ToolCall {
            id: "write-1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"note.txt","content":"Xana output","mode":"overwrite"}),
        };
        let planned = tools.plan(&call, &job.scope.workspace).unwrap();
        let invocation_id = ToolInvocationId::new();
        let fact = PermissionAuditFact {
            request: planned.permission_request(operation_id, invocation_id),
            policy_evaluation: PolicyDecision::Allow,
            controller_decision: None,
            effective: PolicyDecision::Allow,
        };
        session
            .append_record(SessionRecord::PermissionAudited { fact: fact.clone() })
            .unwrap();
        let intent = InvocationIntent {
            operation_id,
            step_id,
            invocation_id,
            result_id: ToolResultId::new(),
            model_call_id: call.id,
            target: InvocationTarget::Tool {
                name: "write_file".into(),
                contract_version: 1,
            },
            final_arguments: planned.final_arguments().clone(),
            permission: fact,
            saved_replay_safety: crate::tool::ReplaySafety::Never,
        };
        session
            .append_record(SessionRecord::InvocationIntentAppended {
                intent: intent.clone(),
            })
            .unwrap();
        let output = planned
            .execute(ToolExecutionContext {
                operation_id,
                events: None,
                outbound_approval: None,
                cleanup: Default::default(),
            })
            .await
            .unwrap();
        if remove_before_result {
            fs::remove_file(&path).unwrap();
        }
        session
            .append_record(SessionRecord::InvocationResultAppended {
                result: InvocationResultRecord {
                    command_status: None,
                    operation_id,
                    invocation_id,
                    result_id: intent.result_id,
                    outcome: InvocationOutcome::Completed {
                        output: DurableValueRef::InlineJson(output.into()),
                    },
                },
            })
            .unwrap();
        assert!(
            session
                .restored_operation(operation_id)
                .unwrap()
                .results
                .contains_key(&invocation_id)
        );
        let Trigger::Files(watch) = job.trigger.unwrap() else {
            unreachable!()
        };
        let now = autonomy::now().unwrap();
        if remove_before_result {
            assert!(files::observe(&store, watch, &job.scope.workspace, now).is_err());
            continue;
        }
        let watch = files::observe(&store, watch, &job.scope.workspace, now).unwrap();
        assert!(!watch.observation.pending);
        fs::write(&path, "a later independent human revision").unwrap();
        let watch = files::observe(&store, watch, &job.scope.workspace, now + 5).unwrap();
        let watch = files::observe(&store, watch, &job.scope.workspace, now + 10).unwrap();
        assert!(watch.observation.pending);
        let mut command = intent;
        command.target = InvocationTarget::Tool {
            name: "run_command".into(),
            contract_version: 1,
        };
        files::record_invocation(&store, &command, false).unwrap();
        fs::write(&path, "unattributed command revision").unwrap();
        assert!(files::observe(&store, watch, &job.scope.workspace, now + 15).is_err());
    }
}

#[cfg(unix)]
#[test]
fn symlink_events_do_not_expand_watcher_scope() {
    let (_home, store, _custody, job) = autonomy::tests::fixture();
    let watch = files::FileTrigger::create(&store, &job.scope.workspace, &job.scope.workspace, &[])
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), job.scope.workspace.join("escape")).unwrap();
    assert!(files::observe(&store, watch, &job.scope.workspace, job.next.at).is_err());
}
