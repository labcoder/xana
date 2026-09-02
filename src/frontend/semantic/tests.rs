use super::{
    activity::{
        ActivityDisclosureV1, ActivityOwnerV1, ActivityStateV1, ApprovalStateV1, AttentionKindV1,
        CheckReceiptV1, CompletionStatusV1, ExecutionOwnerV1, HostLocationV1, WorkspaceAuthorityV1,
        validate_activity_tree,
    },
    content::{
        AttachmentProvenanceV1, CapabilityFactV1, DisclosureDecisionV1, ResourceOperationV1,
    },
    event::{
        DecodedSemanticEventV1, SemanticEventEnvelopeV1, SemanticEventV1, SubmissionOriginV1,
        SurfaceCapabilityV1, UnknownSemanticV1,
    },
    state::ApplyDeltaResult,
    *,
};
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    identity::{ArtifactId, ConversationId, OperationId, PrincipalId, SessionId, ToolInvocationId},
    resource::{
        AccessibilityFactsV1, AccessibilitySourceV1, MediaTypeFactsV1, RESOURCE_SCHEMA_VERSION,
        ResourceKindV1, ResourceMetadataV1, ResourceRefV1, ResourceValidationV1,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

fn freshness() -> FreshnessV1 {
    FreshnessV1 {
        observed_at_unix_millis: 1_000,
        max_age_millis: Some(30_000),
    }
}

fn resource(kind: ResourceKindV1, media_type: &str, bytes: u64) -> ResourceRefV1 {
    ResourceRefV1 {
        version: RESOURCE_SCHEMA_VERSION,
        artifact: ArtifactRecord {
            reference: ArtifactRef {
                id: ArtifactId::new(),
                content_hash: ContentHash::for_bytes(media_type.as_bytes()),
            },
            media_type: media_type.into(),
            byte_len: bytes,
            owner: PrincipalId::new(),
        },
        kind,
        media_type: MediaTypeFactsV1 {
            declared: Some(media_type.into()),
            detected: Some(media_type.into()),
        },
        metadata: ResourceMetadataV1::default(),
        accessibility: Some(AccessibilityFactsV1 {
            label: Some("A test resource".into()),
            transcript: None,
            source: AccessibilitySourceV1::User,
        }),
        validation: ResourceValidationV1::Accepted,
        lineage: None,
    }
}

fn attachment() -> AttachmentV1 {
    AttachmentV1 {
        id: Uuid::new_v4(),
        resource: resource(ResourceKindV1::StaticRaster, "image/png", 128),
        provenance: AttachmentProvenanceV1::UserSelected,
        source_label: Some("example.png".into()),
        capabilities: vec![CapabilityFactV1 {
            operation: ResourceOperationV1::PresentInline,
            availability: AvailabilityV1::Available,
            selected: false,
            authorized: false,
            connection: None,
            model: None,
            effective_max_source_bytes: Some(4 * 1024 * 1024),
            reason_code: None,
            source: FactSourceV1::Runtime,
            freshness: freshness(),
        }],
    }
}

fn usage(
    id: Uuid,
    scope: UsageScopeV1,
    period: &str,
    accounting: UsageAccountingV1,
    input: Option<u64>,
) -> UsageObservationV1 {
    UsageObservationV1 {
        id,
        scope,
        period: period.into(),
        accounting,
        amounts: UsageAmountsV1 {
            input_tokens: input,
            cached_input_tokens: Some(0),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(2),
            reasoning_tokens: Some(1),
            tool_tokens: Some(0),
            request_count: Some(1),
            prompt_bytes: Some(8),
            tool_schema_bytes: Some(0),
            cost_microunits: Some(3),
        },
        context: None,
        rate_limit: None,
        quota: None,
        credits: None,
        request_affinity_digest: None,
        availability: AvailabilityV1::Available,
        source: FactSourceV1::Provider,
        authority: FactAuthorityV1::ProviderReported,
        freshness: freshness(),
    }
}

fn activity(conversation_id: ConversationId, run_id: OperationId) -> ActivityItemV1 {
    ActivityItemV1 {
        id: Uuid::new_v4(),
        parent_id: None,
        conversation_id,
        run_id: Some(run_id),
        owner: ActivityOwnerV1::XanaRoot,
        state: ActivityStateV1::Working,
        summary: SemanticCodeV1::new("activity.working"),
        disclosed_text: Some("Reading the visible project index".into()),
        disclosure: ActivityDisclosureV1::Summary,
        source: FactSourceV1::Runtime,
        freshness: freshness(),
        started_at_unix_millis: Some(1_000),
        finished_at_unix_millis: None,
    }
}

fn execution(conversation_id: ConversationId, run_id: OperationId) -> ExecutionFactsV1 {
    ExecutionFactsV1 {
        conversation_id,
        run_id,
        owner: ExecutionOwnerV1::Native,
        host: HostLocationV1::Embedded,
        workspace_authority: WorkspaceAuthorityV1::WorkspaceWrite,
        tool_authority: vec!["workspace.read".into()],
        connection: Some("local".into()),
        model: Some("test-model".into()),
        capability_grants: vec!["tool.read_file".into()],
        egress_policy: Some("deny".into()),
        controller: Some("desktop".into()),
        approval_policy: "ask".into(),
        source: FactSourceV1::Runtime,
        freshness: freshness(),
    }
}

#[test]
fn rich_content_round_trips_and_unsafe_links_fail_closed() {
    let parts = vec![
        ContentPartV1::Text {
            text: "hello".into(),
        },
        ContentPartV1::Markdown {
            source: "**hello**".into(),
        },
        ContentPartV1::Code {
            language: Some("rust".into()),
            code: "fn main() {}".into(),
        },
        ContentPartV1::Table {
            columns: vec!["name".into()],
            rows: vec![vec!["Xana".into()]],
        },
        ContentPartV1::Diff {
            patch: "-old\n+new".into(),
        },
        ContentPartV1::Math {
            source: "x^2".into(),
            display: true,
        },
        ContentPartV1::Link {
            label: "docs".into(),
            url: "https://example.test/docs".into(),
        },
        ContentPartV1::Resource(Box::new(resource(
            ResourceKindV1::Unknown("future/hologram".into()),
            "application/octet-stream",
            10,
        ))),
        ContentPartV1::Unknown {
            version: 2,
            kind: "future_context".into(),
            payload: json!({"safe_summary": "unsupported"}),
        },
    ];
    for part in &parts {
        part.validate().unwrap();
    }
    let encoded = serde_json::to_vec(&parts).unwrap();
    assert_eq!(
        serde_json::from_slice::<Vec<ContentPartV1>>(&encoded).unwrap(),
        parts
    );

    assert!(
        ContentPartV1::Link {
            label: "secret".into(),
            url: "https://user:password@example.test".into(),
        }
        .validate()
        .is_err()
    );

    let future: ContentPartV1 = serde_json::from_value(json!({
        "version": 1,
        "kind": "memory_preview",
        "payload": {"label": "not enabled"}
    }))
    .unwrap();
    assert!(matches!(
        future,
        ContentPartV1::Unknown {
            version: 1,
            ref kind,
            ..
        } if kind == "memory_preview"
    ));
}

#[test]
fn attachment_policy_requires_valid_refs_and_never_accepts_ambient_paths() {
    let valid = attachment();
    AttachmentPolicySnapshotV1::default()
        .validate_attachments(std::slice::from_ref(&valid))
        .unwrap();

    let mut invalid = valid;
    invalid.source_label = Some(r"C:\Users\someone\secret.png".into());
    assert!(invalid.validate().is_err());
}

#[test]
fn unknown_event_versions_degrade_to_bounded_inspectable_values() {
    let envelope = SemanticEventEnvelopeV1 {
        version: 99,
        kind: "future_memory".into(),
        payload: json!({"status": "unsupported"}),
    };
    assert!(matches!(
        envelope.decode().unwrap(),
        DecodedSemanticEventV1::Unknown(UnknownSemanticV1 { version: 99, .. })
    ));

    let oversized = SemanticEventEnvelopeV1 {
        payload: json!({"value": "x".repeat(65 * 1024)}),
        ..envelope
    };
    assert!(matches!(
        oversized.decode(),
        Err(SemanticError::PayloadTooLarge { .. })
    ));
}

#[test]
fn snapshot_replay_is_ordered_and_a_gap_requires_a_fresh_snapshot() {
    let run_id = OperationId::new();
    let mut replica = SemanticReplicaV1::from_snapshot(SemanticSnapshotV1::default()).unwrap();
    let first = SemanticDeltaV1 {
        sequence: 1,
        event: SemanticEventEnvelopeV1::encode(SemanticEventV1::ContentAppended {
            parts: vec![ContentPartV1::Text { text: "one".into() }],
            origin: SubmissionOriginV1::Interactive,
        })
        .unwrap(),
    };
    assert_eq!(
        replica.apply(first.clone()).unwrap(),
        ApplyDeltaResult::Applied
    );
    assert_eq!(replica.apply(first).unwrap(), ApplyDeltaResult::Duplicate);
    assert_eq!(
        replica
            .apply(SemanticDeltaV1 {
                sequence: 3,
                event: SemanticEventEnvelopeV1::encode(SemanticEventV1::FinalContent {
                    run_id,
                    parts: vec![ContentPartV1::Text {
                        text: "final".into(),
                    }],
                })
                .unwrap(),
            })
            .unwrap(),
        ApplyDeltaResult::NeedsFreshSnapshot
    );
    assert_eq!(replica.snapshot().sequence, 1);

    let mut fresh = replica.snapshot().clone();
    fresh.sequence = 3;
    fresh.authoritative_finals.insert(
        run_id,
        vec![ContentPartV1::Text {
            text: "authoritative".into(),
        }],
    );
    replica.install_snapshot(fresh).unwrap();
    assert_eq!(replica.snapshot().sequence, 3);
    assert!(
        replica
            .snapshot()
            .authoritative_finals
            .contains_key(&run_id)
    );
}

#[test]
fn progress_uses_deltas_and_converges_on_authoritative_final_content() {
    let conversation_id = ConversationId::for_native(SessionId::new());
    let run_id = OperationId::new();
    let activity = activity(conversation_id, run_id);
    let activity_id = activity.id;
    let snapshot = SemanticSnapshotV1 {
        conversation_id: Some(conversation_id),
        activity: vec![activity],
        ..SemanticSnapshotV1::default()
    };
    snapshot.validate().unwrap();
    let mut replica = SemanticReplicaV1::from_snapshot(snapshot).unwrap();
    for (sequence, delta) in [(1, "a"), (2, "b"), (3, "c")] {
        let event = SemanticEventV1::ProgressTextDelta {
            activity_id,
            delta: delta.into(),
        };
        let encoded = serde_json::to_vec(&SemanticEventEnvelopeV1::encode(event).unwrap()).unwrap();
        assert!(encoded.len() < 512, "delta must not repeat cumulative text");
        replica
            .apply(SemanticDeltaV1 {
                sequence,
                event: SemanticEventEnvelopeV1::encode(SemanticEventV1::ProgressTextDelta {
                    activity_id,
                    delta: delta.into(),
                })
                .unwrap(),
            })
            .unwrap();
    }
    replica
        .apply(SemanticDeltaV1 {
            sequence: 4,
            event: SemanticEventEnvelopeV1::encode(SemanticEventV1::FinalContent {
                run_id,
                parts: vec![ContentPartV1::Text {
                    text: "finished".into(),
                }],
            })
            .unwrap(),
        })
        .unwrap();
    assert_eq!(
        replica.snapshot().activity[0].disclosed_text.as_deref(),
        Some("Reading the visible project indexabc")
    );
    assert_eq!(
        replica.snapshot().authoritative_finals[&run_id],
        vec![ContentPartV1::Text {
            text: "finished".into()
        }]
    );
}

#[test]
fn usage_aggregation_deduplicates_deltas_and_replaces_cumulative_snapshots() {
    let run_id = OperationId::new();
    let scope = UsageScopeV1::Run { run_id };
    let mut ledger = UsageLedgerV1::default();
    let delta = usage(
        Uuid::new_v4(),
        scope.clone(),
        "turn-1",
        UsageAccountingV1::Delta,
        Some(5),
    );
    assert!(ledger.observe(delta.clone()).unwrap());
    assert!(!ledger.observe(delta).unwrap());
    assert!(
        ledger
            .observe(usage(
                Uuid::new_v4(),
                scope.clone(),
                "turn-1",
                UsageAccountingV1::CumulativeSnapshot { sequence: 2 },
                Some(10),
            ))
            .unwrap()
    );
    assert!(
        !ledger
            .observe(usage(
                Uuid::new_v4(),
                scope.clone(),
                "turn-1",
                UsageAccountingV1::CumulativeSnapshot { sequence: 1 },
                Some(100),
            ))
            .unwrap()
    );
    assert!(
        ledger
            .observe(usage(
                Uuid::new_v4(),
                scope.clone(),
                "turn-1",
                UsageAccountingV1::CumulativeSnapshot { sequence: 3 },
                Some(12),
            ))
            .unwrap()
    );

    let aggregate = ledger.aggregate(&scope, "turn-1").unwrap();
    assert_eq!(aggregate.amounts.input_tokens, Some(17));
    assert_eq!(aggregate.observation_count, 2);
    assert!(!aggregate.incomplete);
    assert_eq!(
        ledger
            .aggregate(&scope, "turn-2")
            .unwrap()
            .amounts
            .input_tokens,
        None,
        "a reset period must not inherit the previous window"
    );
}

#[test]
fn unavailable_usage_is_not_treated_as_zero() {
    let run_id = OperationId::new();
    let scope = UsageScopeV1::Run { run_id };
    let mut observation = usage(
        Uuid::new_v4(),
        scope.clone(),
        "turn",
        UsageAccountingV1::Delta,
        None,
    );
    observation.availability = AvailabilityV1::Unavailable {
        code: "provider.no_usage".into(),
    };
    let mut ledger = UsageLedgerV1::default();
    ledger.observe(observation).unwrap();
    let aggregate = ledger.aggregate(&scope, "turn").unwrap();

    assert_eq!(aggregate.amounts.input_tokens, None);
    assert!(aggregate.incomplete);
}

#[test]
fn attention_acknowledgement_is_exact_and_opening_other_conversations_has_no_effect() {
    let first_conversation = ConversationId::for_native(SessionId::new());
    let second_conversation = ConversationId::for_native(SessionId::new());
    let first = AttentionItemV1 {
        id: Uuid::new_v4(),
        conversation_id: first_conversation,
        run_id: None,
        kind: AttentionKindV1::NeedsYou,
        message: SemanticCodeV1::new("attention.approval_required"),
        created_at_unix_millis: 1,
        acknowledged_at_unix_millis: None,
    };
    let second = AttentionItemV1 {
        id: Uuid::new_v4(),
        conversation_id: second_conversation,
        kind: AttentionKindV1::Completed,
        ..first.clone()
    };
    let mut state = AttentionStateV1::default();
    state.upsert(first.clone()).unwrap();
    state.upsert(second.clone()).unwrap();
    assert!(state.acknowledge(first.id, 10));

    let retained = state.values().collect::<Vec<_>>();
    assert_eq!(retained.len(), 2);
    assert_eq!(
        retained
            .iter()
            .find(|item| item.id == first.id)
            .unwrap()
            .acknowledged_at_unix_millis,
        Some(10)
    );
    assert_eq!(
        retained
            .iter()
            .find(|item| item.id == second.id)
            .unwrap()
            .acknowledged_at_unix_millis,
        None
    );
}

#[test]
fn owner_qualified_activity_rejects_cycles_and_cross_conversation_parents() {
    let conversation = ConversationId::for_native(SessionId::new());
    let run = OperationId::new();
    let mut parent = activity(conversation, run);
    let mut child = activity(conversation, run);
    child.parent_id = Some(parent.id);
    child.owner = ActivityOwnerV1::NativeChild {
        agent_id: crate::identity::AgentId::new(),
    };
    validate_activity_tree(&[parent.clone(), child.clone()]).unwrap();

    parent.parent_id = Some(child.id);
    assert!(validate_activity_tree(&[parent, child]).is_err());
}

#[test]
fn completion_receipt_binds_execution_identity_and_bounded_evidence() {
    let conversation_id = ConversationId::for_native(SessionId::new());
    let run_id = OperationId::new();
    let receipt = CompletionReceiptV1 {
        id: Uuid::new_v4(),
        conversation_id,
        run_id,
        status: CompletionStatusV1::Completed,
        execution: execution(conversation_id, run_id),
        artifacts: vec![ArtifactId::new()],
        checks: vec![CheckReceiptV1 {
            code: "tests.workspace".into(),
            passed: true,
        }],
        usage: UsageAggregateV1::default(),
        unresolved_warnings: Vec::new(),
        source: FactSourceV1::Runtime,
        authority: FactAuthorityV1::Authoritative,
        freshness: freshness(),
    };
    receipt.validate().unwrap();

    let mut mismatched = receipt;
    mismatched.execution.run_id = OperationId::new();
    assert!(mismatched.validate().is_err());
}

#[test]
fn capability_projection_does_not_confuse_availability_selection_and_authority() {
    let snapshot = SemanticSnapshotV1 {
        capabilities: vec![SurfaceCapabilityV1 {
            id: "vision.image_input".into(),
            availability: AvailabilityV1::Available,
            selected: false,
            authorized: false,
            source: FactSourceV1::Runtime,
            freshness: freshness(),
        }],
        ..SemanticSnapshotV1::default()
    };
    snapshot.validate().unwrap();
    assert!(!snapshot.capabilities[0].selected);
    assert!(!snapshot.capabilities[0].authorized);
}

#[test]
fn semantic_schema_has_no_secret_or_executable_authority_fields() {
    let conversation_id = ConversationId::for_native(SessionId::new());
    let run_id = OperationId::new();
    let snapshot = SemanticSnapshotV1 {
        conversation_id: Some(conversation_id),
        attachments: vec![attachment()],
        approvals: vec![ApprovalV1 {
            invocation_id: ToolInvocationId::new(),
            conversation_id,
            run_id,
            capability: "workspace.read".into(),
            request: SemanticCodeV1::new("approval.workspace_read"),
            state: ApprovalStateV1::Pending,
        }],
        execution_facts: vec![execution(conversation_id, run_id)],
        capabilities: vec![SurfaceCapabilityV1 {
            id: "workspace.read".into(),
            availability: AvailabilityV1::Available,
            selected: true,
            authorized: false,
            source: FactSourceV1::Runtime,
            freshness: freshness(),
        }],
        disclosures: vec![DisclosureReceiptV1 {
            id: Uuid::new_v4(),
            resource_id: ArtifactId::new(),
            operation: ResourceOperationV1::ProviderInput,
            destination: "provider".into(),
            connection: Some("local".into()),
            model: Some("test-model".into()),
            source_bytes: 10,
            derivative: None,
            decision: DisclosureDecisionV1::Denied,
            decided_at_unix_millis: 5,
        }],
        ..SemanticSnapshotV1::default()
    };
    snapshot.validate().unwrap();
    let value = serde_json::to_value(snapshot).unwrap();
    let forbidden = [
        "password",
        "api_key",
        "access_token",
        "refresh_token",
        "authorization",
        "callback",
        "filesystem_path",
        "provider_object",
        "chain_of_thought",
    ];
    assert_no_forbidden_keys(&value, &forbidden);
}

#[test]
fn localization_changes_copy_without_changing_semantic_action() {
    let mut code = SemanticCodeV1::new("attention.approval_required");
    code.parameters
        .insert("count".into(), SemanticParamV1::Unsigned(2));
    code.validate().unwrap();
    let pseudolocalized = format!("[!! {} ··· !!]", code.code.replace('.', " · "));

    assert!(pseudolocalized.contains("approval_required"));
    assert_eq!(code.code, "attention.approval_required");
    assert_eq!(code.parameters["count"], SemanticParamV1::Unsigned(2));
}

#[test]
fn future_voice_origin_carries_only_adapter_identity_and_idempotency() {
    let origin = SubmissionOriginV1::VoiceAdapter {
        adapter: "future/stt".into(),
        request_id: Uuid::new_v4(),
    };
    let event = SemanticEventEnvelopeV1::encode(SemanticEventV1::ContentAppended {
        parts: vec![ContentPartV1::Text {
            text: "transcribed text".into(),
        }],
        origin,
    })
    .unwrap();
    let encoded = serde_json::to_string(&event).unwrap();

    assert!(!encoded.contains("pcm"));
    assert!(!encoded.contains("microphone"));
    assert!(!encoded.contains("realtime_handle"));
}

fn assert_no_forbidden_keys(value: &Value, forbidden: &[&str]) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(!forbidden.contains(&key.as_str()), "forbidden key {key}");
                assert_no_forbidden_keys(value, forbidden);
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_forbidden_keys(value, forbidden);
            }
        }
        _ => {}
    }
}
