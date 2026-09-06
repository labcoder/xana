//! Desktop adapter for a compatible foreground Xana host.
//!
//! The local host keeps runtime, policy, and Conversation authority. This
//! adapter owns only the bounded Desktop projection and local presentation
//! preferences; it never takes control from an existing controller.

use super::*;
use crate::local_host::{
    AttachedObserver, LocalHostError, LocalHostEvent, LocalHostObservation, LocalHostSnapshot,
    ManagedApprovalDecision, connect_observer, reconnect_controller,
};
use std::{collections::HashMap, path::Path, time::Duration};

const CONTROLLER_RENEWAL: Duration = Duration::from_secs(30);

pub(super) async fn connect_if_active(
    paths: &XanaPaths,
    workspace: &Path,
) -> Result<Option<AttachedObserver>, DesktopError> {
    match crate::local_host::inspect_descriptor_health(paths.runtime_dir(), workspace)
        .map_err(local_host_error)?
    {
        crate::local_host::DescriptorHealth::Absent => Ok(None),
        crate::local_host::DescriptorHealth::Stale { .. } => {
            crate::local_host::remove_stale_descriptor(paths.runtime_dir(), workspace)
                .map_err(local_host_error)?;
            Ok(None)
        }
        crate::local_host::DescriptorHealth::Active { .. } => {
            connect_observer(paths.runtime_dir(), workspace)
                .await
                .map(Some)
                .map_err(local_host_error)
        }
        crate::local_host::DescriptorHealth::InvalidActive { path, reason } => {
            Err(DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!(
                    "refusing to replace active local-host descriptor {}: {reason}",
                    path.display()
                ),
            ))
        }
    }
}

pub(super) async fn serve(
    bridge: Bridge,
    paths: &XanaPaths,
    workspace: &Path,
    mut observer: AttachedObserver,
) -> Result<ChatExit, DesktopError> {
    if observer.snapshot().controller.is_none()
        && let Some(conversation) = observer.snapshot().controllable_conversation.clone()
    {
        observer
            .acquire_control(conversation, false)
            .await
            .map_err(local_host_error)?;
    }

    let mut state = AttachedState::open(
        paths,
        workspace,
        observer.snapshot(),
        observer.is_controller(),
    )?;
    let initial = state.project();
    if !bridge.startup.ready(initial.clone()) {
        bridge
            .publish_critical(DesktopUpdate::Snapshot(Box::new(initial)))
            .await?;
    }

    let mut commands = bridge.commands.lock().await;
    let mut renewal = tokio::time::interval(CONTROLLER_RENEWAL);
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    renewal.tick().await;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    release_control(&mut observer).await;
                    return Ok(ChatExit::Quit);
                };
                if command.version != FRONTEND_PROTOCOL_VERSION {
                    bridge.publish_command_result(
                        command.command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::ProtocolMismatch,
                            format!(
                                "Desktop command protocol {} does not match runtime protocol {}",
                                command.version, FRONTEND_PROTOCOL_VERSION
                            ),
                        )),
                    ).await?;
                    continue;
                }
                if handle_command(&bridge, &mut observer, &mut state, command).await? {
                    release_control(&mut observer).await;
                    bridge.publish_critical(DesktopUpdate::BackendStopped {
                        expected: true,
                        error: None,
                    }).await?;
                    return Ok(ChatExit::Quit);
                }
            }
            observation = observer.next() => {
                match observation {
                    Ok(observation) => {
                        apply_observation(&bridge, &mut state, observation).await?;
                    }
                    Err(LocalHostError::SequenceGap { .. }) => {
                        let reconnect = observer.take_controller_reconnect();
                        observer = match reconnect {
                            Some(capability) => reconnect_controller(
                                paths.runtime_dir(), workspace, capability
                            ).await,
                            None => connect_observer(paths.runtime_dir(), workspace).await,
                        }.map_err(local_host_error)?;
                        state.replace_host(observer.snapshot(), observer.is_controller())?;
                        bridge.publish_critical(DesktopUpdate::Snapshot(Box::new(state.project()))).await?;
                    }
                    Err(LocalHostError::Closed) => {
                        return Err(DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "the attached foreground Xana host stopped",
                        ));
                    }
                    Err(error) => return Err(local_host_error(error)),
                }
            }
            _ = renewal.tick(), if observer.is_controller() => {
                observer.renew_control().await.map_err(local_host_error)?;
            }
        }
    }
}

struct AttachedState {
    host: LocalHostSnapshot,
    frontend: ClientSnapshot,
    authority: DesktopAuthority,
    controller_id: Option<String>,
    managed_approvals: HashMap<uuid::Uuid, DesktopPendingApproval>,
    notification_policy: NotificationPolicy,
    navigation: DesktopNavigationSnapshot,
    navigation_store: navigation::DesktopNavigationStore,
    layout: DesktopResolvedLayout,
    layout_store: layout::DesktopLayoutStore,
    settings: settings::DesktopSettingsState,
    attachments: DesktopAttachmentService,
}

impl AttachedState {
    fn open(
        paths: &XanaPaths,
        workspace: &Path,
        host: &LocalHostSnapshot,
        controller: bool,
    ) -> Result<Self, DesktopError> {
        let frontend = host.frontend.clone().ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::UnsupportedExecutionOwner,
                "the foreground host did not provide a compatible frontend snapshot",
            )
        })?;
        if frontend.version != FRONTEND_PROTOCOL_VERSION {
            return Err(DesktopError::new(
                DesktopErrorCode::ProtocolMismatch,
                format!(
                    "foreground host protocol {} does not match Desktop protocol {}",
                    frontend.version, FRONTEND_PROTOCOL_VERSION
                ),
            ));
        }
        let selected = host
            .controllable_conversation
            .as_deref()
            .or(host.active_conversation.as_deref());
        let navigation_store = navigation::DesktopNavigationStore::open(paths, workspace)?;
        let navigation = navigation_store.snapshot(selected)?;
        let layout_store = layout::DesktopLayoutStore::open(paths);
        let layout = layout_store.resolve(selected.unwrap_or("attached"));
        let settings =
            settings::DesktopSettingsState::open(crate::settings::SettingsManager::new(paths))?;
        let notification_policy =
            crate::config::XanaConfig::load_registry_from(paths.config_file())
                .map_err(|error| {
                    DesktopError::new(
                        DesktopErrorCode::ConfigurationUnavailable,
                        format!("could not load notification policy: {error}"),
                    )
                })?
                .notifications;
        let store = crate::artifact::ArtifactStore::open(paths.data_dir()).map_err(|error| {
            DesktopError::new(DesktopErrorCode::StateInvalid, error.to_string())
        })?;
        let attachments = DesktopAttachmentService {
            workspace: workspace.to_path_buf(),
            store: store.clone(),
            ingestor: ImageIngestor::new(store, ImageLimits::default()),
            resource_policy: frontend.semantic.attachment_policy.configured.clone(),
            owner: crate::identity::PrincipalId::new(),
        };
        let controller_id = controller
            .then(|| {
                host.controller
                    .as_ref()
                    .map(|lease| lease.controller_id.to_string())
            })
            .flatten();
        Ok(Self {
            host: host.clone(),
            frontend,
            authority: if controller {
                DesktopAuthority::Controller
            } else {
                DesktopAuthority::Observer
            },
            controller_id,
            managed_approvals: HashMap::new(),
            notification_policy,
            navigation,
            navigation_store,
            layout,
            layout_store,
            settings,
            attachments,
        })
    }

    fn replace_host(
        &mut self,
        host: &LocalHostSnapshot,
        controller: bool,
    ) -> Result<(), DesktopError> {
        let frontend = host.frontend.clone().ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::UnsupportedExecutionOwner,
                "the foreground host did not provide a compatible frontend snapshot",
            )
        })?;
        self.host = host.clone();
        self.frontend = frontend;
        self.authority = if controller {
            DesktopAuthority::Controller
        } else {
            DesktopAuthority::Observer
        };
        self.controller_id = controller
            .then(|| {
                host.controller
                    .as_ref()
                    .map(|lease| lease.controller_id.to_string())
            })
            .flatten();
        self.managed_approvals.clear();
        Ok(())
    }

    fn project(&self) -> DesktopSnapshot {
        let mut snapshot = project_frontend_snapshot(
            &self.frontend,
            self.authority,
            true,
            &self.notification_policy,
            &self.navigation,
            &self.layout,
            self.settings.snapshot(),
            None,
        );
        snapshot.host_sequence = self.host.sequence;
        snapshot.hosted_workspace_count = 1;
        snapshot.hosted_conversation_count = self.host.conversations.len();
        snapshot.hosted_conversations = self
            .host
            .conversations
            .iter()
            .map(|conversation| {
                let selected = self
                    .host
                    .controllable_conversation
                    .as_deref()
                    .is_some_and(|id| id == conversation.identity);
                DesktopHostedConversation {
                    conversation: conversation.identity.clone(),
                    workspace_id: self.host.workspace_id.clone(),
                    connection: if selected {
                        self.frontend.connection.clone()
                    } else {
                        "unavailable".to_owned()
                    },
                    model: if selected {
                        self.frontend.model.clone()
                    } else {
                        "unavailable".to_owned()
                    },
                    profile: None,
                    permission_mode: if selected {
                        self.frontend.approval_policy.clone()
                    } else {
                        "unavailable".to_owned()
                    },
                    state: if selected && self.frontend.active_operation.is_some() {
                        DesktopConversationState::Running
                    } else {
                        DesktopConversationState::Idle
                    },
                    active_operation: selected
                        .then_some(self.frontend.active_operation)
                        .flatten()
                        .map(DesktopOperationId),
                    pending_approvals: if selected {
                        self.frontend.pending_approval_count + self.managed_approvals.len()
                    } else {
                        0
                    },
                    activity_count: if selected {
                        self.frontend.activity_count
                    } else {
                        0
                    },
                    last_outcome: None,
                    controller: self.host.controller.as_ref().map(project_controller),
                }
            })
            .collect();
        snapshot.attached_conversation = self
            .host
            .controllable_conversation
            .clone()
            .or_else(|| self.host.active_conversation.clone());
        snapshot.controllers = self
            .host
            .controller
            .as_ref()
            .map(project_controller)
            .into_iter()
            .collect();
        snapshot.host_lifecycle = "attached".to_owned();
        snapshot.pending_approval_count = snapshot
            .pending_approval_count
            .saturating_add(self.managed_approvals.len());
        snapshot
            .pending_approvals
            .extend(self.managed_approvals.values().cloned());
        snapshot
            .pending_approvals
            .sort_by_key(|approval| approval.id.to_string());
        snapshot
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.frontend.sequence.saturating_add(1);
        self.frontend.sequence = sequence;
        sequence
    }

    fn require_controller(&self) -> Result<(), DesktopError> {
        (self.authority == DesktopAuthority::Controller)
            .then_some(())
            .ok_or_else(|| {
                DesktopError::new(
                    DesktopErrorCode::AuthorityRequired,
                    "this Desktop is observing a foreground host controlled by another client",
                )
            })
    }
}

async fn apply_observation(
    bridge: &Bridge,
    state: &mut AttachedState,
    observation: LocalHostObservation,
) -> Result<(), DesktopError> {
    state.host.sequence = observation.sequence;
    match observation.event {
        LocalHostEvent::ScheduledJobsChanged { jobs } => {
            state.host.scheduled_jobs = jobs;
        }
        LocalHostEvent::Frontend(event) => {
            let sequence = state.next_sequence();
            state.frontend.apply(&event, sequence);
            let projected = DesktopObservation {
                version: FRONTEND_PROTOCOL_VERSION,
                sequence,
                conversation_start: state.frontend.conversation_start,
                conversation_total: state.frontend.conversation_total,
                event: project_event(
                    &event,
                    &state.frontend.session_id,
                    &state.frontend.semantic.attachment_policy.configured,
                ),
            };
            let replaceable = projected.event.replaceable();
            bridge
                .publish(DesktopUpdate::Observation(projected), replaceable)
                .await?;
            bridge
                .publish(
                    DesktopUpdate::HostObservation(DesktopHostObservation {
                        sequence: observation.sequence,
                        event: DesktopHostEvent::RuntimeObservation {
                            conversation: selected_conversation(&state.host),
                        },
                    }),
                    true,
                )
                .await?;
        }
        LocalHostEvent::ManagedApprovalRequested(request) => {
            let permission_id =
                DesktopPermissionId::attached_managed(request.operation_id, request.approval_id);
            let approval = DesktopPendingApproval {
                id: permission_id,
                tool: request.method,
                effect: "execute".to_owned(),
                scope: request
                    .command
                    .or(request.cwd)
                    .or(request.reason)
                    .unwrap_or_else(|| "managed runtime request".to_owned()),
            };
            state
                .managed_approvals
                .insert(request.approval_id, approval.clone());
            publish_event(
                bridge,
                state,
                DesktopEvent::PermissionRequired {
                    permission_id,
                    tool: approval.tool,
                    effect: approval.effect,
                    scope: approval.scope,
                },
            )
            .await?;
        }
        LocalHostEvent::ManagedApprovalResolved {
            approval_id,
            accepted: _,
        } => {
            if let Some(approval) = state.managed_approvals.remove(&approval_id) {
                publish_event(
                    bridge,
                    state,
                    DesktopEvent::PermissionResolved {
                        permission_id: approval.id,
                    },
                )
                .await?;
            }
        }
        LocalHostEvent::ManagedTurnFinished {
            operation_id,
            error,
        } => {
            if state.frontend.active_operation == Some(operation_id) {
                state.frontend.active_operation = None;
            }
            if let Some(error) = error {
                publish_event(
                    bridge,
                    state,
                    DesktopEvent::Error(DesktopError::new(
                        DesktopErrorCode::RuntimeUnavailable,
                        error,
                    )),
                )
                .await?;
            }
        }
        LocalHostEvent::ControllerChanged {
            controller,
            change,
            reason: _,
        } => {
            state.host.controller.clone_from(&controller);
            let current = controller
                .as_ref()
                .map(|lease| lease.controller_id.to_string());
            state.authority = if current.is_some() && current == state.controller_id {
                DesktopAuthority::Controller
            } else {
                DesktopAuthority::Observer
            };
            let conversation = controller
                .as_ref()
                .map(|lease| lease.conversation.clone())
                .unwrap_or_else(|| selected_conversation(&state.host));
            bridge
                .publish_critical(DesktopUpdate::HostObservation(DesktopHostObservation {
                    sequence: observation.sequence,
                    event: DesktopHostEvent::ControllerChanged {
                        conversation,
                        controller: controller.as_ref().map(project_controller),
                        change: format!("{change:?}").to_ascii_lowercase(),
                    },
                }))
                .await?;
            bridge
                .publish_critical(DesktopUpdate::Snapshot(Box::new(state.project())))
                .await?;
        }
        LocalHostEvent::ObserverCommandRejected { command } => {
            publish_event(
                bridge,
                state,
                DesktopEvent::Error(DesktopError::new(
                    DesktopErrorCode::AuthorityRequired,
                    format!("foreground host rejected observer command {command}"),
                )),
            )
            .await?;
        }
    }
    Ok(())
}

async fn publish_event(
    bridge: &Bridge,
    state: &mut AttachedState,
    event: DesktopEvent,
) -> Result<(), DesktopError> {
    let sequence = state.next_sequence();
    bridge
        .publish(
            DesktopUpdate::Observation(DesktopObservation {
                version: FRONTEND_PROTOCOL_VERSION,
                sequence,
                conversation_start: state.frontend.conversation_start,
                conversation_total: state.frontend.conversation_total,
                event,
            }),
            false,
        )
        .await
}

async fn handle_command(
    bridge: &Bridge,
    observer: &mut AttachedObserver,
    state: &mut AttachedState,
    command: BridgeCommand,
) -> Result<bool, DesktopError> {
    let command_id = command.command_id;
    match command.value {
        BridgeCommandValue::Vision(_) => {
            bridge
                .publish_command_result(
                    command_id,
                    Err(vision::invalid(DesktopVisionError::Unsupported)),
                )
                .await?;
        }
        BridgeCommandValue::RequestSnapshot => {
            bridge
                .publish_critical(DesktopUpdate::Snapshot(Box::new(state.project())))
                .await?;
            bridge.publish_command_result(command_id, Ok(())).await?;
        }
        BridgeCommandValue::Submit {
            operation_id,
            input,
            attachments,
            acknowledge_workspace_write_collision: _,
            correlation,
        } => {
            if correlation.is_some() {
                bridge
                    .publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::UnsupportedExecutionOwner,
                            "correlated submission requires an embedded native owner",
                        )),
                    )
                    .await?;
                return Ok(false);
            }
            let result = state.require_controller().and_then(|()| {
                validate_desktop_attachments(attachments).map(|images| {
                    if images.is_empty() {
                        RuntimeCommand::SubmitTurn {
                            operation_id: operation_id.0,
                            input,
                        }
                    } else {
                        RuntimeCommand::SubmitTurnWithImages {
                            operation_id: operation_id.0,
                            input,
                            images,
                        }
                    }
                })
            });
            let result = match result {
                Ok(command) => observer
                    .send_command(command)
                    .await
                    .map_err(local_host_error)
                    .and_then(command_result),
                Err(error) => Err(error),
            };
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::Clear => {
            let result =
                send_controller_command(observer, state, RuntimeCommand::ClearConversation).await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::BrowserControl(action) => {
            let result =
                send_controller_command(observer, state, RuntimeCommand::BrowserControl { action })
                    .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::Interrupt { operation_id } => {
            let result = send_controller_command(
                observer,
                state,
                RuntimeCommand::InterruptOperation {
                    operation_id: operation_id.0,
                },
            )
            .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::DecidePermission {
            permission_id,
            allow_once,
        } => {
            let result = state.require_controller();
            let result = match (result, permission_id.0) {
                (Err(error), _) => Err(error),
                (
                    Ok(()),
                    DesktopPermissionTarget::Native {
                        operation_id,
                        invocation_id,
                    },
                ) => observer
                    .send_command(RuntimeCommand::DecidePermission {
                        operation_id,
                        invocation_id,
                        decision: if allow_once {
                            ControllerDecision::AllowOnce
                        } else {
                            ControllerDecision::Deny
                        },
                    })
                    .await
                    .map_err(local_host_error)
                    .and_then(command_result),
                (Ok(()), DesktopPermissionTarget::AttachedManaged { approval_id, .. }) => observer
                    .decide_managed_approval(
                        approval_id,
                        if allow_once {
                            ManagedApprovalDecision::AcceptOnce
                        } else {
                            ManagedApprovalDecision::Decline
                        },
                    )
                    .await
                    .map_err(local_host_error),
                (Ok(()), DesktopPermissionTarget::Managed { .. }) => Err(DesktopError::new(
                    DesktopErrorCode::CommandRejected,
                    "an embedded managed approval was sent to an attached host",
                )),
            };
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::DecideRoundBudget {
            operation_id,
            suspension_id,
            action,
        } => {
            let result = send_controller_command(
                observer,
                state,
                RuntimeCommand::DecideRoundBudget {
                    operation_id: operation_id.0,
                    suspension_id: suspension_id.0,
                    action,
                },
            )
            .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::StageResource {
            path,
            external_approved,
        } => {
            let service = state.attachments.clone();
            let result = tokio::task::spawn_blocking(move || {
                service.stage_path(&path.to_string_lossy(), external_approved)
            })
            .await
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeCrashed,
                    format!("Desktop attachment worker stopped: {error}"),
                )
            })
            .and_then(std::convert::identity);
            if let Ok(attachment) = &result {
                bridge
                    .publish_critical(DesktopUpdate::AttachmentStaged {
                        command_id,
                        attachment: attachment.clone(),
                    })
                    .await?;
            }
            bridge
                .publish_command_result(command_id, result.map(|_| ()))
                .await?;
        }
        BridgeCommandValue::StageClipboardImage => {
            let service = state.attachments.clone();
            let result = tokio::task::spawn_blocking(move || service.stage_clipboard())
                .await
                .map_err(|error| {
                    DesktopError::new(
                        DesktopErrorCode::RuntimeCrashed,
                        format!("Desktop clipboard worker stopped: {error}"),
                    )
                })
                .and_then(std::convert::identity);
            if let Ok(attachment) = &result {
                bridge
                    .publish_critical(DesktopUpdate::AttachmentStaged {
                        command_id,
                        attachment: attachment.clone(),
                    })
                    .await?;
            }
            bridge
                .publish_command_result(command_id, result.map(|_| ()))
                .await?;
        }
        BridgeCommandValue::SaveArtifact {
            artifact_id,
            destination,
        } => {
            let result = perform_artifact_command(
                state.attachments.clone(),
                &state.frontend,
                &artifact_id,
                DesktopArtifactCommand::Save(destination),
            )
            .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::RevealArtifact { artifact_id } => {
            let result = perform_artifact_command(
                state.attachments.clone(),
                &state.frontend,
                &artifact_id,
                DesktopArtifactCommand::Reveal,
            )
            .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::OpenArtifact { artifact_id } => {
            let result = perform_artifact_command(
                state.attachments.clone(),
                &state.frontend,
                &artifact_id,
                DesktopArtifactCommand::Open,
            )
            .await;
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::SetSidebarMode { mode } => {
            let result = state.navigation_store.set_sidebar_mode(mode);
            if result.is_ok() {
                state.navigation.sidebar_mode = mode;
                bridge
                    .publish_critical(DesktopUpdate::Navigation(state.navigation.clone()))
                    .await?;
            }
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::SaveLayout { layout } => {
            let conversation = selected_conversation(&state.host);
            let result = layout
                .validate()
                .and_then(|()| state.layout_store.save_conversation(&conversation, &layout));
            if result.is_ok() {
                state.layout = DesktopResolvedLayout {
                    layout,
                    source: DesktopLayoutSource::Conversation,
                    warning: None,
                };
                bridge
                    .publish_critical(DesktopUpdate::Layout(Box::new(state.layout.clone())))
                    .await?;
            }
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::ResetLayout => {
            let conversation = selected_conversation(&state.host);
            let result = state.layout_store.clear_conversation(&conversation);
            if result.is_ok() {
                state.layout = state.layout_store.resolve(&conversation);
                bridge
                    .publish_critical(DesktopUpdate::Layout(Box::new(state.layout.clone())))
                    .await?;
            }
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::SaveLayoutAsDefault => {
            let result = state.layout_store.save_default(&state.layout.layout);
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::ClearDefaultLayout => {
            let conversation = selected_conversation(&state.host);
            let result = state.layout_store.clear_default();
            if result.is_ok() {
                state.layout = state.layout_store.resolve(&conversation);
                bridge
                    .publish_critical(DesktopUpdate::Layout(Box::new(state.layout.clone())))
                    .await?;
            }
            bridge.publish_command_result(command_id, result).await?;
        }
        BridgeCommandValue::Shutdown => {
            bridge.publish_command_result(command_id, Ok(())).await?;
            return Ok(true);
        }
        BridgeCommandValue::SelectManagedModel { .. }
        | BridgeCommandValue::SetManagedReasoning { .. }
        | BridgeCommandValue::SwitchConversation { .. }
        | BridgeCommandValue::NewConversation { .. }
        | BridgeCommandValue::ArchiveManagedConversation { .. }
        | BridgeCommandValue::RenameProject { .. }
        | BridgeCommandValue::SetProjectArchived { .. }
        | BridgeCommandValue::UngroupConversation { .. }
        | BridgeCommandValue::MoveConversation { .. }
        | BridgeCommandValue::BranchConversation { .. }
        | BridgeCommandValue::BeginSettings
        | BridgeCommandValue::SetSetting { .. }
        | BridgeCommandValue::ResetSetting { .. }
        | BridgeCommandValue::RevertSetting { .. }
        | BridgeCommandValue::ValidateSettings { .. }
        | BridgeCommandValue::CommitSettings { .. }
        | BridgeCommandValue::DiscardSettings { .. }
        | BridgeCommandValue::ReloadSettings => {
            bridge
                .publish_command_result(
                    command_id,
                    Err(DesktopError::new(
                        DesktopErrorCode::AuthorityRequired,
                        "the foreground host owns this operation; use its owning surface",
                    )),
                )
                .await?;
        }
    }
    Ok(false)
}

async fn send_controller_command(
    observer: &mut AttachedObserver,
    state: &AttachedState,
    command: RuntimeCommand,
) -> Result<(), DesktopError> {
    state.require_controller()?;
    observer
        .send_command(command)
        .await
        .map_err(local_host_error)
        .and_then(command_result)
}

async fn release_control(observer: &mut AttachedObserver) {
    if observer.is_controller() {
        let _ = observer.release_control().await;
    }
}

fn selected_conversation(host: &LocalHostSnapshot) -> String {
    host.controllable_conversation
        .clone()
        .or_else(|| host.active_conversation.clone())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn local_host_error(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(
        DesktopErrorCode::RuntimeUnavailable,
        format!("could not attach to the foreground Xana host: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig};
    use crate::frontend::{ClientSnapshotSeed, semantic::HostLocationV1};
    use crate::local_host::{HostSnapshotSeed, LocalConversationOwner, LocalHostConversation};
    use crate::shell::ShellConfig;
    use std::net::{IpAddr, Ipv4Addr};

    type TestBridgeChannels = (
        Bridge,
        mpsc::Sender<BridgeCommand>,
        mpsc::Receiver<DesktopUpdate>,
        std_mpsc::Receiver<Result<DesktopSnapshot, DesktopError>>,
    );

    fn write_config(paths: &XanaPaths) {
        let config = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "fixture".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "fixture-model".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
    }

    fn frontend(session_id: crate::identity::SessionId) -> ClientSnapshot {
        ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id,
                connection: "fixture".to_owned(),
                execution_owner: "native".to_owned(),
                model: "fixture-model".to_owned(),
                reasoning_effort: None,
                host_location: HostLocationV1::Loopback,
                approval_policy: "ask".to_owned(),
                children: Vec::new(),
                resource_policy: crate::resource::ResourcePolicyV1::default(),
            },
            Vec::new(),
        )
    }

    fn seed(workspace: &Path, session_id: crate::identity::SessionId) -> HostSnapshotSeed {
        HostSnapshotSeed {
            workspace_id: crate::workspace_identity::WorkspaceIdentity::resolve(workspace)
                .unwrap()
                .collision_key()
                .to_owned(),
            workspace_name: "workspace".to_owned(),
            conversations: vec![LocalHostConversation {
                identity: format!("native/{session_id}"),
                owner: LocalConversationOwner::Native,
                state: "controlled".to_owned(),
                record_count: Some(0),
                selected: true,
            }],
            conversations_truncated: false,
            active_conversation: Some(format!("native/{session_id}")),
        }
    }

    fn bridge_channels() -> TestBridgeChannels {
        let (command_sender, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (updates, update_receiver) = mpsc::channel(UPDATE_CAPACITY);
        let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
        (
            Bridge {
                commands: Arc::new(tokio::sync::Mutex::new(command_receiver)),
                updates,
                update_signal: DesktopWakeSignal::default(),
                startup: StartupSignal::new(startup_sender),
                notification_policy: NotificationPolicy::default(),
                deferred: Arc::new(Mutex::new(DeferredDelivery::default())),
                service_certificate: None,
            },
            command_sender,
            update_receiver,
            startup_receiver,
        )
    }

    #[test]
    fn attached_projection_is_honest_about_authority_and_location() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        write_config(&paths);
        let session_id = crate::identity::SessionId::new();
        let frontend = frontend(session_id);
        let host = LocalHostSnapshot::new(uuid::Uuid::new_v4(), 1, seed(&workspace, session_id))
            .with_controllable_conversation(format!("native/{session_id}"))
            .with_frontend(frontend);
        let state = AttachedState::open(&paths, &workspace, &host, false).unwrap();

        let snapshot = state.project();
        assert_eq!(snapshot.authority, DesktopAuthority::Observer);
        assert!(snapshot.attached_to_foreground_host);
        assert_eq!(snapshot.host_lifecycle, "attached");
        assert_eq!(snapshot.hosted_workspace_count, 1);
        assert_eq!(snapshot.hosted_conversation_count, 1);
    }

    #[tokio::test]
    async fn desktop_attaches_as_observer_without_taking_an_existing_controller() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        write_config(&paths);
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session_id = crate::identity::SessionId::new();
        let conversation = format!("native/{session_id}");
        let events = Arc::new(Mutex::new(None));
        let factory_events = Arc::clone(&events);
        let server = crate::local_host::LocalHostServer::bind_controlled(
            paths.runtime_dir(),
            &workspace,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
            seed(&workspace, session_id),
            crate::local_host::ControlledExecution::new(
                conversation.clone(),
                Some(frontend(session_id)),
                None,
                move |hub| {
                    let (execution, receiver) = crate::local_host::fake_execution(hub);
                    *factory_events.lock().unwrap() = Some(receiver);
                    execution
                },
            ),
        )
        .await
        .unwrap();
        let shutdown = server.shutdown_token();
        let server_task = tokio::spawn(server.run());
        let mut incumbent = connect_observer(paths.runtime_dir(), &workspace)
            .await
            .unwrap();
        incumbent
            .acquire_control(conversation, false)
            .await
            .unwrap();

        let desktop_observer = connect_if_active(&paths, &workspace)
            .await
            .unwrap()
            .unwrap();
        let (bridge, commands, _updates, startup) = bridge_channels();
        let serve_task = tokio::spawn({
            let paths = paths.clone();
            let workspace = workspace.clone();
            async move { serve(bridge, &paths, &workspace, desktop_observer).await }
        });
        let initial = tokio::task::spawn_blocking(move || {
            startup
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(initial.authority, DesktopAuthority::Observer);
        assert!(initial.attached_to_foreground_host);

        commands
            .send(BridgeCommand {
                version: FRONTEND_PROTOCOL_VERSION,
                command_id: 1,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        assert_eq!(serve_task.await.unwrap().unwrap(), ChatExit::Quit);

        let accepted = incumbent
            .send_command(RuntimeCommand::ClearConversation)
            .await
            .unwrap();
        assert!(
            accepted.accepted,
            "Desktop must not take over the incumbent"
        );
        let mut receiver = events.lock().unwrap().take().unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(crate::local_host::FakeExecutionEvent::Command(command))
                if matches!(command.value, crate::frontend::ClientCommandValue::ClearConversation)
        ));

        incumbent.release_control().await.unwrap();
        shutdown.cancel();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn desktop_acquires_an_unclaimed_controller_and_routes_commands() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        write_config(&paths);
        let workspace = directory.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session_id = crate::identity::SessionId::new();
        let conversation = format!("native/{session_id}");
        let events = Arc::new(Mutex::new(None));
        let factory_events = Arc::clone(&events);
        let server = crate::local_host::LocalHostServer::bind_controlled(
            paths.runtime_dir(),
            &workspace,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
            seed(&workspace, session_id),
            crate::local_host::ControlledExecution::new(
                conversation,
                Some(frontend(session_id)),
                None,
                move |hub| {
                    let (execution, receiver) = crate::local_host::fake_execution(hub);
                    *factory_events.lock().unwrap() = Some(receiver);
                    execution
                },
            ),
        )
        .await
        .unwrap();
        let shutdown = server.shutdown_token();
        let server_task = tokio::spawn(server.run());
        let desktop_observer = connect_if_active(&paths, &workspace)
            .await
            .unwrap()
            .unwrap();
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let serve_task = tokio::spawn({
            let paths = paths.clone();
            let workspace = workspace.clone();
            async move { serve(bridge, &paths, &workspace, desktop_observer).await }
        });
        let initial = tokio::task::spawn_blocking(move || {
            startup
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(initial.authority, DesktopAuthority::Controller);

        commands
            .send(BridgeCommand {
                version: FRONTEND_PROTOCOL_VERSION,
                command_id: 1,
                value: BridgeCommandValue::Clear,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    updates.recv().await,
                    Some(DesktopUpdate::CommandResult {
                        command_id: 1,
                        accepted: true,
                        ..
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let mut receiver = events.lock().unwrap().take().unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(crate::local_host::FakeExecutionEvent::Command(command))
                if matches!(command.value, crate::frontend::ClientCommandValue::ClearConversation)
        ));

        commands
            .send(BridgeCommand {
                version: FRONTEND_PROTOCOL_VERSION,
                command_id: 2,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        assert_eq!(serve_task.await.unwrap().unwrap(), ChatExit::Quit);
        shutdown.cancel();
        server_task.await.unwrap().unwrap();
    }
}
