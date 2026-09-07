use super::*;
use crate::{
    artifact::ArtifactStore,
    frontend::semantic::{ContentPartV1, normalize_message},
    message::ContentBlock,
};

#[tokio::test]
async fn executor_keeps_complete_evidence_inspectable_and_compactable() {
    let (data, _workspace, mut session, operation_id, _, _) =
        durable_pending_operation(ReplaySafety::Safe);
    let step_id = session.restored_operation(operation_id).unwrap().step_order[0];
    let (commits, mut commands) = DurableOperationSender::channel();
    let writer = tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                DurableOperationCommand::Append {
                    record,
                    acknowledged,
                    ..
                } => {
                    let _ = acknowledged
                        .send(session.append_record(*record).map_err(|e| e.to_string()));
                }
                DurableOperationCommand::StoreJson {
                    value,
                    acknowledged,
                } => {
                    let _ = acknowledged
                        .send(session.store_tool_output(value).map_err(|e| e.to_string()));
                }
            }
        }
        session
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = registry("counted", 1, ReplaySafety::Safe, effects.clone());
    let (events, _receiver) = mpsc::unbounded_channel();
    let (permissions, broker) = PermissionBroker::spawn_for_durable_runtime(
        PermissionPolicy::new(PolicyDecision::Allow, vec![], Path::new(".")).unwrap(),
        true,
        events,
        [],
    );
    let executor = OperationExecutor::new(
        &tools,
        Path::new("."),
        permissions.clone(),
        commits.clone(),
        Arc::new(NoopBoundaryObserver),
        None,
        DeferredCleanup::default(),
    );
    let complete = format!("{}TAIL_EVIDENCE", "🦀".repeat(20_000));
    let result = executor
        .invoke_tool(
            operation_id,
            step_id,
            ToolInvocationId::new(),
            ToolCall {
                id: "large-result".into(),
                name: "counted".into(),
                arguments: json!({"fixture_output": complete}),
            },
        )
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert!(result.output.len() < 8 * 1024);
    let artifact = result
        .artifact
        .clone()
        .expect("typed registered output reference");
    let bytes = ArtifactStore::new(data.path().join("artifacts"))
        .read_bounded(&artifact, 128 * 1024)
        .unwrap();
    assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), complete);
    let message = Message::tool_result(result);
    assert!(normalize_message(&message).iter().any(
        |part| matches!(part, ContentPartV1::Resource(resource) if resource.artifact == *artifact)
    ));
    let mut messages: Vec<_> = (0..100)
        .map(|i| Message::text(Role::Assistant, format!("step {i} completed")))
        .collect();
    messages.push(message.clone());
    let summary = crate::session::CompactionSummary::derive(None, &messages, 16 * 1024);
    assert!(summary.references.iter().any(|reference| {
        reference.contains(&artifact.reference.id.to_string())
            && reference.contains(artifact.reference.content_hash.as_str())
    }));
    permissions.shutdown();
    broker.await.unwrap();
    drop(executor);
    drop(commits);
    let mut session = writer.await.unwrap();
    let head = session.append_message(message).unwrap();
    let (branch, _) = DurableSession::branch_at(data.path(), session.session_id(), head).unwrap();
    assert!(SessionStore::inspect(branch.path()).unwrap().records.iter().any(|record| matches!(&record.record, SessionRecord::ArtifactRegistered { artifact: copied } if copied == artifact.as_ref())));
    branch.discard_staged_branch().unwrap();
    assert!(
        ArtifactStore::new(data.path().join("artifacts"))
            .read_bounded(&artifact, 128 * 1024)
            .is_ok()
    );
    let loaded = SessionStore::inspect(session.path()).unwrap();
    assert!(loaded.records.iter().any(|record| matches!(&record.record, SessionRecord::ConversationEntryAppended { entry } if matches!(&entry.message.content[0], ContentBlock::ToolResult(result) if result.artifact.as_ref() == Some(&artifact)))));

    // Old records have no output metadata; their exact small output survives.
    let old: ToolResult =
        serde_json::from_value(json!({"call_id":"legacy","output":"exact","status":"Success"}))
            .unwrap();
    assert_eq!(old, ToolResult::success("legacy", "exact"));
}

#[tokio::test]
async fn explicit_recovery_prunes_a_new_large_result_only_after_registration() {
    let (data, _workspace, mut session, operation_id, _, _) =
        durable_pending_operation(ReplaySafety::Safe);
    let complete = "recovered evidence 🦀".repeat(4000);
    let effects = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools
        .register(CountedTool {
            output: Some(complete.clone()),
            name: "counted",
            contract_version: 1,
            replay_safety: ReplaySafety::Safe,
            effects: effects.clone(),
        })
        .unwrap();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let (permissions, broker) = PermissionBroker::spawn_for_durable_runtime(
        PermissionPolicy::new(PolicyDecision::Allow, vec![], session.workspace_root()).unwrap(),
        true,
        events,
        [],
    );
    execute_recovery(
        &mut session,
        operation_id,
        &tools,
        &permissions,
        &mut receiver,
        |_| panic!("allow policy"),
    )
    .await
    .unwrap();
    permissions.shutdown();
    broker.await.unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let loaded = SessionStore::inspect(session.path()).unwrap();
    let result = loaded
        .records
        .iter()
        .find_map(|record| match &record.record {
            SessionRecord::ConversationEntryAppended { entry } => {
                entry.message.content.iter().find_map(|block| match block {
                    ContentBlock::ToolResult(result) if result.artifact.is_some() => Some(result),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("recovered tool output has typed metadata");
    assert!(result.output.len() < 8 * 1024);
    let bytes = ArtifactStore::new(data.path().join("artifacts"))
        .read_bounded(result.artifact.as_ref().unwrap(), 256 * 1024)
        .unwrap();
    assert_eq!(serde_json::from_slice::<String>(&bytes).unwrap(), complete);
}
