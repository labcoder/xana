use super::*;

fn now() -> Instant {
    Instant::now()
}

#[test]
fn takeover_requires_confirmation_and_cannot_cross_a_pending_approval() {
    let start = now();
    let conversation = "conversation-a".to_owned();
    let first = ControllerClientId::new();
    let second = ControllerClientId::new();
    let mut leases = ControllerLeases::new();
    let first_grant = leases
        .acquire(conversation.clone(), first, None, false, start)
        .unwrap();

    assert!(matches!(
        leases.acquire(conversation.clone(), second, None, false, start),
        Err(ControllerLeaseError::TakeoverConfirmationRequired {
            current,
        }) if current.conversation == conversation
            && current.controller_id == first_grant.snapshot.controller_id
            && current.generation == first_grant.snapshot.generation
    ));
    let confirmation = ControllerTakeoverConfirmation::from(&first_grant.snapshot);
    assert!(matches!(
        leases.acquire(conversation.clone(), second, Some(confirmation), true, start),
        Err(ControllerLeaseError::PendingApproval(value)) if value == conversation
    ));

    let takeover = leases
        .acquire(
            conversation.clone(),
            second,
            Some(confirmation),
            false,
            start,
        )
        .unwrap();
    assert!(matches!(
        takeover.change,
        ControllerChangeKind::TakenOver { previous_controller }
            if previous_controller == first_grant.snapshot.controller_id
    ));
    assert_eq!(
        takeover.snapshot.takeover,
        ControllerTakeoverState::Confirmed
    );
    assert!(!leases.is_controller(&conversation, first));
    assert!(leases.is_controller(&conversation, second));
}

#[test]
fn stale_takeover_confirmation_cannot_displace_the_race_winner() {
    let start = now();
    let conversation = "conversation-a".to_owned();
    let incumbent = ControllerClientId::new();
    let first_challenger = ControllerClientId::new();
    let second_challenger = ControllerClientId::new();
    let mut leases = ControllerLeases::new();
    let incumbent = leases
        .acquire(conversation.clone(), incumbent, None, false, start)
        .unwrap();
    let confirmation = ControllerTakeoverConfirmation::from(&incumbent.snapshot);

    let winner = leases
        .acquire(
            conversation.clone(),
            first_challenger,
            Some(confirmation),
            false,
            start,
        )
        .unwrap();
    assert!(matches!(
        leases.acquire(
            conversation.clone(),
            second_challenger,
            Some(confirmation),
            false,
            start,
        ),
        Err(ControllerLeaseError::TakeoverConfirmationRequired { current })
            if current.controller_id == winner.snapshot.controller_id
                && current.generation == winner.snapshot.generation
    ));
    assert!(leases.is_controller(&conversation, first_challenger));
    assert!(!leases.is_controller(&conversation, second_challenger));
}

#[test]
fn reconnect_uses_monotonic_expiry_and_rotates_lease_generation_and_capability() {
    let start = now();
    let grace = Duration::from_secs(3);
    let conversation = "conversation-a".to_owned();
    let first = ControllerClientId::new();
    let replacement = ControllerClientId::new();
    let mut leases = ControllerLeases::new();
    let mut initial = leases
        .acquire(conversation.clone(), first, None, false, start)
        .unwrap();
    let initial_generation = initial.snapshot.generation;
    let reconnect = initial.take_reconnect_capability();
    let (_, expiry) = leases
        .disconnect_client(
            first,
            ControllerDisconnectReason::TransportClosed,
            start,
            grace,
        )
        .pop()
        .unwrap();

    let mut reconnected = leases
        .reconnect(replacement, &reconnect, start + Duration::from_secs(2))
        .unwrap();
    assert!(reconnected.snapshot.generation > initial_generation);
    assert_eq!(reconnected.snapshot.state, ControllerLeaseState::Connected);
    assert!(leases.is_controller(&conversation, replacement));
    assert!(
        leases
            .reconnect(
                ControllerClientId::new(),
                &reconnect,
                start + Duration::from_secs(2)
            )
            .is_err()
    );
    assert!(
        leases
            .expire(&expiry, start + Duration::from_secs(3))
            .is_none()
    );

    let rotated = reconnected.take_reconnect_capability();
    let (_, next_expiry) = leases
        .disconnect_client(
            replacement,
            ControllerDisconnectReason::SequenceGap,
            start + Duration::from_secs(4),
            grace,
        )
        .pop()
        .unwrap();
    assert!(
        leases
            .reconnect(
                ControllerClientId::new(),
                &rotated,
                start + Duration::from_secs(8)
            )
            .is_err()
    );
    assert!(
        leases
            .expire(&next_expiry, start + Duration::from_secs(8))
            .is_some(),
        "the authoritative expiry path removes the expired reconnect lease"
    );
}

#[test]
fn independent_conversations_have_independent_controllers() {
    let start = now();
    let mut leases = ControllerLeases::new();
    let client_a = ControllerClientId::new();
    let client_b = ControllerClientId::new();
    leases
        .acquire("a".to_owned(), client_a, None, false, start)
        .unwrap();
    leases
        .acquire("b".to_owned(), client_b, None, false, start)
        .unwrap();

    assert!(leases.is_controller(&"a".to_owned(), client_a));
    assert!(!leases.is_controller(&"a".to_owned(), client_b));
    assert!(leases.is_controller(&"b".to_owned(), client_b));
    assert!(leases.snapshot(&"a".to_owned(), start).is_some());
    assert!(leases.snapshot(&"b".to_owned(), start).is_some());
}

#[test]
fn renew_rotates_the_capability_and_stale_release_fails() {
    let start = now();
    let conversation = "conversation-a".to_owned();
    let controller = ControllerClientId::new();
    let stale = ControllerClientId::new();
    let mut leases = ControllerLeases::new();
    let mut first = leases
        .acquire(conversation.clone(), controller, None, false, start)
        .unwrap();
    let old_capability = first.take_reconnect_capability();
    let renewed = leases.renew(&conversation, controller, start).unwrap();
    assert!(renewed.snapshot.generation > first.snapshot.generation);
    assert!(matches!(renewed.change, ControllerChangeKind::Renewed));
    assert!(matches!(
        leases.release(&conversation, stale),
        Err(ControllerLeaseError::NotController(value)) if value == conversation
    ));

    let _ = leases.disconnect_client(
        controller,
        ControllerDisconnectReason::TransportClosed,
        start,
        Duration::from_secs(3),
    );
    assert!(matches!(
        leases.reconnect(ControllerClientId::new(), &old_capability, start),
        Err(ControllerLeaseError::InvalidReconnect)
    ));
}

#[test]
fn reconnect_capability_can_replace_a_stalled_transport_before_disconnect_arrives() {
    let start = now();
    let conversation = "conversation-a".to_owned();
    let stalled_client = ControllerClientId::new();
    let replacement = ControllerClientId::new();
    let mut leases = ControllerLeases::new();
    let mut grant = leases
        .acquire(conversation.clone(), stalled_client, None, false, start)
        .unwrap();
    let reconnect = grant.take_reconnect_capability();

    let replacement_grant = leases.reconnect(replacement, &reconnect, start).unwrap();
    assert!(matches!(
        replacement_grant.change,
        ControllerChangeKind::Reconnected
    ));
    assert!(!leases.is_controller(&conversation, stalled_client));
    assert!(leases.is_controller(&conversation, replacement));
    assert!(
        leases
            .disconnect_client(
                stalled_client,
                ControllerDisconnectReason::SequenceGap,
                start,
                Duration::from_secs(3),
            )
            .is_empty()
    );
}
