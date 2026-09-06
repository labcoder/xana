use super::*;
mod durable;
use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    message::{ContentBlock, Message, Role, ToolCall, ToolResult},
};

fn finite(contract: CompletionContract) -> CompletionEvidence {
    let mut value = CompletionEvidence::new(
        OperationId::new(),
        WorkKind::Root,
        EvidenceOwner::Native,
        CompletionClaim::Completed,
        contract,
    )
    .unwrap();
    value.revision = 2;
    value.delivered(b"the model says done");
    value
}

fn command_messages(code: i32) -> Vec<Message> {
    let mut result = ToolResult::success("check", "untrusted output says all checks passed");
    result.command_status = Some(CommandStatus {
        success: code == 0,
        exit_code: Some(code),
    });
    vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "check".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({"command":"cargo test","cwd":"."}),
            })],
        },
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult(result)],
        },
    ]
}

fn command_contract() -> CompletionContract {
    CompletionContract {
        conditions: vec![AcceptanceCondition::CommandSucceeded {
            command: "cargo test".into(),
            cwd: ".".into(),
        }],
    }
}

#[test]
fn model_claim_does_not_replace_missing_failed_or_stale_check() {
    let mut missing = finite(command_contract());
    missing.evaluate();
    assert_eq!(missing.outcome, EvidenceOutcome::NeedsAttention);
    let mut failed = finite(command_contract());
    from_messages(&mut failed, &command_messages(1));
    failed.evaluate();
    assert_eq!(failed.outcome, EvidenceOutcome::Incomplete);
    let mut current = finite(command_contract());
    from_messages(&mut current, &command_messages(0));
    current.evaluate();
    assert_eq!(current.outcome, EvidenceOutcome::ConditionsVerified);
    current.work_revision += 1;
    current.evaluate();
    assert_eq!(current.outcome, EvidenceOutcome::NeedsAttention);
    current.checks[0].generation = OperationId::new();
    assert!(current.validate().is_err());
}

#[test]
fn open_delivery_is_not_independent_correctness_and_unknown_effect_never_passes() {
    let mut value = finite(Default::default());
    value.evaluate();
    assert_eq!(value.outcome, EvidenceOutcome::DeliveryVerified);
    assert!(value.summary().contains("not independently checked"));
    value.effects.push(EffectEvidence {
        invocation: ToolInvocationId::new(),
        generation: value.generation,
        outcome: EffectOutcome::Unknown,
        replay_safe: false,
    });
    value.evaluate();
    assert_eq!(value.outcome, EvidenceOutcome::NeedsAttention);
    value.effects.push(value.effects[0].clone());
    assert!(value.validate().is_err());
}

#[test]
fn artifact_verifier_is_reserved_once_and_replaced_bytes_fail() {
    let data = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(data.path().to_owned());
    let (artifact, _) = store
        .put(b"immutable", "text/plain", PrincipalId::new())
        .unwrap();
    let mut value = finite(CompletionContract {
        conditions: vec![AcceptanceCondition::ArtifactPresent {
            artifact: artifact.reference.clone(),
        }],
    });
    add_artifact(&mut value, artifact.clone());
    assert!(value.reserve_verifier(true, false).unwrap());
    assert!(value.reserve_verifier(true, false).is_err());
    let verified = verify_artifacts(
        value.clone(),
        &store,
        &tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(verified.outcome, EvidenceOutcome::ConditionsVerified);
    assert!(valid_transition(Some(&value), &verified));
    let path = store
        .verified_path(&artifact, MAX_VERIFY_BYTES as usize)
        .unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    let replaced = verify_artifacts(
        value.clone(),
        &store,
        &tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(replaced.verification, VerificationState::Failed);
    assert!(
        verify_artifacts(
            verified,
            &store,
            &tokio_util::sync::CancellationToken::new()
        )
        .is_err()
    );
    let other = ArtifactStore::new(data.path().join("missing"));
    let absent =
        verify_artifacts(value, &other, &tokio_util::sync::CancellationToken::new()).unwrap();
    assert_eq!(absent.verification, VerificationState::Failed);
    assert!(!absent.supported());
}

#[test]
fn verifier_cancel_unavailable_exhausted_and_durable_transition_fail_closed() {
    let data = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(data.path().to_owned());
    let (artifact, _) = store
        .put(b"receipt", "text/plain", PrincipalId::new())
        .unwrap();
    for (authorized, cancelled, exhausted, expected) in [
        (false, false, false, VerificationState::Unavailable),
        (true, true, false, VerificationState::Cancelled),
        (true, false, true, VerificationState::Unavailable),
    ] {
        let mut value = finite(Default::default());
        add_artifact(&mut value, artifact.clone());
        value.budget.exhausted = exhausted;
        assert!(!value.reserve_verifier(authorized, cancelled).unwrap());
        assert_eq!(value.verification, expected);
        assert!(!value.supported());
    }
    let mut declaration = CompletionEvidence::new(
        OperationId::new(),
        WorkKind::Root,
        EvidenceOwner::Native,
        CompletionClaim::Interrupted,
        Default::default(),
    )
    .unwrap();
    declaration.evaluate();
    assert!(valid_transition(None, &declaration));
    let mut result = declaration.clone();
    result.revision = 2;
    result.claim = CompletionClaim::Completed;
    result.delivered(b"result");
    result.evaluate();
    assert!(valid_transition(Some(&declaration), &result));
    assert!(!valid_transition(Some(&result), &result));
    result
        .contract
        .conditions
        .push(AcceptanceCondition::Declared {
            id: "changed".into(),
            revision: ContentHash::for_bytes(b"v2"),
        });
    result.contract_digest = result.contract.digest();
    result.evaluate();
    assert!(!valid_transition(Some(&declaration), &result));
}

#[test]
fn exact_fresh_command_pass_resolves_known_failed_check_but_never_unknown_effect() {
    let mut value = finite(command_contract());
    let mut messages = command_messages(1);
    messages.extend(command_messages(0));
    from_messages(&mut value, &messages);
    value.evaluate();
    assert_eq!(value.outcome, EvidenceOutcome::ConditionsVerified);
    value.effects[0].outcome = EffectOutcome::Failed;
    value.evaluate();
    assert!(value.supported());
    value.effects[0].outcome = EffectOutcome::Unknown;
    value.evaluate();
    assert_eq!(value.outcome, EvidenceOutcome::NeedsAttention);
    value.effects[0].outcome = EffectOutcome::Acknowledged;
    value.checks[1].command_digest = command_digest("other", ".");
    value.evaluate();
    assert!(!value.supported());
}

#[test]
fn unacknowledged_mutating_failure_and_duplicate_pending_call_cannot_pass() {
    for (name, outcome) in [
        ("write_file", EffectOutcome::Unknown),
        ("read_file", EffectOutcome::Failed),
    ] {
        let mut value = finite(Default::default());
        let call = ToolCall {
            id: "effect".into(),
            name: name.into(),
            arguments: serde_json::json!({"path":"note.txt"}),
        };
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(call.clone())],
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult(ToolResult::error(
                    "effect",
                    "transport disappeared",
                ))],
            },
        ];
        from_messages(&mut value, &messages);
        value.evaluate();
        assert_eq!(value.effects[0].outcome, outcome);
        assert!(!value.supported());

        let mut duplicate = finite(Default::default());
        let mut messages = messages;
        messages.insert(
            0,
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(call)],
            },
        );
        from_messages(&mut duplicate, &messages);
        duplicate.evaluate();
        assert!(duplicate.omitted_observations);
        assert!(!duplicate.supported());
    }
}

#[test]
fn cancelled_reserved_verifier_is_terminal_and_cannot_restart() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().to_owned());
    let (artifact, _) = store
        .put(b"immutable", "text/plain", PrincipalId::new())
        .unwrap();
    let mut value = finite(Default::default());
    add_artifact(&mut value, artifact);
    value.reserve_verifier(true, false).unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    let cancelled = verify_artifacts(value.clone(), &store, &cancellation).unwrap();
    assert_eq!(cancelled.verification, VerificationState::Cancelled);
    assert!(!cancelled.supported());
    assert!(valid_transition(Some(&value), &cancelled));
    assert!(verify_artifacts(cancelled, &store, &cancellation).is_err());
}

#[test]
fn exact_allowance_can_finish_but_cannot_admit_additional_verification() {
    let mut value = finite(command_contract());
    from_messages(&mut value, &command_messages(0));
    value.budget.remaining_requests = Some(0);
    value.budget.exhausted = true;
    value.evaluate();
    assert_eq!(value.outcome, EvidenceOutcome::ConditionsVerified);
    let store_home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(store_home.path().to_owned());
    let (artifact, _) = store
        .put(b"additional receipt", "text/plain", PrincipalId::new())
        .unwrap();
    add_artifact(&mut value, artifact);
    assert!(!value.reserve_verifier(true, false).unwrap());
    assert_eq!(value.verification, VerificationState::Unavailable);
    assert!(!value.supported());
    let mut over = finite(Default::default());
    over.budget.exceeded = true;
    over.evaluate();
    assert_eq!(over.outcome, EvidenceOutcome::Incomplete);
}
