//! Cross-surface contract tests.
//!
//! These fixtures compare semantic identity and safe degradation. They do not
//! require pixel-identical presentations, native windows, or live providers.

use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    command_catalog::{
        AuthorityRequirement, ColorCapability, CommandContext, CommandSurface,
        PresentationCapabilities, commands_for,
    },
    frontend::semantic::{
        AvailabilityV1, ContentPartV1, ContentProjectionTierV1, ResourceCapabilityContextV1,
        ResourceOperationV1, SemanticDeltaV1, SemanticEventEnvelopeV1, SemanticEventV1,
        SemanticReplicaV1, SemanticSnapshotV1, SubmissionOriginV1, project_content,
        project_resource_capabilities,
    },
    identity::{ArtifactId, OperationId, PrincipalId},
    resource::{
        MediaTypeFactsV1, RESOURCE_SCHEMA_VERSION, ResourceKindV1, ResourceMetadataV1,
        ResourcePolicyV1, ResourceRefV1, ResourceValidationV1,
    },
};

#[test]
fn shared_commands_keep_identity_authority_and_outcomes_on_every_chat_surface() {
    for stable_id in [
        "capability.report.v1",
        "conversation.new.v1",
        "conversation.continue.v1",
        "conversation.preview.v1",
        "conversation.attach.v1",
        "model.select.v1",
        "profile.manage.v1",
        "project.manage.v1",
        "skill.manage.v1",
        "plugin.manage.v1",
        "mcp.manage.v1",
        "external_agent.manage.v1",
        "image.manage.v1",
        "help.contextual.v1",
    ] {
        let projected = [
            CommandSurface::Plain,
            CommandSurface::Tui,
            CommandSurface::Desktop,
        ]
        .map(|surface| {
            commands_for(surface)
                .find(|command| command.stable_id == stable_id)
                .unwrap_or_else(|| panic!("{stable_id} is missing from {surface:?}"))
        });
        let expected = projected[0];
        for command in projected.into_iter().skip(1) {
            assert_eq!(command.action, expected.action, "{stable_id}");
            assert_eq!(command.authority, expected.authority, "{stable_id}");
            assert_eq!(command.confirmation, expected.confirmation, "{stable_id}");
            assert_eq!(command.effect, expected.effect, "{stable_id}");
            assert_eq!(command.success_code, expected.success_code, "{stable_id}");
            assert_eq!(command.error_codes, expected.error_codes, "{stable_id}");
        }
    }
}

#[test]
fn observer_never_gains_mutation_authority_from_a_richer_surface() {
    for surface in [
        CommandSurface::Plain,
        CommandSurface::Tui,
        CommandSurface::Desktop,
    ] {
        let context = CommandContext {
            surface,
            authority: AuthorityRequirement::Observer,
            interactive: true,
            configured: true,
        };
        for command in commands_for(surface) {
            if command.effect != crate::command_catalog::CommandEffect::Inspect {
                assert!(
                    !command.availability(context).enabled,
                    "{} gained authority on {surface:?}",
                    command.stable_id
                );
            }
        }
    }
}

#[test]
fn rich_content_degrades_without_losing_safe_fallbacks_or_adding_actions() {
    let parts = [
        ContentPartV1::Markdown {
            source: "**bounded**".to_owned(),
        },
        ContentPartV1::Math {
            source: "x^2".to_owned(),
            display: true,
        },
        ContentPartV1::Link {
            label: "docs".to_owned(),
            url: "https://example.test/docs".to_owned(),
        },
        ContentPartV1::Resource(Box::new(resource(ResourceKindV1::Video, "video/webm"))),
        ContentPartV1::Unknown {
            version: 99,
            kind: "future_hologram".to_owned(),
            payload: serde_json::json!({"private": "not projected"}),
        },
    ];
    let surfaces = [
        PresentationCapabilities::plain(ColorCapability::None, false),
        PresentationCapabilities::tui(ColorCapability::TrueColor, true, true, true, true),
        PresentationCapabilities::desktop(),
    ];

    for part in &parts {
        let projected = surfaces.map(|capabilities| project_content(part, capabilities));
        for projection in &projected {
            assert!(!projection.fallback_text.trim().is_empty());
            projection.outcome.validate().unwrap();
        }
        assert_eq!(
            projected[0].fallback_text, projected[1].fallback_text,
            "terminal styling must not alter retained meaning"
        );
        assert_eq!(projected[1].fallback_text, projected[2].fallback_text);
        assert!(projected[0].actions.len() <= projected[2].actions.len());
    }

    let video = project_content(&parts[3], PresentationCapabilities::desktop());
    assert_eq!(video.tier, ContentProjectionTierV1::Metadata);
    let unknown = project_content(&parts[4], PresentationCapabilities::desktop());
    assert_eq!(unknown.tier, ContentProjectionTierV1::Unsupported);
    assert!(!unknown.fallback_text.contains("not projected"));
}

#[test]
fn resource_capabilities_keep_permission_and_support_separate_on_every_surface() {
    let resource = resource(ResourceKindV1::StaticRaster, "image/png");
    for presentation in [
        PresentationCapabilities::plain(ColorCapability::None, false),
        PresentationCapabilities::tui(ColorCapability::Ansi256, true, false, false, false),
        PresentationCapabilities::desktop(),
    ] {
        let facts = project_resource_capabilities(
            &resource,
            &ResourceCapabilityContextV1 {
                presentation,
                policy: ResourcePolicyV1::default(),
                observed_at_unix_millis: 1,
                exact_route_facts: Vec::new(),
            },
        )
        .unwrap();
        let external = facts
            .iter()
            .find(|fact| fact.operation == ResourceOperationV1::OpenExternal)
            .unwrap();
        assert!(matches!(
            external.availability,
            AvailabilityV1::PermissionRequired { .. }
        ));
        assert!(!external.authorized);
        let provider = facts
            .iter()
            .find(|fact| fact.operation == ResourceOperationV1::ProviderInput)
            .unwrap();
        assert_eq!(provider.availability, AvailabilityV1::Unsupported);
        assert!(!provider.authorized);
    }
}

#[test]
fn snapshot_delta_and_final_content_converge_before_presentation() {
    let run_id = OperationId::new();
    let events = [
        SemanticEventV1::ContentAppended {
            parts: vec![ContentPartV1::Text {
                text: "working".to_owned(),
            }],
            origin: SubmissionOriginV1::Interactive,
        },
        SemanticEventV1::FinalContent {
            run_id,
            parts: vec![ContentPartV1::Markdown {
                source: "**done**".to_owned(),
            }],
        },
    ];
    let mut replicas = [
        SemanticReplicaV1::from_snapshot(SemanticSnapshotV1::default()).unwrap(),
        SemanticReplicaV1::from_snapshot(SemanticSnapshotV1::default()).unwrap(),
        SemanticReplicaV1::from_snapshot(SemanticSnapshotV1::default()).unwrap(),
    ];
    for (offset, event) in events.into_iter().enumerate() {
        let delta = SemanticDeltaV1 {
            sequence: u64::try_from(offset + 1).unwrap(),
            event: SemanticEventEnvelopeV1::encode(event).unwrap(),
        };
        for replica in &mut replicas {
            replica.apply(delta.clone()).unwrap();
        }
    }
    assert!(
        replicas
            .windows(2)
            .all(|pair| pair[0].snapshot() == pair[1].snapshot())
    );
    assert_eq!(
        replicas[0].snapshot().authoritative_finals[&run_id],
        vec![ContentPartV1::Markdown {
            source: "**done**".to_owned()
        }]
    );
}

fn resource(kind: ResourceKindV1, media_type: &str) -> ResourceRefV1 {
    ResourceRefV1 {
        version: RESOURCE_SCHEMA_VERSION,
        artifact: ArtifactRecord {
            reference: ArtifactRef {
                id: ArtifactId::new(),
                content_hash: ContentHash::for_bytes(b"fixture"),
            },
            media_type: media_type.to_owned(),
            byte_len: 7,
            owner: PrincipalId::new(),
        },
        kind,
        media_type: MediaTypeFactsV1 {
            declared: Some(media_type.to_owned()),
            detected: Some(media_type.to_owned()),
        },
        metadata: ResourceMetadataV1::default(),
        accessibility: None,
        validation: ResourceValidationV1::Accepted,
        lineage: None,
    }
}
