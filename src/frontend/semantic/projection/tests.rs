use super::*;
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    command_catalog::{ColorCapability, PresentationCapabilities},
    identity::{ArtifactId, PrincipalId},
    message::{Role, ToolCall, ToolResult},
    resource::{
        AccessibilityFactsV1, AccessibilitySourceV1, MediaTypeFactsV1, RESOURCE_SCHEMA_VERSION,
        ResourceMetadataV1, ResourceValidationV1,
    },
};
use serde_json::json;

#[test]
fn normalizes_unambiguous_rich_blocks_and_keeps_hostile_markup_inert() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text("```rust\nfn main() {}\n```".into()),
            ContentBlock::Text("| name | state |\n| --- | --- |\n| Xana | ready |".into()),
            ContentBlock::Text("$$x^2$$".into()),
            ContentBlock::Text("[docs](https://example.test/docs)".into()),
            ContentBlock::Text("<script>alert(1)</script>\u{202e}".into()),
        ],
    };

    let parts = normalize_message(&message);
    assert!(matches!(parts[0], ContentPartV1::Code { .. }));
    assert!(matches!(parts[1], ContentPartV1::Table { .. }));
    assert!(matches!(parts[2], ContentPartV1::Math { .. }));
    assert!(matches!(parts[3], ContentPartV1::Link { .. }));
    assert!(matches!(parts[4], ContentPartV1::Markdown { .. }));
    assert!(!serde_json::to_string(&parts).unwrap().contains("\\u202e"));
    for part in parts {
        part.validate().unwrap();
    }
}

#[test]
fn tool_projection_is_inert_and_does_not_copy_arguments() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "run_command".into(),
                arguments: json!({"api_key":"must-not-cross"}),
            }),
            ContentBlock::ToolResult(ToolResult::success("call-1", "done\u{1b}[31m")),
        ],
    };
    let encoded = serde_json::to_string(&normalize_message(&message)).unwrap();

    assert!(encoded.contains("run_command"));
    assert!(!encoded.contains("must-not-cross"));
    assert!(!encoded.contains("\\u001b"));
}

#[test]
fn projections_are_capability_driven_and_always_keep_readable_fallbacks() {
    let plain = PresentationCapabilities::plain(ColorCapability::None, false);
    let desktop = PresentationCapabilities::desktop();
    let markdown = ContentPartV1::Markdown {
        source: "**hello**".into(),
    };
    assert_eq!(
        project_content(&markdown, plain).tier,
        ContentProjectionTierV1::Text
    );
    assert_eq!(
        project_content(&markdown, desktop).tier,
        ContentProjectionTierV1::Rich
    );

    let resource = ContentPartV1::Resource(Box::new(resource(ResourceKindV1::Lottie)));
    let projection = project_content(&resource, desktop);
    assert_eq!(projection.tier, ContentProjectionTierV1::Metadata);
    assert!(projection.fallback_text.contains("lottie"));
    assert_eq!(projection.actions.len(), 5);

    let future = ContentPartV1::Unknown {
        version: 7,
        kind: "future/hologram".into(),
        payload: json!({"html":"<script>"}),
    };
    let projection = project_content(&future, desktop);
    assert_eq!(projection.tier, ContentProjectionTierV1::Unsupported);
    assert!(!projection.fallback_text.contains("script"));
}

#[test]
fn fresh_recap_is_explicit_and_existing_summaries_are_attributed() {
    let recap = FreshRecapRequestV1::new("openai", "gpt-test").unwrap();
    assert_eq!(recap.connection, "openai");
    assert_eq!(
        recap.outcome.code,
        "summary.fresh_recap.requires_model_request"
    );
    let summary = AttributedSummaryV1::provider_existing(
        "A provider-supplied summary",
        "openai",
        "gpt-test",
        FreshnessV1 {
            observed_at_unix_millis: 10,
            max_age_millis: Some(20),
        },
    )
    .unwrap();
    assert_eq!(summary.source, SummarySourceV1::Provider);
    assert_eq!(
        summary.usage_effect,
        SummaryUsageEffectV1::ExistingProviderRequest
    );
}

#[test]
fn link_preview_cards_are_bounded_untrusted_runtime_facts() {
    let mut card = LinkPreviewCardV1 {
        requested_url: "https://example.test/article".into(),
        final_url: "https://example.test/article".into(),
        site_name: "example.test".into(),
        title: Some("Bounded title".into()),
        fetched_unix_ms: 10,
        media_type: "text/html".into(),
        response_bytes: 12,
        content_digest: "a".repeat(64),
        redirects: Vec::new(),
        text: "safe text".into(),
        text_truncated: false,
        untrusted: true,
        cache_status: LinkPreviewCacheStatusV1::FreshNotCached,
        artifact: None,
    };
    card.validate().unwrap();

    card.untrusted = false;
    assert!(
        card.validate()
            .unwrap_err()
            .to_string()
            .contains("untrusted")
    );
    card.untrusted = true;
    card.final_url = "https://example.test/article#secret".into();
    assert!(
        card.validate()
            .unwrap_err()
            .to_string()
            .contains("fragment")
    );
}

#[test]
fn resource_capabilities_keep_presentation_route_and_authority_independent() {
    let resource = resource(ResourceKindV1::StaticRaster);
    let route = CapabilityFactV1 {
        operation: ResourceOperationV1::ProviderInput,
        availability: AvailabilityV1::Available,
        selected: true,
        authorized: false,
        connection: Some("openai".into()),
        model: Some("gpt-test".into()),
        effective_max_source_bytes: Some(1_024),
        reason_code: None,
        source: FactSourceV1::Model,
        freshness: FreshnessV1 {
            observed_at_unix_millis: 10,
            max_age_millis: Some(20),
        },
    };
    let facts = project_resource_capabilities(
        &resource,
        &ResourceCapabilityContextV1 {
            presentation: PresentationCapabilities::desktop(),
            policy: ResourcePolicyV1::default(),
            observed_at_unix_millis: 11,
            exact_route_facts: vec![route],
        },
    )
    .unwrap();

    let present = facts
        .iter()
        .find(|fact| fact.operation == ResourceOperationV1::PresentInline)
        .unwrap();
    assert_eq!(present.availability, AvailabilityV1::Available);
    assert!(present.authorized);
    let provider = facts
        .iter()
        .find(|fact| fact.operation == ResourceOperationV1::ProviderInput)
        .unwrap();
    assert!(provider.selected);
    assert!(!provider.authorized);
    assert_eq!(provider.connection.as_deref(), Some("openai"));
    assert!(matches!(
        facts
            .iter()
            .find(|fact| fact.operation == ResourceOperationV1::FocusedAnalysis)
            .unwrap()
            .availability,
        AvailabilityV1::Unsupported
    ));
}

fn resource(kind: ResourceKindV1) -> ResourceRefV1 {
    ResourceRefV1 {
        version: RESOURCE_SCHEMA_VERSION,
        artifact: ArtifactRecord {
            reference: ArtifactRef {
                id: ArtifactId::new(),
                content_hash: ContentHash::for_bytes(b"resource"),
            },
            media_type: "application/json".into(),
            byte_len: 8,
            owner: PrincipalId::new(),
        },
        kind,
        media_type: MediaTypeFactsV1 {
            declared: Some("application/json".into()),
            detected: Some("application/json".into()),
        },
        metadata: ResourceMetadataV1::default(),
        accessibility: Some(AccessibilityFactsV1 {
            label: None,
            transcript: None,
            source: AccessibilitySourceV1::Unavailable,
        }),
        validation: ResourceValidationV1::Accepted,
        lineage: None,
    }
}
