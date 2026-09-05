use super::*;
use crate::{
    identity::{ConversationEntryId, OperationId},
    message::{Message, Role},
    session::{CompactionReason, DurableSession},
    storage::{RecoveryIdentity, TestCustody},
};

fn budget() -> crate::prompt::PromptBudgetPlan {
    crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 128,
            tool_reserve_tokens: 512,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "fixture".into(),
            model: "deterministic".into(),
            context_tokens: Some(16_384),
            max_output_tokens: Some(2_048),
            reasoning: false,
        },
    )
    .unwrap()
}

#[derive(Default)]
struct FixtureTurn {
    operation: Option<OperationId>,
    first_operation: Option<OperationId>,
    intent: Option<crate::operation::InvocationIntent>,
}

impl FixtureTurn {
    fn append(&mut self, session: &mut DurableSession, index: usize) {
        use crate::{
            identity::{StepId, ToolInvocationId, ToolResultId},
            message::{ContentBlock, ToolCall, ToolResult},
            native_runtime::OperationOutcome,
            operation::{
                InvocationIntent, InvocationOutcome, InvocationResultRecord, InvocationTarget,
                NamedValueRecord,
            },
            permission::{PermissionAuditFact, PermissionRequest, PermissionScope, PolicyDecision},
            tool::{EffectClass, ReplaySafety},
        };
        let phase = index % 1000;
        let repeats = if index.is_multiple_of(997) {
            4096
        } else {
            [2, 8, 32, 128][index % 4]
        };
        let text = format!("{index}: {}", "日本語 🦀 text ".repeat(repeats));
        if phase == 1 {
            let operation_id = self.operation.unwrap();
            let invocation_id = ToolInvocationId::new();
            let arguments = serde_json::json!({"fixture":index});
            let call = ToolCall {
                id: invocation_id.to_string(),
                name: "fixture_read".into(),
                arguments: arguments.clone(),
            };
            let entry = session
                .append_message(Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text(text), ContentBlock::ToolCall(call)],
                })
                .unwrap();
            let step_id = StepId::new();
            session
                .append_record(SessionRecord::StepStarted {
                    operation_id,
                    step_id,
                    assistant_entry_id: entry,
                })
                .unwrap();
            let permission = PermissionAuditFact {
                request: PermissionRequest {
                    operation_id,
                    invocation_id,
                    tool_name: "fixture_read".into(),
                    effect_class: EffectClass::Read,
                    final_arguments: arguments.clone(),
                    scope: PermissionScope::Unscoped,
                    outbound_review: None,
                },
                policy_evaluation: PolicyDecision::Allow,
                controller_decision: None,
                effective: PolicyDecision::Allow,
            };
            session
                .append_record(SessionRecord::PermissionAudited {
                    fact: permission.clone(),
                })
                .unwrap();
            let intent = InvocationIntent {
                operation_id,
                step_id,
                invocation_id,
                result_id: ToolResultId::new(),
                model_call_id: invocation_id.to_string(),
                target: InvocationTarget::Tool {
                    name: "fixture_read".into(),
                    contract_version: 1,
                },
                final_arguments: arguments,
                permission,
                saved_replay_safety: ReplaySafety::Safe,
            };
            session
                .append_record(SessionRecord::InvocationIntentAppended {
                    intent: intent.clone(),
                })
                .unwrap();
            self.intent = Some(intent);
        } else if phase == 2 {
            let intent = self.intent.take().unwrap();
            let (output,artifact)=session.store_tool_output(serde_json::json!({"fixture":index,"evidence":"bounded encrypted artifact ".repeat(2048)})).unwrap();
            session
                .append_record(SessionRecord::InvocationResultAppended {
                    result: InvocationResultRecord {
                        operation_id: intent.operation_id,
                        invocation_id: intent.invocation_id,
                        result_id: intent.result_id,
                        outcome: InvocationOutcome::Completed {
                            output: output.clone(),
                        },
                    },
                })
                .unwrap();
            session
                .append_record(SessionRecord::NamedValueSet {
                    value: NamedValueRecord {
                        id: crate::identity::NamedValueId::new(),
                        operation_id: intent.operation_id,
                        name: "fixture evidence".into(),
                        value: output,
                    },
                })
                .unwrap();
            let mut result = ToolResult::success(intent.model_call_id, text);
            result.artifact = artifact.map(Box::new);
            session
                .append_message(Message::tool_result(result))
                .unwrap();
        } else if index.is_multiple_of(2) {
            let entry = session
                .append_message(Message::text(Role::User, text))
                .unwrap();
            let operation = OperationId::new();
            self.first_operation.get_or_insert(operation);
            self.operation = Some(operation);
            session
                .append_record(SessionRecord::OperationAccepted {
                    operation_id: operation,
                    thread_id: session.thread_id(),
                    input_entry_id: entry,
                })
                .unwrap();
        } else {
            session
                .append_message(Message::text(Role::Assistant, text))
                .unwrap();
            session
                .append_record(SessionRecord::OperationFinished {
                    operation_id: self.operation.take().unwrap(),
                    outcome: OperationOutcome::Completed,
                })
                .unwrap();
        }
    }
}

#[test]
fn protected_execution_archives_raw_prefix_and_reopens_exact_bounded_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    let mut point = ConversationEntryId::new();
    let mut turn = FixtureTurn::default();
    for index in 0..2300 {
        turn.append(&mut session, index);
        if index == 2199 {
            point = home.history_metadata(id).unwrap().head.unwrap();
        }
        if index > 0 && index % 128 == 127 {
            let candidate = session
                .prepare_compaction(
                    OperationId::new(),
                    CompactionReason::AutomaticThreshold,
                    &budget(),
                )
                .unwrap();
            session.commit_compaction(candidate).unwrap();
        }
    }
    let continuation = session.prompt_continuation().unwrap();
    assert!(continuation.history.len() < 256);
    assert!(
        session.conversation().is_err(),
        "complete history is never silently truncated"
    );
    let page = session.initial_conversation_page().unwrap();
    assert_eq!(page.total, 2300);
    assert!(page.has_older && page.messages.len() <= 128);
    drop(session);
    let (mut resumed, summary) = DurableSession::resume_protected(home.clone(), id).unwrap();
    assert_eq!(resumed.prompt_continuation().unwrap(), continuation);
    assert_eq!(summary.active_entry_count, 2300);
    assert_eq!(summary.compaction_count, 17);
    assert!(summary.bounded_details);
    let old = resumed
        .inspect_stored_operation(turn.first_operation.unwrap())
        .unwrap()
        .unwrap();
    assert!(old.finished.is_some() && old.results.len() == 1);
    resumed
        .append_message(Message::text(Role::User, "after reconnect"))
        .unwrap();
    let revision = home.history_metadata(id).unwrap().revision;
    let restart = RecordEnvelope::new(
        id,
        SessionRecord::OperationStateChanged {
            operation_id: turn.first_operation.unwrap(),
            state: crate::native_runtime::OperationState::Running,
        },
    );
    assert!(home.append_history(id, revision, &restart).is_err());
    assert_eq!(home.history_metadata(id).unwrap().revision, revision);
    let target = SessionId::new();
    let branch = home
        .branch_history(id, point, target, crate::identity::ThreadId::new())
        .unwrap();
    assert_eq!(branch.branch.unwrap().shared_entry_count, 2200);
    let page = home.history_page(target, None, None, 128).unwrap();
    assert_eq!(page.total, 2200);
    let branch_continuation = DurableSession::inspect_execution_protected(&home, target).unwrap();
    assert!(branch_continuation.entries.len() < 2048);
    home.verify_content().unwrap();
}

#[test]
fn checkpoint_corruption_and_missing_archive_checkpoint_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    session
        .append_message(Message::text(Role::User, "exact input"))
        .unwrap();
    drop(session);
    home.with_database(|db| {
        db.connection.execute(
            "UPDATE native_execution_checkpoints SET body=x'7b7d' WHERE session=?1",
            [id.to_string()],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(DurableSession::resume_protected(home, id).is_err());
}

#[test]
fn protected_branch_cannot_turn_forgotten_source_into_eligible_history() {
    use crate::memory::{MemoryContext, MemoryEdit, MemoryOwner, MemoryScope};
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    let point = session
        .append_message(Message::text(Role::User, "source to forget"))
        .unwrap();
    let owner = MemoryOwner::new(
        home.clone(),
        MemoryContext {
            conversation: Some(id.to_string().parse().unwrap()),
            ..Default::default()
        },
    );
    let fact = owner
        .remember(MemoryScope::User, "Source fact".into(), None)
        .unwrap();
    owner
        .revise(fact.id, fact.revision, MemoryEdit::Forget)
        .unwrap();
    let target = SessionId::new();
    assert!(
        home.branch_history(id, point, target, crate::identity::ThreadId::new())
            .is_err()
    );
    assert!(
        !home.history_exists(target).unwrap(),
        "rejected branch is atomic"
    );
    assert_eq!(
        home.history_page(id, None, None, 128)
            .unwrap()
            .messages
            .len(),
        1,
        "owner inspection remains available"
    );
}

#[test]
fn committed_suffix_recovers_after_checkpoint_write_is_lost_and_keeps_unfinished_intent() {
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    let mut turn = FixtureTurn::default();
    for index in 0..60 {
        turn.append(&mut session, index);
    }
    let checkpoint:(i64,String,Vec<u8>)=home.with_database(|db| Ok(db.connection.query_row("SELECT revision,prefix_digest,body FROM native_execution_checkpoints WHERE session=?1",[id.to_string()],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)).unwrap();
    for index in 60..100 {
        turn.append(&mut session, index);
    }
    let expected = session.prompt_continuation().unwrap();
    drop(session);
    // Model a crash after durable append but before a later checkpoint commit.
    home.with_database(|db| {db.connection.execute("UPDATE native_execution_checkpoints SET revision=?2,prefix_digest=?3,body=?4 WHERE session=?1",rusqlite::params![id.to_string(),checkpoint.0,checkpoint.1,checkpoint.2])?;Ok(())}).unwrap();
    let (mut resumed, _) = DurableSession::resume_protected(home.clone(), id).unwrap();
    assert_eq!(resumed.prompt_continuation().unwrap(), expected);
    let mut unfinished = FixtureTurn::default();
    unfinished.append(&mut resumed, 1000);
    unfinished.append(&mut resumed, 1001);
    let operation = unfinished.operation.unwrap();
    drop(resumed);
    let (resumed, summary) = DurableSession::resume_protected(home.clone(), id).unwrap();
    let operation = resumed
        .inspect_stored_operation(operation)
        .unwrap()
        .unwrap();
    assert!(operation.finished.is_none());
    assert_eq!(operation.intents.len(), 1);
    assert!(operation.results.is_empty());
    assert_eq!(summary.unfinished.len(), 1);
    home.verify_content().unwrap();
}

#[test]
fn large_uncompacted_active_turn_is_refused_before_unbounded_execution_growth() {
    let directory = tempfile::tempdir().unwrap();
    let home = ProtectedStore::initialize(
        directory.path(),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    for _ in 0..crate::session::hydration::MAX_EXECUTION_ENTRIES {
        session
            .append_message(Message::text(Role::User, "x"))
            .unwrap();
    }
    let before = home.history_metadata(id).unwrap().revision;
    assert!(
        session
            .append_message(Message::text(Role::User, "refused"))
            .is_err()
    );
    assert_eq!(home.history_metadata(id).unwrap().revision, before);
    assert!(session.retained_pressure());
    drop(session);
    assert!(DurableSession::resume_protected(home, id).is_ok());
}

#[test]
fn retained_registration_limit_refuses_growth_but_allows_same_name_updates() {
    let id = SessionId::new();
    let mut state = crate::session::reduce(&[RecordEnvelope::new(
        id,
        SessionRecord::SessionCreated {
            thread_id: crate::identity::ThreadId::new(),
            workspace_root: "fixture".into(),
        },
    )])
    .unwrap();
    let context_id = crate::identity::ContextId::new();
    for index in 0..4096 {
        state
            .named_context
            .insert(format!("slot-{index}"), (context_id, 1));
    }
    let update = SessionRecord::NamedContextSet {
        name: "slot-0".into(),
        context_id,
        version: 1,
    };
    assert!(crate::session::hydration::validate_append_bounds(&state, &update).is_ok());
    let addition = SessionRecord::NamedContextSet {
        name: "one-too-many".into(),
        context_id,
        version: 1,
    };
    assert!(crate::session::hydration::validate_append_bounds(&state, &addition).is_err());
}

#[test]
#[ignore = "opt-in release-profile production 10k/100k history resource probe"]
fn protected_execution_resource_probe() {
    use std::time::Instant;
    let count: usize = std::env::var("XANA_HISTORY_PROBE_MESSAGES")
        .unwrap_or_else(|_| "10000".into())
        .parse()
        .unwrap();
    assert!([10_000, 100_000].contains(&count));
    let directory = tempfile::tempdir().unwrap();
    // Identify only this owned synthetic home if the opt-in probe is interrupted;
    // recovery material stays in memory and is never logged or persisted here.
    println!("fixture_directory={}", directory.path().display());
    let paths =
        crate::paths::XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let identity = RecoveryIdentity::generate();
    let custody = TestCustody::default();
    let home = ProtectedStore::initialize(paths.data_dir(), &identity, &custody).unwrap();
    let id = SessionId::new();
    let mut session =
        DurableSession::create_protected(home.clone(), directory.path().to_owned(), id).unwrap();
    let generation = Instant::now();
    let mut turn = FixtureTurn::default();
    for index in 0..count {
        turn.append(&mut session, index);
        if index % 1024 == 1023 {
            let candidate = session
                .prepare_compaction(
                    OperationId::new(),
                    CompactionReason::AutomaticThreshold,
                    &budget(),
                )
                .unwrap();
            session.commit_compaction(candidate).unwrap();
        }
        if index % 10_000 == 9999 {
            println!("fixture_committed={}", index + 1);
        }
    }
    println!(
        "production_fixture messages={count} generation_ms={} journal_bytes={}",
        generation.elapsed().as_millis(),
        home.history_metadata(id).unwrap().bytes
    );
    drop(session);
    drop(home);
    for trial in 0..5 {
        let timer = Instant::now();
        let home = ProtectedStore::open(paths.data_dir(), &custody).unwrap();
        let open_us = timer.elapsed().as_micros();
        let timer = Instant::now();
        let (session, summary) = DurableSession::resume_protected(home.clone(), id).unwrap();
        let resume_us = timer.elapsed().as_micros();
        assert_eq!(summary.active_entry_count, count);
        let page = session.initial_conversation_page().unwrap();
        assert_eq!(page.total, count);
        assert!(page.messages.len() <= 128);
        assert!(
            session
                .inspect_stored_operation(turn.first_operation.unwrap())
                .unwrap()
                .unwrap()
                .finished
                .is_some()
        );
        let mut samples = Vec::new();
        for turn in 0..30 {
            let start = [0, count / 2, count - 128][turn % 3];
            let timer = Instant::now();
            let page = home.history_page(id, None, Some(start), 128).unwrap();
            samples.push(timer.elapsed().as_micros());
            assert_eq!(page.start, start);
        }
        samples.sort_unstable();
        let state = DurableSession::inspect_execution_protected(&home, id).unwrap();
        println!(
            "protected_execution messages={count} trial={trial} open_us={open_us} resume_us={resume_us} page_median_us={} page_p95_us={} retained_entries={} execution_bytes={} initial_page_bytes={}",
            samples[15],
            samples[28],
            state.entries.len(),
            crate::session::hydration::encode(&state).unwrap().len(),
            serde_json::to_vec(&page.messages).unwrap().len()
        );
        drop(session);
        if trial == 4 {
            let timer = Instant::now();
            home.verify_content().unwrap();
            println!(
                "immutable_verification messages={count} elapsed_ms={}",
                timer.elapsed().as_millis()
            );
            if std::env::var_os("XANA_HISTORY_VERIFY_PROFILE_ONLY").is_some() {
                println!("verification_profile_only=true backup_restore_not_measured=true");
                return;
            }
            let timer = Instant::now();
            let backup = home
                .backup(
                    &crate::storage::backup::BackupPolicy::default(),
                    1000,
                    false,
                )
                .unwrap()
                .snapshot
                .unwrap();
            println!(
                "protected_backup messages={count} elapsed_ms={}",
                timer.elapsed().as_millis()
            );
            drop(home);
            let timer = Instant::now();
            let plan = crate::storage::restore::preview(&paths, &backup, &identity).unwrap();
            crate::storage::restore::apply(&paths, &backup, &identity, &plan.review).unwrap();
            let recovered = ProtectedStore::recover(paths.data_dir(), &identity).unwrap();
            assert_eq!(
                recovered.history_page(id, None, None, 128).unwrap().total,
                count
            );
            assert_eq!(
                DurableSession::inspect_execution_protected(&recovered, id)
                    .unwrap()
                    .session_id,
                id
            );
            println!(
                "protected_restore messages={count} elapsed_ms={}",
                timer.elapsed().as_millis()
            );
        }
    }
}
