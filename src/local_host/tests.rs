use super::{
    descriptor::discover,
    hub::ObservationHub,
    protocol::{
        ClientFrame, ClientHello, ClientRole, HostEvent, HostSnapshot, HostSnapshotSeed,
        LOCAL_HOST_PROTOCOL_VERSION, ServerFrame, decode_server_frame, workspace_identity,
    },
    transport::{LocalHostServer, constant_time_equal, origin_is_loopback, validate_hello},
};
use crate::{
    frontend::{ClientCommand, ClientEvent},
    runtime::{AgentEvent, RuntimeCommand},
    workspace_host::WorkspaceHost,
};
use futures::{SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Barrier},
    thread,
};
use tempfile::tempdir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn seed(workspace: &std::path::Path, data_root: &std::path::Path) -> HostSnapshotSeed {
    let host = WorkspaceHost::open(data_root, workspace).unwrap();
    HostSnapshotSeed::from_workspace(&host.snapshot().unwrap())
}

#[test]
fn snapshot_subscription_and_publication_have_one_atomic_sequence_boundary() {
    let directory = tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let snapshot = HostSnapshot::new(uuid::Uuid::new_v4(), seed(&workspace, directory.path()));
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
}

#[test]
fn hello_rejects_wrong_version_workspace_role_and_capability_without_echoing_secret() {
    let capability = *blake3::hash(b"correct").as_bytes();
    let valid = ClientHello {
        version: LOCAL_HOST_PROTOCOL_VERSION,
        host_id: uuid::Uuid::new_v4(),
        workspace_id: "workspace".into(),
        capability: "correct".into(),
        role: ClientRole::Observer,
    };
    assert!(validate_hello(&valid, valid.host_id, "workspace", &capability).is_ok());

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
    ];
    for hello in cases {
        let error = validate_hello(&hello, valid.host_id, "workspace", &capability).unwrap_err();
        assert!(!error.contains(&hello.capability));
    }
}

fn valid_for_test(hello: &ClientHello) -> ClientHello {
    ClientHello {
        version: hello.version,
        host_id: hello.host_id,
        workspace_id: hello.workspace_id.clone(),
        capability: hello.capability.clone(),
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
        workspace_identity(&workspace.canonicalize().unwrap())
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
        workspace_id: descriptor.workspace_id.clone(),
        capability: descriptor.capability.clone(),
        role: ClientRole::Observer,
    });
    socket
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    let first = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(matches!(
        decode_server_frame(&first).unwrap(),
        ServerFrame::Snapshot(_)
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
