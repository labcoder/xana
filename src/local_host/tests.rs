use super::{
    descriptor::discover,
    hub::ObservationHub,
    protocol::{
        ClientFrame, ClientHello, ClientRole, HostEvent, HostSnapshot, HostSnapshotSeed,
        LOCAL_HOST_PROTOCOL_VERSION, ServerFrame, decode_client_frame, decode_server_frame,
    },
    transport::{
        ControlledExecution, LocalHostServer, constant_time_equal, origin_is_loopback,
        validate_hello,
    },
};
use crate::{
    frontend::{ClientCommand, ClientEvent},
    native_runtime::{AgentEvent, RuntimeCommand},
    workspace_host::WorkspaceHost,
};
use futures::{SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Barrier, Mutex},
    thread,
};
use tempfile::tempdir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn seed(workspace: &std::path::Path, data_root: &std::path::Path) -> HostSnapshotSeed {
    let host = WorkspaceHost::open(data_root, workspace).unwrap();
    HostSnapshotSeed::from_workspace(&host.snapshot().unwrap())
}

#[test]
fn managed_approval_transport_preserves_authority_bearing_command_and_cwd_tails() {
    use super::protocol::{HostObservation, ManagedApprovalSnapshot, encode_frame};
    use crate::{identity::OperationId, managed::codex::ApprovalRequest};
    let command = format!("echo {} && important-tail", "argument ".repeat(90));
    let cwd = format!("C:/workspace/{}important-cwd-tail", "nested/".repeat(90));
    let request = ApprovalRequest {
        item_id: Some("command-1".into()),
        method: "item/commandExecution/requestApproval".into(),
        available_decisions: ["accept".into(), "decline".into()].into_iter().collect(),
        reason: None,
        command: Some(command.clone()),
        cwd: Some(cwd.clone()),
    };
    let frame = ServerFrame::Observation(HostObservation {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        sequence: 1,
        event: HostEvent::ManagedApprovalRequested(ManagedApprovalSnapshot::bounded(
            uuid::Uuid::new_v4(),
            OperationId::new(),
            request,
        )),
    });
    let received = decode_server_frame(&encode_frame(&frame).unwrap()).unwrap();
    let ServerFrame::Observation(HostObservation {
        event: HostEvent::ManagedApprovalRequested(approval),
        ..
    }) = received
    else {
        panic!("expected managed approval");
    };
    assert_eq!(approval.command.as_deref(), Some(command.as_str()));
    assert_eq!(approval.cwd.as_deref(), Some(cwd.as_str()));
}

#[test]
fn snapshot_subscription_and_publication_have_one_atomic_sequence_boundary() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let snapshot = HostSnapshot::new(uuid::Uuid::new_v4(), 1, seed(&workspace, directory.path()));
    let hub = ObservationHub::new(snapshot);
    let barrier = Arc::new(Barrier::new(2));
    let publisher = hub.clone();
    let publish_barrier = Arc::clone(&barrier);
    let task = thread::spawn(move || {
        publish_barrier.wait();
        for index in 0..100 {
            publisher
                .publish(HostEvent::Frontend(ClientEvent::bounded(
                    AgentEvent::CommandRejected {
                        reason: format!("event {index}"),
                    },
                )))
                .unwrap();
        }
    });
    barrier.wait();
    let mut subscription = hub.subscribe().unwrap();
    task.join().unwrap();
    let mut expected = subscription.snapshot.sequence + 1;
    while let Ok(observation) = subscription.observations.try_recv() {
        assert_eq!(observation.sequence, expected);
        expected += 1;
    }
    assert_eq!(expected, 101);
}

#[test]
fn capability_comparison_and_origin_policy_are_explicit() {
    let capability = *blake3::hash(b"correct").as_bytes();
    assert!(constant_time_equal(&capability, &capability));
    assert!(!constant_time_equal(
        &capability,
        blake3::hash(b"wrong").as_bytes()
    ));
    assert!(origin_is_loopback("http://localhost:3000"));
    assert!(origin_is_loopback("https://127.0.0.1"));
    assert!(origin_is_loopback("http://[::1]:8080"));
    assert!(!origin_is_loopback("https://example.com"));
    assert!(!origin_is_loopback("null"));
    assert!(decode_client_frame(
        r#"{"type":"get_artifact","value":{"request_id":"00000000-0000-0000-0000-000000000001","artifact_id":"../../etc/passwd","max_preview_bytes":64}}"#
    )
    .is_err());
}

#[test]
fn hello_rejects_wrong_version_workspace_role_and_capability_without_echoing_secret() {
    let capability = *blake3::hash(b"correct").as_bytes();
    let valid = ClientHello {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        host_id: uuid::Uuid::new_v4(),
        host_generation: 7,
        workspace_id: "workspace".into(),
        capability: "correct".into(),
        controller_reconnect: None,
        role: ClientRole::Observer,
    };
    assert!(
        validate_hello(
            &valid,
            valid.host_id,
            valid.host_generation,
            "workspace",
            &capability
        )
        .is_ok()
    );

    let cases = [
        {
            let mut hello = valid_for_test(&valid);
            hello.version = 99;
            hello
        },
        {
            let mut hello = valid_for_test(&valid);
            hello.workspace_id = "other".into();
            hello
        },
        {
            let mut hello = valid_for_test(&valid);
            hello.capability = "wrong".into();
            hello
        },
        {
            let mut hello = valid_for_test(&valid);
            hello.host_id = uuid::Uuid::new_v4();
            hello
        },
        {
            let mut hello = valid_for_test(&valid);
            hello.host_generation += 1;
            hello
        },
    ];
    for hello in cases {
        let error = validate_hello(
            &hello,
            valid.host_id,
            valid.host_generation,
            "workspace",
            &capability,
        )
        .unwrap_err();
        assert!(!error.contains(&hello.capability));
    }
}

fn valid_for_test(hello: &ClientHello) -> ClientHello {
    ClientHello {
        version: hello.version,
        host_id: hello.host_id,
        host_generation: hello.host_generation,
        workspace_id: hello.workspace_id.clone(),
        capability: hello.capability.clone(),
        controller_reconnect: None,
        role: hello.role,
    }
}

#[tokio::test]
async fn real_loopback_observer_discovers_authenticates_and_receives_snapshot() {
    let directory = tempdir().unwrap();
    let runtime = directory.path().join("run");
    let data = directory.path().join("data");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let seed = seed(&workspace, &data);
    let server = LocalHostServer::bind(
        &runtime,
        &workspace,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        seed,
    )
    .await
    .unwrap();
    assert!(server.endpoint().ip().is_loopback());
    let descriptor_path = server.descriptor_path().to_owned();
    assert!(descriptor_path.exists());
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.run());

    let observer = super::connect_observer(&runtime, &workspace).await.unwrap();
    assert_eq!(observer.snapshot().version, LOCAL_HOST_PROTOCOL_VERSION);
    assert_eq!(
        observer.snapshot().workspace_id,
        crate::workspace_identity::WorkspaceIdentity::resolve(&workspace)
            .unwrap()
            .collision_key()
    );
    assert!(!observer.snapshot().workspace_name.is_empty());

    shutdown.cancel();
    task.await.unwrap().unwrap();
    assert!(!descriptor_path.exists());
}

#[tokio::test]
async fn observer_command_is_rejected_and_audited_without_runtime_mutation() {
    let directory = tempdir().unwrap();
    let runtime = directory.path().join("run");
    let data = directory.path().join("data");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let server = LocalHostServer::bind(
        &runtime,
        &workspace,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        seed(&workspace, &data),
    )
    .await
    .unwrap();
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.run());
    let descriptor = discover(&runtime, &workspace).unwrap();
    let (mut socket, _) = connect_async(format!("ws://{}", descriptor.endpoint))
        .await
        .unwrap();
    let hello = ClientFrame::Hello(ClientHello {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        host_id: descriptor.host_id,
        host_generation: descriptor.generation,
        workspace_id: descriptor.workspace_id.clone(),
        capability: descriptor.capability.clone(),
        controller_reconnect: None,
        role: ClientRole::Observer,
    });
    socket
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    let first = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(matches!(
        decode_server_frame(&first).unwrap(),
        ServerFrame::Snapshot { .. }
    ));
    let command = ClientCommand::new(RuntimeCommand::ClearConversation);
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientFrame::Command(command))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    let result = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(matches!(
        decode_server_frame(&result).unwrap(),
        ServerFrame::CommandResult(result) if !result.accepted
    ));
    let audit = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(matches!(
        decode_server_frame(&audit).unwrap(),
        ServerFrame::Observation(observation)
            if matches!(observation.event, HostEvent::ObserverCommandRejected { .. })
    ));

    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn controller_is_explicit_exclusive_and_release_fails_closed() {
    let directory = tempdir().unwrap();
    let runtime = directory.path().join("run");
    let data = directory.path().join("data");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let events = Arc::new(Mutex::new(None));
    let factory_events = Arc::clone(&events);
    let server = LocalHostServer::bind_controlled(
        &runtime,
        &workspace,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        seed(&workspace, &data),
        ControlledExecution::new("native/test".into(), None, None, move |hub| {
            let (execution, receiver) = super::execution::fake_execution(hub);
            *factory_events.lock().unwrap() = Some(receiver);
            execution
        }),
    )
    .await
    .unwrap();
    let shutdown = server.shutdown_token();
    let task = tokio::spawn(server.run());
    tokio::task::yield_now().await;

    let mut first = super::connect_observer(&runtime, &workspace).await.unwrap();
    let mut second = super::connect_observer(&runtime, &workspace).await.unwrap();
    first
        .acquire_control("native/test".into(), false)
        .await
        .unwrap();
    assert!(first.is_controller());
    assert!(
        second
            .acquire_control("native/test".into(), false)
            .await
            .unwrap_err()
            .to_string()
            .contains("requires confirmation")
    );
    let accepted = first
        .send_command(crate::native_runtime::RuntimeCommand::ClearConversation)
        .await
        .unwrap();
    assert!(accepted.accepted);
    let mut events = events.lock().unwrap().take().unwrap();
    assert!(matches!(
        events.recv().await,
        Some(super::execution::FakeExecutionEvent::Command(command))
            if matches!(command.value, crate::frontend::ClientCommandValue::ClearConversation)
    ));

    second
        .acquire_control("native/test".into(), true)
        .await
        .unwrap();
    assert!(
        !first
            .send_command(crate::native_runtime::RuntimeCommand::ClearConversation)
            .await
            .unwrap()
            .accepted
    );
    second.release_control().await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(super::execution::FakeExecutionEvent::FailClosed)
    ));

    shutdown.cancel();
    task.await.unwrap().unwrap();
    let first = events.recv().await;
    let second = events.recv().await;
    assert!(
        matches!(first, Some(super::execution::FakeExecutionEvent::Shutdown))
            || matches!(second, Some(super::execution::FakeExecutionEvent::Shutdown))
    );
}

#[test]
fn reconnect_capability_restores_only_the_same_controller_during_grace() {
    use crate::controller::{ControllerClientId, ControllerDisconnectReason};
    use std::time::Duration;

    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let host_id = uuid::Uuid::new_v4();
    let snapshot = HostSnapshot::new(host_id, 1, seed(&workspace, directory.path()))
        .with_controllable_conversation("native/test".into());
    let hub = ObservationHub::new(snapshot);
    let original = ControllerClientId::new();
    let mut grant = hub
        .acquire_controller(original, "native/test", None)
        .unwrap();
    let reconnect = grant.take_reconnect_capability();
    let expiry = hub
        .disconnect_controller(
            original,
            ControllerDisconnectReason::TransportClosed,
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(
        hub.reconnect_controller(ControllerClientId::new(), "wrong")
            .is_err()
    );
    let replacement = ControllerClientId::new();
    hub.reconnect_controller(replacement, &reconnect).unwrap();
    assert!(hub.is_controller(replacement));
    assert!(!hub.expire_controller(&expiry));

    let next_expiry = hub
        .disconnect_controller(
            replacement,
            ControllerDisconnectReason::TransportClosed,
            Duration::ZERO,
        )
        .unwrap();
    assert!(hub.expire_controller(&next_expiry));
    assert!(!hub.is_controller(replacement));
    assert!(
        hub.reconnect_controller(ControllerClientId::new(), &reconnect)
            .is_err()
    );
}

#[test]
fn simultaneous_exact_takeovers_have_one_visible_winner() {
    use crate::controller::{ControllerClientId, ControllerTakeoverConfirmation};
    use std::sync::Barrier;

    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let snapshot = HostSnapshot::new(uuid::Uuid::new_v4(), 1, seed(&workspace, directory.path()))
        .with_controllable_conversation("native/test".into());
    let hub = ObservationHub::new(snapshot);
    let incumbent = ControllerClientId::new();
    let grant = hub
        .acquire_controller(incumbent, "native/test", None)
        .unwrap();
    let confirmation = ControllerTakeoverConfirmation::from(&grant.snapshot);
    let barrier = Arc::new(Barrier::new(3));
    let attempts = [ControllerClientId::new(), ControllerClientId::new()]
        .into_iter()
        .map(|client| {
            let hub = hub.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                (
                    client,
                    hub.acquire_controller(client, "native/test", Some(confirmation)),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|(_, result)| result.is_err())
            .count(),
        1
    );
    let winner = outcomes
        .iter()
        .find_map(|(client, result)| result.is_ok().then_some(*client))
        .unwrap();
    assert!(hub.is_controller(winner));
}

#[test]
fn a_full_observer_queue_is_evicted_without_blocking_publication() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let hub = ObservationHub::new(HostSnapshot::new(
        uuid::Uuid::new_v4(),
        1,
        seed(&workspace, directory.path()),
    ));
    let _slow = hub.subscribe().unwrap();
    for index in 0..=super::hub::OBSERVER_QUEUE_CAPACITY {
        hub.publish(HostEvent::ObserverCommandRejected {
            command: format!("event-{index}"),
        })
        .unwrap();
    }
    assert_eq!(hub.subscriber_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn host_shutdown_aborts_an_unresponsive_owned_execution_by_the_hard_bound() {
    let directory = tempdir().unwrap();
    let runtime = directory.path().join("run");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let server = LocalHostServer::bind_controlled(
        &runtime,
        &workspace,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        seed(&workspace, directory.path()),
        ControlledExecution::new("native/test".into(), None, None, |_| {
            super::execution::stubborn_execution()
        }),
    )
    .await
    .unwrap();
    let descriptor = server.descriptor_path().to_owned();
    server.shutdown_token().cancel();
    server.run().await.unwrap();
    assert!(!descriptor.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn descriptor_is_owner_only_on_unix() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempdir().unwrap();
    let runtime = directory.path().join("run");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let server = LocalHostServer::bind(
        &runtime,
        &workspace,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        seed(&workspace, directory.path()),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::metadata(server.descriptor_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn non_loopback_bind_is_rejected_before_descriptor_creation() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let seed = seed(&workspace, directory.path());
    let result = LocalHostServer::bind(
        directory.path(),
        &workspace,
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        0,
        seed,
    )
    .await;
    let Err(error) = result else {
        panic!("non-loopback bind unexpectedly succeeded");
    };
    assert!(error.to_string().contains("loopback"));
}
