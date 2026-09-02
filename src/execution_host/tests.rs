use super::*;
use crate::{
    frontend::{ClientEvent, ClientObservation},
    identity::ToolInvocationId,
    managed::thread_store::ManagedThreadStore,
    native_runtime::AgentEvent,
    permission::{PermissionAuditFact, PermissionRequest, PermissionScope, PolicyDecision},
    session::DurableSession,
    tool::EffectClass,
};
use serde_json::Value;
use std::{fs, path::Path};
use tempfile::TempDir;

fn native_conversation(directory: &TempDir, workspace: &Path) -> (ConversationRef, WorkspaceHost) {
    let session = DurableSession::create(directory.path(), workspace.to_owned()).unwrap();
    let conversation = ConversationRef::Native {
        session_id: session.session_id(),
    };
    drop(session);
    let workspace_host = WorkspaceHost::open(directory.path(), workspace).unwrap();
    (conversation, workspace_host)
}

fn registration(conversation: ConversationRef, connection: &str) -> ConversationRegistration {
    ConversationRegistration::new(
        conversation,
        connection,
        format!("{connection}-model"),
        Some(format!("{connection}-profile")),
        "ask",
    )
}

#[test]
fn conversations_hold_independent_controller_authority() {
    let directory = tempfile::tempdir().unwrap();
    let host = ExecutionHost::new();
    let workspace_a = directory.path().join("workspace-a");
    let workspace_b = directory.path().join("workspace-b");
    fs::create_dir(&workspace_a).unwrap();
    fs::create_dir(&workspace_b).unwrap();
    let first = register_native(&host, &directory, &workspace_a);
    let second = register_native(&host, &directory, &workspace_b);
    let first_client = ControllerClientId::new();
    let second_client = ControllerClientId::new();

    let first_grant = host
        .acquire_controller(&first, first_client, None, Instant::now())
        .unwrap();
    let second_grant = host
        .acquire_controller(&second, second_client, None, Instant::now())
        .unwrap();

    host.require_controller(&first, first_client).unwrap();
    host.require_controller(&second, second_client).unwrap();
    assert!(matches!(
        host.require_controller(&first, second_client),
        Err(ExecutionHostError::Controller(
            ControllerLeaseError::NotController(value)
        )) if value == first
    ));
    let snapshot = host.snapshot().unwrap();
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|candidate| candidate.conversation == first)
            .and_then(|candidate| candidate.controller.as_ref())
            .map(|controller| controller.controller_id),
        Some(first_grant.snapshot.controller_id)
    );
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|candidate| candidate.conversation == second)
            .and_then(|candidate| candidate.controller.as_ref())
            .map(|controller| controller.controller_id),
        Some(second_grant.snapshot.controller_id)
    );
}

#[test]
fn pending_approval_blocks_an_exact_controller_takeover() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let host = ExecutionHost::new();
    let conversation = register_native(&host, &directory, &workspace);
    let incumbent = ControllerClientId::new();
    let challenger = ControllerClientId::new();
    let grant = host
        .acquire_controller(&conversation, incumbent, None, Instant::now())
        .unwrap();
    let request = PermissionRequest {
        operation_id: OperationId::new(),
        invocation_id: ToolInvocationId::new(),
        tool_name: "write_file".to_owned(),
        effect_class: EffectClass::Write,
        final_arguments: Value::Null,
        scope: PermissionScope::WorkspacePath {
            canonical_path: workspace.canonicalize().unwrap(),
        },
        outbound_review: None,
    };
    host.record_runtime_observation(
        &conversation,
        &ClientObservation {
            version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
            sequence: 1,
            event: ClientEvent::Runtime(Box::new(AgentEvent::PermissionRequested { request })),
        },
    )
    .unwrap();

    assert!(matches!(
        host.acquire_controller(
            &conversation,
            challenger,
            Some(ControllerTakeoverConfirmation::from(&grant.snapshot)),
            Instant::now(),
        ),
        Err(ExecutionHostError::Controller(
            ControllerLeaseError::PendingApproval(value)
        )) if value == conversation
    ));
    host.require_controller(&conversation, incumbent).unwrap();
}

fn register_native(host: &ExecutionHost, directory: &TempDir, workspace: &Path) -> ConversationRef {
    let (conversation, workspace_host) = native_conversation(directory, workspace);
    host.register(workspace_host, registration(conversation.clone(), "native"))
        .unwrap();
    conversation
}

#[test]
fn aliases_share_one_workspace_collision_domain() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let host = ExecutionHost::new();
    let first = register_native(&host, &directory, &workspace);
    let (second, alias_host) = native_conversation(&directory, &workspace.join("."));
    host.register(alias_host, registration(second.clone(), "native"))
        .unwrap();

    let snapshot = host.snapshot().unwrap();
    assert_eq!(snapshot.workspaces.len(), 1);
    assert_eq!(snapshot.conversations.len(), 2);

    let first_run = host
        .begin_run(
            &first,
            OperationId::new(),
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        )
        .unwrap();
    assert!(matches!(
        host.begin_run(
            &second,
            OperationId::new(),
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        ),
        Err(ExecutionHostError::WriteCollision { .. })
    ));
    let second_run = host
        .begin_run(
            &second,
            OperationId::new(),
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Acknowledge,
        )
        .unwrap();
    host.finish_run(first_run, Ok(OperationOutcome::Completed))
        .unwrap();
    host.finish_run(second_run, Ok(OperationOutcome::Completed))
        .unwrap();
}

#[test]
fn admission_is_bounded_to_eight_conversations_and_four_runs() {
    let directory = tempfile::tempdir().unwrap();
    let host = ExecutionHost::new();
    let mut conversations = Vec::new();
    for index in 0..=MAX_HOSTED_CONVERSATIONS {
        let workspace = directory.path().join(format!("workspace-{index}"));
        fs::create_dir(&workspace).unwrap();
        let (conversation, workspace_host) = native_conversation(&directory, &workspace);
        let result = host.register(
            workspace_host,
            registration(conversation.clone(), &format!("connection-{index}")),
        );
        if index < MAX_HOSTED_CONVERSATIONS {
            result.unwrap();
            conversations.push(conversation);
        } else {
            assert!(matches!(
                result,
                Err(ExecutionHostError::Limit {
                    resource: "Conversation",
                    limit: MAX_HOSTED_CONVERSATIONS,
                })
            ));
        }
    }

    let mut runs = Vec::new();
    for conversation in conversations.iter().take(MAX_CONCURRENT_RUNS) {
        runs.push(
            host.begin_run(
                conversation,
                OperationId::new(),
                RunAccess::WorkspaceWrite,
                WriteCollisionDecision::Reject,
            )
            .unwrap(),
        );
    }
    assert!(matches!(
        host.begin_run(
            &conversations[MAX_CONCURRENT_RUNS],
            OperationId::new(),
            RunAccess::ReadOnly,
            WriteCollisionDecision::Reject,
        ),
        Err(ExecutionHostError::Limit {
            resource: "concurrent Run",
            limit: MAX_CONCURRENT_RUNS,
        })
    ));
    for run in runs {
        host.finish_run(run, Ok(OperationOutcome::Completed))
            .unwrap();
    }
}

#[test]
fn four_conversations_stream_without_cross_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let host = ExecutionHost::new();
    let mut conversations = Vec::new();
    let mut runs = Vec::new();
    for index in 0..4 {
        let workspace = directory.path().join(format!("stream-workspace-{index}"));
        fs::create_dir(&workspace).unwrap();
        let conversation = register_native(&host, &directory, &workspace);
        runs.push(
            host.begin_run(
                &conversation,
                OperationId::new(),
                RunAccess::WorkspaceWrite,
                WriteCollisionDecision::Reject,
            )
            .unwrap(),
        );
        conversations.push(conversation);
    }
    let start = host.snapshot().unwrap().sequence;
    std::thread::scope(|scope| {
        for conversation in &conversations {
            let host = host.clone();
            let conversation = conversation.clone();
            scope.spawn(move || {
                for sequence in 1..=32_u64 {
                    host.record_runtime_observation(
                        &conversation,
                        &ClientObservation {
                            version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
                            sequence,
                            event: ClientEvent::Runtime(Box::new(AgentEvent::CommandRejected {
                                reason: format!("{conversation}:{sequence}"),
                            })),
                        },
                    )
                    .unwrap();
                }
            });
        }
    });
    let HostChanges::Events(events) = host.changes_after(start).unwrap() else {
        panic!("bounded stream should retain this suffix");
    };
    for conversation in &conversations {
        let delivered = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.event,
                    HostEvent::RuntimeObservation { conversation: actual, .. }
                        if actual == conversation
                )
            })
            .count();
        assert_eq!(delivered, 32);
    }
    for run in runs {
        host.finish_run(run, Ok(OperationOutcome::Completed))
            .unwrap();
    }
}

#[test]
fn a_discarded_event_position_requires_one_fresh_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let host = ExecutionHost::new();
    let conversation = register_native(&host, &directory, &workspace);
    let old_cursor = host.snapshot().unwrap().sequence;

    for sequence in 1..=600_u64 {
        host.record_runtime_observation(
            &conversation,
            &ClientObservation {
                version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
                sequence,
                event: ClientEvent::Runtime(Box::new(AgentEvent::CommandRejected {
                    reason: format!("event-{sequence}"),
                })),
            },
        )
        .unwrap();
    }

    let HostChanges::SnapshotRequired(snapshot) = host.changes_after(old_cursor).unwrap() else {
        panic!("discarded cursor must not receive an unproven suffix");
    };
    assert_eq!(snapshot.sequence, host.snapshot().unwrap().sequence);
    assert_eq!(snapshot.conversations.len(), 1);
    assert_eq!(snapshot.conversations[0].activity_count, 600);
    assert!(matches!(
        host.changes_after(snapshot.sequence).unwrap(),
        HostChanges::Events(events) if events.is_empty()
    ));
}

#[test]
fn a_forward_runtime_gap_reports_resync_without_wedging_the_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let host = ExecutionHost::new();
    let conversation = register_native(&host, &directory, &workspace);
    let observation = |sequence| ClientObservation {
        version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
        sequence,
        event: ClientEvent::Runtime(Box::new(AgentEvent::CommandRejected {
            reason: format!("event-{sequence}"),
        })),
    };

    assert!(matches!(
        host.record_runtime_observation(&conversation, &observation(3)),
        Err(ExecutionHostError::RuntimeGap {
            expected: 1,
            received: 3,
        })
    ));
    host.record_runtime_observation(&conversation, &observation(4))
        .unwrap();

    assert_eq!(host.snapshot().unwrap().conversations[0].activity_count, 2);
}

#[test]
fn attach_restores_native_or_managed_owner_and_failure_preserves_previous() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let host = ExecutionHost::new();
    let native = register_native(&host, &directory, &workspace);
    let native_receipt = host.attach(&native).unwrap();
    assert_eq!(
        native_receipt.restoration,
        OwnerRestoration::NativeDurableHistory
    );

    let managed = ConversationRef::Managed {
        conversation_id: crate::identity::ConversationId::new(),
        connection: "codex".to_owned(),
        thread_id: "thread-1".to_owned(),
    };
    {
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                managed.conversation_id(),
                Some("thread-1".to_owned()),
                Some("identity-v1"),
            )
            .unwrap();
    }
    host.register(
        WorkspaceHost::open(directory.path(), &workspace).unwrap(),
        registration(managed.clone(), "codex"),
    )
    .unwrap();
    let managed_receipt = host.attach(&managed).unwrap();
    assert_eq!(managed_receipt.previous, Some(native.clone()));
    assert_eq!(
        managed_receipt.restoration,
        OwnerRestoration::ManagedOpaqueThread
    );

    let unavailable = ConversationRef::Managed {
        conversation_id: crate::identity::ConversationId::new(),
        connection: "codex".to_owned(),
        thread_id: "not-retained".to_owned(),
    };
    host.register(
        WorkspaceHost::open(directory.path(), &workspace).unwrap(),
        registration(unavailable.clone(), "codex"),
    )
    .unwrap();
    assert!(matches!(
        host.attach(&unavailable),
        Err(ExecutionHostError::InvalidConversation(_))
    ));
    assert_eq!(host.snapshot().unwrap().attached, Some(managed));
}

#[test]
fn restart_reconstructs_idle_conversations_without_replaying_runs() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let conversation = {
        let host = ExecutionHost::new();
        let conversation = register_native(&host, &directory, &workspace);
        host.acquire_controller(
            &conversation,
            ControllerClientId::new(),
            None,
            Instant::now(),
        )
        .unwrap();
        let _run = host
            .begin_run(
                &conversation,
                OperationId::new(),
                RunAccess::WorkspaceWrite,
                WriteCollisionDecision::Reject,
            )
            .unwrap();
        conversation
    };

    let restarted = ExecutionHost::new();
    restarted
        .register(
            WorkspaceHost::open(directory.path(), &workspace).unwrap(),
            registration(conversation.clone(), "native"),
        )
        .unwrap();
    let snapshot = restarted.snapshot().unwrap();
    assert_eq!(snapshot.active_runs, 0);
    assert!(snapshot.conversations[0].controller.is_none());
    assert_eq!(
        snapshot.conversations[0].state,
        HostedConversationState::Idle
    );
    let run = restarted
        .begin_run(
            &conversation,
            OperationId::new(),
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        )
        .unwrap();
    restarted
        .finish_run(run, Ok(OperationOutcome::Completed))
        .unwrap();
}

#[test]
fn terminal_outcomes_remain_exact_and_conversation_scoped() {
    let directory = tempfile::tempdir().unwrap();
    let host = ExecutionHost::new();
    let outcomes = [
        (
            OperationOutcome::Completed,
            HostedConversationState::Completed,
        ),
        (OperationOutcome::Failed, HostedConversationState::Failed),
        (
            OperationOutcome::Declined,
            HostedConversationState::Declined,
        ),
        (
            OperationOutcome::Interrupted,
            HostedConversationState::Interrupted,
        ),
    ];
    let mut conversations = Vec::new();
    for (index, (outcome, expected)) in outcomes.into_iter().enumerate() {
        let workspace = directory.path().join(format!("terminal-{index}"));
        fs::create_dir(&workspace).unwrap();
        let conversation = register_native(&host, &directory, &workspace);
        let run = host
            .begin_run(
                &conversation,
                OperationId::new(),
                RunAccess::WorkspaceWrite,
                WriteCollisionDecision::Reject,
            )
            .unwrap();
        host.finish_run(run, Ok(outcome)).unwrap();
        conversations.push((conversation, outcome, expected));
    }

    let snapshot = host.snapshot().unwrap();
    for (conversation, outcome, expected) in conversations {
        let projected = snapshot
            .conversations
            .iter()
            .find(|candidate| candidate.conversation == conversation)
            .unwrap();
        assert_eq!(projected.state, expected);
        assert_eq!(projected.last_outcome, Some(outcome));
        assert_eq!(projected.last_error, None);
    }
}

#[test]
fn approvals_failures_and_interruptions_never_cross_conversation_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_a = directory.path().join("workspace-a");
    let workspace_b = directory.path().join("workspace-b");
    fs::create_dir(&workspace_a).unwrap();
    fs::create_dir(&workspace_b).unwrap();
    let host = ExecutionHost::new();
    let first = register_native(&host, &directory, &workspace_a);
    let second = register_native(&host, &directory, &workspace_b);
    let first_operation = OperationId::new();
    let second_operation = OperationId::new();
    let first_run = host
        .begin_run(
            &first,
            first_operation,
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        )
        .unwrap();
    let second_run = host
        .begin_run(
            &second,
            second_operation,
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        )
        .unwrap();
    let request = PermissionRequest {
        operation_id: first_operation,
        invocation_id: ToolInvocationId::new(),
        tool_name: "write_file".to_owned(),
        effect_class: EffectClass::Write,
        final_arguments: Value::Null,
        scope: PermissionScope::WorkspacePath {
            canonical_path: workspace_a.canonicalize().unwrap(),
        },
        outbound_review: None,
    };
    host.record_runtime_observation(
        &first,
        &ClientObservation {
            version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
            sequence: 1,
            event: ClientEvent::Runtime(Box::new(AgentEvent::PermissionRequested {
                request: request.clone(),
            })),
        },
    )
    .unwrap();
    host.record_runtime_observation(
        &second,
        &ClientObservation {
            version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
            sequence: 1,
            event: ClientEvent::Runtime(Box::new(AgentEvent::OperationFailed {
                operation_id: second_operation,
                reason: "second-only failure".to_owned(),
            })),
        },
    )
    .unwrap();
    host.record_runtime_observation(
        &first,
        &ClientObservation {
            version: crate::frontend::FRONTEND_PROTOCOL_VERSION,
            sequence: 2,
            event: ClientEvent::Runtime(Box::new(AgentEvent::PermissionAudited {
                fact: PermissionAuditFact {
                    request,
                    policy_evaluation: PolicyDecision::Ask,
                    controller_decision: None,
                    effective: PolicyDecision::Deny,
                },
            })),
        },
    )
    .unwrap();
    host.finish_run(first_run, Ok(OperationOutcome::Interrupted))
        .unwrap();
    host.finish_run(second_run, Ok(OperationOutcome::Failed))
        .unwrap();

    let snapshot = host.snapshot().unwrap();
    let first_snapshot = snapshot
        .conversations
        .iter()
        .find(|candidate| candidate.conversation == first)
        .unwrap();
    let second_snapshot = snapshot
        .conversations
        .iter()
        .find(|candidate| candidate.conversation == second)
        .unwrap();
    assert_eq!(first_snapshot.pending_approvals, 0);
    assert_eq!(first_snapshot.last_error, None);
    assert_eq!(
        first_snapshot.last_outcome,
        Some(OperationOutcome::Interrupted)
    );
    assert_eq!(second_snapshot.pending_approvals, 0);
    assert_eq!(
        second_snapshot.last_error.as_deref(),
        Some("second-only failure")
    );
    assert_eq!(second_snapshot.last_outcome, Some(OperationOutcome::Failed));
}
