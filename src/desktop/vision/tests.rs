use super::*;
use crate::{
    config::{PermissionMode, XanaConfig},
    desktop::tests::{bridge_channels, execution_host, scripted_client},
    execution_host::HostedConversationState,
    native_runtime::OperationOutcome,
};

#[test]
fn terminal_observations_only_release_the_matching_hosted_operation() {
    let operation_id = OperationId::new();
    for event in [
        AgentEvent::TurnStartUnavailable { operation_id },
        AgentEvent::OperationStateChanged {
            operation_id,
            state: OperationState::Finished(OperationOutcome::Interrupted),
        },
    ] {
        let observation = ClientObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: 1,
            event: ClientEvent::Runtime(Box::new(event)),
        };
        assert!(super::super::observation_outcome(&observation, OperationId::new()).is_none());
        assert!(super::super::observation_outcome(&observation, operation_id).is_some());
    }
}

#[tokio::test]
async fn failed_planning_does_not_settle_an_unrelated_foreground_run() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
    let (client, conversation) = scripted_client(&root);
    let (owner, mut observer) = client.into_parts();
    let host = execution_host(paths.data_dir(), &root, &conversation);
    let controller = DesktopController {
        conversation: conversation.clone(),
        client_id: ControllerClientId::new(),
    };
    let _grant = host
        .acquire_controller(&conversation, controller.client_id, None, Instant::now())
        .unwrap();
    let unrelated_operation = OperationId::new();
    let run = host
        .begin_foreground_run(
            &conversation,
            unrelated_operation,
            RunAccess::WorkspaceWrite,
            WriteCollisionDecision::Reject,
        )
        .await
        .unwrap();
    let mut active_run = Some(run.clone());
    let registry = XanaConfig::parse_registry(
        r#"
version = 4
default_profile = "default"
permission_mode = "deny"
[providers.chat]
kind = "ollama"
[profiles.default]
connection = "chat"
model = "synthetic"
"#,
    )
    .unwrap();
    let artifacts = crate::artifact::ArtifactStore::new(directory.path().join("artifacts"));
    let principal = PrincipalId::new();
    let mut state = State {
        config: Configuration {
            service: VisionTurnService::new(
                registry,
                crate::outbound::OutboundGuard::open(&paths).unwrap(),
                vec![],
                vec![],
                PermissionMode::Deny,
                artifacts.clone(),
                principal,
            ),
            config_file: paths.config_file().to_owned(),
            artifacts,
            owner: principal,
            conversation: SessionId::new(),
            native_images: false,
            native_destination: VisionDestination {
                route: None,
                connection: "synthetic".into(),
                model: "synthetic".into(),
                adapter: "native".into(),
                recipient: "http://127.0.0.1:1/v1".into(),
                recipient_digest: "0".repeat(64),
            },
            writer: DurableOperationSender::channel().0,
        },
        pending: None,
        job: None,
        reserved_operation: None,
        native_admission: None,
    };
    let (bridge, _commands, mut updates, _startup) = bridge_channels();
    state
        .finished(
            Finished {
                command_id: 41,
                operation: OperationId::new(),
                result: Err(DesktopVisionError::InvalidImage),
            },
            &bridge,
            &owner,
            &host,
            &controller,
            &mut active_run,
        )
        .await
        .unwrap();
    assert_eq!(active_run.as_ref(), Some(&run));
    let snapshot = host.snapshot().unwrap();
    let selected = snapshot
        .conversations
        .iter()
        .find(|item| item.conversation == conversation)
        .unwrap();
    assert_eq!(selected.active_operation, Some(unrelated_operation));
    assert_eq!(selected.state, HostedConversationState::Running);
    assert!(matches!(
        updates.try_recv().unwrap(),
        DesktopUpdate::Vision(DesktopVisionUpdate::Rejected {
            command_id: 41,
            reason: DesktopVisionError::InvalidImage,
        })
    ));
    assert!(matches!(
        updates.try_recv().unwrap(),
        DesktopUpdate::CommandResult {
            command_id: 41,
            accepted: false,
            ..
        }
    ));
    // A pre-admission event must resolve only its own image wait and must not
    // invent a durable NativeSubmitted/Failed result from an uncertain boundary.
    let image_operation = OperationId::new();
    state.native_admission = Some((
        42,
        VisionReceipt {
            version: 1,
            conversation_id: state.config.conversation.to_string(),
            operation_id: image_operation.to_string(),
            revision: 2,
            plan_digest: "0".repeat(64),
            prompt_digest: "1".repeat(64),
            destination: state.config.native_destination.clone(),
            sources: vec![VisionSource {
                artifact_id: uuid::Uuid::new_v4().to_string(),
                digest: "2".repeat(64),
                media_type: "image/png".into(),
                byte_len: 64,
            }],
            status: VisionStatus::NativeSubmitted,
            usage: VisionUsage::default(),
            derivative: None,
            untrusted_derivative: true,
        },
    ));
    state.reserved_operation = Some(image_operation);
    for event in [
        AgentEvent::TurnStartUnavailable {
            operation_id: unrelated_operation,
        },
        AgentEvent::CommandRejected {
            reason: "unrelated command".into(),
        },
    ] {
        state
            .observe(&ClientEvent::bounded(event), &bridge)
            .await
            .unwrap();
        assert!(state.native_admission.is_some());
        assert!(updates.try_recv().is_err());
    }
    state
        .observe(
            &ClientEvent::bounded(AgentEvent::TurnStartUnavailable {
                operation_id: image_operation,
            }),
            &bridge,
        )
        .await
        .unwrap();
    assert!(state.native_admission.is_none());
    assert!(state.reserved_operation.is_none());
    assert!(matches!(
        updates.try_recv().unwrap(),
        DesktopUpdate::Vision(DesktopVisionUpdate::Rejected {
            command_id: 42,
            reason: DesktopVisionError::Unavailable,
        })
    ));
    assert!(matches!(
        updates.try_recv().unwrap(),
        DesktopUpdate::CommandResult {
            command_id: 42,
            accepted: false,
            ..
        }
    ));
    assert!(
        updates.try_recv().is_err(),
        "no fabricated receipt may be published"
    );
    assert_eq!(active_run.as_ref(), Some(&run));
    host.finish_run(active_run.take().unwrap(), Ok(OperationOutcome::Completed))
        .unwrap();
    owner
        .send(ClientCommand::new(RuntimeCommand::Shutdown))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(observation) = observer.next().await {
            assert!(!matches!(
                observation.event,
                ClientEvent::Runtime(event)
                    if matches!(event.as_ref(), AgentEvent::UserMessageCommitted { .. })
            ));
        }
    })
    .await
    .unwrap();
}
