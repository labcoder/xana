use super::{
    policy::{ResourcePolicyError, StaticRasterPolicyV1, VideoPolicyV1},
    reference::ResourceValidationError,
    *,
};
use crate::{
    artifact::{ArtifactRecord, ArtifactRef, ContentHash},
    identity::{ArtifactId, PrincipalId},
};

fn resource(kind: ResourceKindV1) -> ResourceRefV1 {
    ResourceRefV1 {
        version: RESOURCE_SCHEMA_VERSION,
        artifact: ArtifactRecord {
            reference: ArtifactRef {
                id: ArtifactId::new(),
                content_hash: ContentHash::for_bytes(b"resource"),
            },
            media_type: "image/png".into(),
            byte_len: 8,
            owner: PrincipalId::new(),
        },
        kind,
        media_type: MediaTypeFactsV1 {
            declared: Some("image/png".into()),
            detected: Some("image/png".into()),
        },
        metadata: ResourceMetadataV1 {
            width: Some(1),
            height: Some(1),
            ..ResourceMetadataV1::default()
        },
        accessibility: Some(AccessibilityFactsV1 {
            label: Some("one pixel".into()),
            transcript: None,
            source: AccessibilitySourceV1::User,
        }),
        validation: ResourceValidationV1::Accepted,
        lineage: None,
    }
}

#[test]
fn unknown_resource_kind_round_trips_without_claiming_support() {
    let future = resource(ResourceKindV1::Unknown("future/hologram".into()));
    future.validate().unwrap();
    let encoded = serde_json::to_string(&future).unwrap();
    let decoded: ResourceRefV1 = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, future);
    assert_eq!(decoded.kind.code(), "future/hologram");
}

#[test]
fn resource_reference_rejects_ambient_or_unbounded_values() {
    let mut invalid = resource(ResourceKindV1::StaticRaster);
    invalid.media_type.declared = Some("image/png\r\nsecret: value".into());
    assert!(invalid.validate().is_err());

    let mut invalid = resource(ResourceKindV1::StaticRaster);
    invalid.artifact.byte_len = MAX_RESOURCE_SOURCE_BYTES as u64 + 1;
    assert!(matches!(
        invalid.validate(),
        Err(ResourceValidationError::AboveCompiledCeiling { .. })
    ));
}

#[test]
fn policy_defaults_preserve_existing_image_limits() {
    let policy = ResourcePolicyV1::default();
    policy.validate().unwrap();

    assert_eq!(policy.max_resources_per_turn, 8);
    assert_eq!(policy.static_raster.max_per_turn, 8);
    assert_eq!(policy.static_raster.max_source_bytes, 4 * 1024 * 1024);
    assert_eq!(
        policy.static_raster.max_total_bytes_per_turn,
        20 * 1024 * 1024
    );
    assert_eq!(policy.static_raster.max_pixels, 40_000_000);
}

#[test]
fn zero_unlimited_and_inconsistent_policy_values_fail_closed() {
    let zero = ResourcePolicyV1 {
        max_active_jobs: 0,
        ..ResourcePolicyV1::default()
    };
    assert!(matches!(zero.validate(), Err(ResourcePolicyError::Zero(_))));

    let unlimited = ResourcePolicyV1 {
        video: VideoPolicyV1 {
            max_source_bytes: u64::MAX,
            ..VideoPolicyV1::default()
        },
        ..ResourcePolicyV1::default()
    };
    assert!(matches!(
        unlimited.validate(),
        Err(ResourcePolicyError::AboveCeiling { .. })
    ));

    let inconsistent = ResourcePolicyV1 {
        static_raster: StaticRasterPolicyV1 {
            max_per_turn: ResourcePolicyV1::default()
                .max_resources_per_turn
                .saturating_add(1),
            ..StaticRasterPolicyV1::default()
        },
        ..ResourcePolicyV1::default()
    };
    assert!(matches!(
        inconsistent.validate(),
        Err(ResourcePolicyError::Inconsistent { .. })
    ));
}

#[test]
fn route_policy_can_only_narrow_and_turn_accounting_is_checked() {
    let base = ResourcePolicyV1::default();
    let route = ResourcePolicyV1 {
        max_resources_per_turn: 2,
        max_total_source_bytes: 12,
        static_raster: StaticRasterPolicyV1 {
            max_per_turn: 2,
            max_source_bytes: 6,
            max_total_bytes_per_turn: 12,
            ..StaticRasterPolicyV1::default()
        },
        ..base.clone()
    };
    let effective = base.effective_with(&route).unwrap();

    assert_eq!(effective.max_resources_per_turn, 2);
    assert_eq!(effective.static_raster.max_source_bytes, 6);
    effective.admit_source_lengths([6, 6]).unwrap();
    assert!(matches!(
        effective.admit_source_lengths([6, 7]),
        Err(ResourcePolicyError::TurnLimit { .. })
    ));
    assert!(matches!(
        effective.admit_source_lengths([1, 1, 1]),
        Err(ResourcePolicyError::TurnLimit { .. })
    ));
}
