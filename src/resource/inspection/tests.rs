use super::*;
use crate::identity::PrincipalId;
use tempfile::tempdir;

fn inspect(bytes: &[u8], declared: &str) -> ResourceRefV1 {
    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().to_owned());
    let (artifact, _) = store
        .put_bounded(
            bytes,
            declared,
            PrincipalId::new(),
            MAX_RESOURCE_SOURCE_BYTES,
        )
        .unwrap();
    ResourceInspector::new(store, ResourcePolicyV1::default())
        .unwrap()
        .inspect(&artifact)
        .unwrap()
}

#[test]
fn detects_static_and_animated_webp_without_decoding() {
    let mut webp = vec![0_u8; 30];
    webp[..4].copy_from_slice(b"RIFF");
    webp[8..12].copy_from_slice(b"WEBP");
    webp[12..16].copy_from_slice(b"VP8X");
    webp[24] = 1;
    webp[27] = 2;

    let static_resource = inspect(&webp, "image/webp");
    assert_eq!(static_resource.kind, ResourceKindV1::StaticRaster);
    assert_eq!(static_resource.metadata.width, Some(2));
    assert_eq!(static_resource.metadata.height, Some(3));

    webp[20] = 0x02;
    let animated = inspect(&webp, "image/webp");
    assert_eq!(animated.kind, ResourceKindV1::AnimatedRaster);
}

#[test]
fn detected_type_wins_classification_while_declaration_remains_visible() {
    let bytes = [0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0];
    let resource = inspect(&bytes, "image/png");

    assert_eq!(resource.kind, ResourceKindV1::Video);
    assert_eq!(resource.media_type.declared.as_deref(), Some("image/png"));
    assert_eq!(resource.media_type.detected.as_deref(), Some("video/webm"));
    resource.validate().unwrap();
}

#[test]
fn active_document_families_remain_pending_for_reviewed_adapters() {
    let svg = inspect(b"<svg><script>bad()</script></svg>", "image/svg+xml");
    assert_eq!(svg.kind, ResourceKindV1::Svg);
    assert_eq!(svg.validation, ResourceValidationV1::Pending);

    let lottie = inspect(
        br#"{"v":"5.0","layers":[],"assets":[{"u":"https://example.test"}]}"#,
        "application/json",
    );
    assert_eq!(lottie.kind, ResourceKindV1::Lottie);
    assert_eq!(lottie.validation, ResourceValidationV1::Pending);
}

#[test]
fn source_and_metadata_limits_fail_closed_with_stable_codes() {
    let mut png = vec![0_u8; 24];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[12..16].copy_from_slice(b"IHDR");
    png[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000_u32.to_be_bytes());
    let resource = inspect(&png, "image/png");

    assert_eq!(
        resource.validation,
        ResourceValidationV1::Rejected {
            code: "resource.metadata_limit_exceeded".into()
        }
    );
}

#[test]
fn aggregate_preflight_rejects_before_attempting_artifact_io() {
    let directory = tempdir().unwrap();
    let store = ArtifactStore::new(directory.path().to_owned());
    let artifact = ArtifactRecord {
        reference: crate::artifact::ArtifactRef {
            id: crate::identity::ArtifactId::new(),
            content_hash: crate::artifact::ContentHash::for_bytes(b"missing"),
        },
        media_type: "application/octet-stream".into(),
        byte_len: ResourcePolicyV1::default().max_total_source_bytes + 1,
        owner: PrincipalId::new(),
    };
    let result = ResourceInspector::new(store, ResourcePolicyV1::default())
        .unwrap()
        .inspect(&artifact)
        .unwrap();

    assert_eq!(
        result.validation,
        ResourceValidationV1::Rejected {
            code: "resource.turn_source_bytes_exceeded".into()
        }
    );
}
