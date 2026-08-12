use super::*;
use std::{collections::VecDeque, ffi::OsString};
use tempfile::TempDir;

fn all_classes() -> BTreeSet<OutboundDataClass> {
    OutboundDataClass::ALL.into_iter().collect()
}

fn classes(mask: u8) -> BTreeSet<OutboundDataClass> {
    OutboundDataClass::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(index, class)| (mask & (1 << index) != 0).then_some(class))
        .collect()
}

fn recipient(identity: &[u8]) -> RecipientIdentity {
    RecipientIdentity::new(
        RecipientKind::ExternalAgent,
        "reviewer",
        "https://agent.example.test/a2a",
        identity,
    )
    .unwrap()
}

fn item(class: OutboundDataClass, bytes: &[u8]) -> OutboundItem {
    OutboundItem::new(
        class,
        "selected item",
        Some("artifact:example".to_owned()),
        "explicit user selection",
        bytes.to_vec(),
    )
    .unwrap()
}

fn request(recipient: RecipientIdentity, items: Vec<OutboundItem>) -> OutboundRequest {
    OutboundRequest::new(
        OperationId::new(),
        recipient,
        "perform the reviewed task",
        items,
    )
    .unwrap()
}

fn policy(allowed: BTreeSet<OutboundDataClass>) -> OutboundPolicyLayers {
    OutboundPolicyLayers {
        connection_allowed: allowed.clone(),
        user_ceiling: allowed.clone(),
        profile_allowed: allowed,
        conversation_allowed: None,
    }
}

fn guard() -> (TempDir, OutboundGuard) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
    let guard = OutboundGuard::open(&paths).unwrap();
    (directory, guard)
}

#[derive(Default)]
struct Controller {
    decisions: VecDeque<OutboundApprovalDecision>,
    reviews: Vec<OutboundApprovalRequest>,
}

impl Controller {
    fn with(decisions: impl IntoIterator<Item = OutboundApprovalDecision>) -> Self {
        Self {
            decisions: decisions.into_iter().collect(),
            reviews: Vec::new(),
        }
    }
}

impl OutboundApprovalController for Controller {
    fn decide<'a>(
        &'a mut self,
        request: &'a OutboundApprovalRequest,
    ) -> BoxFuture<'a, OutboundApprovalDecision> {
        self.reviews.push(request.clone());
        let decision = self.decisions.pop_front().expect("test decision");
        Box::pin(async move { decision })
    }
}

#[derive(Default)]
struct FakeTransport {
    calls: usize,
    sent: Vec<Vec<u8>>,
    failure: Option<OutboundTransportFailure>,
}

impl OutboundTransport for FakeTransport {
    type Receipt = usize;

    fn send<'a>(
        &'a mut self,
        _recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        self.calls += 1;
        self.sent
            .extend(items.iter().map(|item| item.bytes().to_vec()));
        let failure = self.failure;
        let byte_count = items.iter().map(|item| item.bytes().len()).sum();
        Box::pin(async move { failure.map_or(Ok(byte_count), Err) })
    }
}

#[test]
fn every_policy_layer_only_narrows() {
    for connection_mask in 0_u8..64 {
        for user_mask in 0_u8..64 {
            for profile_mask in 0_u8..64 {
                let conversation_mask = connection_mask.rotate_left(1) ^ user_mask;
                let layers = OutboundPolicyLayers {
                    connection_allowed: classes(connection_mask),
                    user_ceiling: classes(user_mask),
                    profile_allowed: classes(profile_mask),
                    conversation_allowed: Some(classes(conversation_mask)),
                };
                let effective = layers.effective();
                assert!(effective.is_subset(&layers.connection_allowed));
                assert!(effective.is_subset(&layers.user_ceiling));
                assert!(effective.is_subset(&layers.profile_allowed));
                assert!(effective.is_subset(layers.conversation_allowed.as_ref().unwrap()));
                for class in OutboundDataClass::ALL {
                    assert_eq!(
                        effective.contains(&class),
                        layers.connection_allowed.contains(&class)
                            && layers.user_ceiling.contains(&class)
                            && layers.profile_allowed.contains(&class)
                            && layers
                                .conversation_allowed
                                .as_ref()
                                .unwrap()
                                .contains(&class)
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn allow_once_sends_exact_selected_bytes_and_audits_metadata() {
    let (_directory, guard) = guard();
    let mut controller = Controller::with([OutboundApprovalDecision::AllowOnce]);
    let mut transport = FakeTransport::default();
    let mut audit = Vec::new();
    let payloads = vec![
        item(OutboundDataClass::PromptText, b"first"),
        item(OutboundDataClass::SelectedMessages, b"second"),
    ];

    let receipt = guard
        .dispatch(
            request(recipient(b"identity-one"), payloads),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap();

    assert_eq!(receipt, 11);
    assert_eq!(transport.calls, 1);
    assert_eq!(transport.sent, [b"first".to_vec(), b"second".to_vec()]);
    assert_eq!(controller.reviews.len(), 1);
    assert_eq!(controller.reviews[0].items.len(), 2);
    assert_eq!(
        audit.iter().map(|event| event.stage).collect::<Vec<_>>(),
        [
            OutboundAuditStage::Requested,
            OutboundAuditStage::Allowed {
                source: OutboundDecisionSource::UserOnce,
            },
            OutboundAuditStage::Sending,
            OutboundAuditStage::Succeeded,
        ]
    );
}

#[tokio::test]
async fn denial_cancellation_and_missing_controller_send_zero_bytes() {
    for decision in [
        Some(OutboundApprovalDecision::DenyOnce),
        Some(OutboundApprovalDecision::Cancel),
        None,
    ] {
        let (_directory, guard) = guard();
        let mut controller = Controller::with(decision);
        let mut transport = FakeTransport::default();
        let mut audit = Vec::new();
        let result = guard
            .dispatch(
                request(
                    recipient(b"identity-one"),
                    vec![item(OutboundDataClass::PromptText, b"protected")],
                ),
                &policy(all_classes()),
                decision.map(|_| &mut controller),
                &mut transport,
                &mut audit,
            )
            .await;

        assert!(matches!(
            result,
            Err(OutboundError::Denied(_) | OutboundError::Cancelled)
        ));
        assert_eq!(transport.calls, 0);
        assert!(transport.sent.is_empty());
        assert!(
            !audit
                .iter()
                .any(|event| matches!(event.stage, OutboundAuditStage::Sending))
        );
    }
}

#[tokio::test]
async fn policy_ceiling_denial_precedes_approval_and_send() {
    let (_directory, guard) = guard();
    let mut controller = Controller::with([OutboundApprovalDecision::AllowOnce]);
    let mut transport = FakeTransport::default();
    let mut audit = Vec::new();
    let result = guard
        .dispatch(
            request(
                recipient(b"identity-one"),
                vec![item(OutboundDataClass::SelectedFileContents, b"protected")],
            ),
            &policy(classes(1)),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await;

    assert!(matches!(
        result,
        Err(OutboundError::Denied(OutboundDecisionSource::PolicyCeiling))
    ));
    assert!(controller.reviews.is_empty());
    assert_eq!(transport.calls, 0);
}

#[tokio::test]
async fn saved_decisions_are_exact_to_identity_and_class_and_are_revocable() {
    let (_directory, guard) = guard();
    let original = recipient(b"identity-one");
    let changed = recipient(b"identity-two");
    let mut controller = Controller::with([
        OutboundApprovalDecision::SaveAllow,
        OutboundApprovalDecision::AllowOnce,
        OutboundApprovalDecision::AllowOnce,
    ]);
    let mut transport = FakeTransport::default();
    let mut audit = Vec::new();

    guard
        .dispatch(
            request(
                original.clone(),
                vec![item(OutboundDataClass::PromptText, b"one")],
            ),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap();
    guard
        .dispatch(
            request(
                original.clone(),
                vec![item(OutboundDataClass::PromptText, b"two")],
            ),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap();
    assert_eq!(
        controller.reviews.len(),
        1,
        "saved exact class should be reused"
    );

    guard
        .dispatch(
            request(changed, vec![item(OutboundDataClass::PromptText, b"three")]),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap();
    assert_eq!(
        controller.reviews.len(),
        2,
        "identity changes require review"
    );

    assert!(
        guard
            .revoke(&original, OutboundDataClass::PromptText)
            .unwrap()
    );
    guard
        .dispatch(
            request(original, vec![item(OutboundDataClass::PromptText, b"four")]),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap();
    assert_eq!(controller.reviews.len(), 3, "revocation requires review");
}

#[tokio::test]
async fn saved_deny_fails_closed_without_reprompting() {
    let (_directory, guard) = guard();
    let recipient = recipient(b"identity-one");
    let mut controller = Controller::with([OutboundApprovalDecision::SaveDeny]);
    let mut transport = FakeTransport::default();
    let mut audit = Vec::new();

    for _ in 0..2 {
        let result = guard
            .dispatch(
                request(
                    recipient.clone(),
                    vec![item(OutboundDataClass::PromptText, b"protected")],
                ),
                &policy(all_classes()),
                Some(&mut controller),
                &mut transport,
                &mut audit,
            )
            .await;
        assert!(matches!(result, Err(OutboundError::Denied(_))));
    }
    assert_eq!(controller.reviews.len(), 1);
    assert_eq!(transport.calls, 0);
}

#[test]
fn review_and_audit_never_retain_or_render_selected_content() {
    let secret = "sk-secret-DO-NOT-LEAK";
    let request = request(
        recipient(b"identity-one"),
        vec![item(OutboundDataClass::PromptText, secret.as_bytes())],
    );
    let review = request.review();
    let audit = audit_event(&review, OutboundAuditStage::Requested);

    let surfaces = [
        format!("{request:?}"),
        review.render(),
        serde_json::to_string(&review).unwrap(),
        serde_json::to_string(&audit).unwrap(),
    ];
    for surface in surfaces {
        assert!(
            !surface.contains(secret),
            "leaked content through {surface}"
        );
    }
    assert!(review.render().contains("selected item"));
    assert_eq!(review.items[0].content_digest.len(), 64);
}

#[test]
fn unsafe_review_strings_and_oversized_payloads_fail_before_authorization() {
    assert!(matches!(
        RecipientIdentity::new(
            RecipientKind::McpHttp,
            "server\u{1b}[31m",
            "https://example.test",
            b"identity"
        ),
        Err(OutboundError::UnsafeReviewText { .. })
    ));
    assert!(matches!(
        OutboundItem::new(
            OutboundDataClass::PromptText,
            "item",
            None,
            "source",
            vec![0; MAX_ITEM_BYTES + 1]
        ),
        Err(OutboundError::ItemTooLarge { .. })
    ));
}

#[tokio::test]
async fn transport_failure_is_redacted_and_audited() {
    let (_directory, guard) = guard();
    let mut controller = Controller::with([OutboundApprovalDecision::AllowOnce]);
    let mut transport = FakeTransport {
        failure: Some(OutboundTransportFailure::Protocol),
        ..FakeTransport::default()
    };
    let mut audit = Vec::new();

    let error = guard
        .dispatch(
            request(
                recipient(b"identity-one"),
                vec![item(OutboundDataClass::PromptText, b"protected")],
            ),
            &policy(all_classes()),
            Some(&mut controller),
            &mut transport,
            &mut audit,
        )
        .await
        .unwrap_err();

    assert_eq!(error.to_string(), "outbound transport failed (Protocol)");
    assert!(matches!(
        audit.last().map(|event| event.stage),
        Some(OutboundAuditStage::Failed)
    ));
}
