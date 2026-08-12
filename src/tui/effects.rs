//! Native and managed execution-owner interpretation of TUI update effects.

use super::{
    clipboard, session,
    state::{ArtifactAction, TuiState, UpdateEffect},
};
use crate::{
    frontend::EmbeddedClient,
    managed::codex::ApprovalDecision,
    managed_execution::ManagedTuiDriver,
    native_runtime::RuntimeCommand,
    plain_terminal::{ChatExit, ChatHeader},
    presentation::PresentationPreferences,
    vision::{ImageIngestor, ImageLimits},
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use std::io;
use tokio::sync::oneshot;

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_managed_effect(
    effect: UpdateEffect,
    state: &mut TuiState,
    driver: &ManagedTuiDriver,
    workspace_host: &WorkspaceHost,
    workspace: &std::path::Path,
    artifact_store: &crate::artifact::ArtifactStore,
    owner: crate::identity::PrincipalId,
    preferences_path: &std::path::Path,
    session_preferences: &mut session::SessionPreferenceStore,
    pending_approval: &mut Option<oneshot::Sender<ApprovalDecision>>,
    clipboard: &mut clipboard::Clipboard,
) -> Result<Option<ChatExit>> {
    match effect {
        UpdateEffect::None => {}
        UpdateEffect::Quit => return Ok(Some(ChatExit::Quit)),
        UpdateEffect::NewConversation => return Ok(Some(ChatExit::NewConversation)),
        UpdateEffect::Doctor => return Ok(Some(ChatExit::Doctor(None))),
        UpdateEffect::Reset => return Ok(Some(ChatExit::Reset)),
        UpdateEffect::Setup(section) => return Ok(Some(ChatExit::Setup(section))),
        UpdateEffect::ControlCommand { family, arguments } => {
            return Ok(Some(ChatExit::ControlCommand { family, arguments }));
        }
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } => {
            let model = driver.models.iter().find(|model| model.id == state.model);
            if !images.is_empty()
                && !model.is_some_and(|model| model.input_modalities.contains("image"))
            {
                state.restore_submission(
                    input,
                    images,
                    format!(
                        "{}/{} is not advertised as image-capable",
                        state.connection, state.model
                    ),
                );
            } else if let Err(reason) = driver
                .submit(operation_id, input.clone(), images.clone())
                .await
            {
                state.restore_submission(input, images, reason);
            } else {
                state.mark_submitted(operation_id, input);
            }
        }
        UpdateEffect::Interrupt { operation_id } => {
            if !driver.interrupt(operation_id) {
                state.set_status("No matching managed turn is active");
            }
        }
        UpdateEffect::Steer { input, .. } => {
            state.restore_submission(input, Vec::new(), "Codex app-server does not advertise same-turn steering; message retained as a draft".to_owned());
        }
        UpdateEffect::Attach(path) => {
            match ImageIngestor::new(artifact_store.clone(), ImageLimits::default())
                .ingest_path(workspace, &path, owner)
            {
                Ok(attachment) => state.stage_image(attachment),
                Err(error) => state.set_status(format!("could not attach {path}: {error}")),
            }
        }
        UpdateEffect::AttachClipboard => match clipboard.get_image(artifact_store.clone(), owner) {
            Ok(attachment) => state.stage_image(attachment),
            Err(error) => state.set_status(error),
        },
        UpdateEffect::SelectModel(selection) => {
            let requested = selection
                .split_once('/')
                .map_or(selection.as_str(), |(_, model)| model);
            if !driver.models.iter().any(|model| model.id == requested) {
                state.set_status(format!("Codex does not advertise model {requested:?}"));
            } else {
                driver
                    .select_model(requested.to_owned())
                    .await
                    .map_err(anyhow::Error::msg)?;
                state.set_model(requested.to_owned());
            }
        }
        UpdateEffect::SetReasoning(effort) => {
            driver
                .set_reasoning((effort != "auto").then_some(effort))
                .await
                .map_err(anyhow::Error::msg)?;
            state
                .set_status("Reasoning updated for subsequent turns; managed context is unchanged");
        }
        UpdateEffect::PersistComposer(preset) => {
            if let Err(error) = PresentationPreferences::set_composer(preferences_path, preset) {
                state.set_status(format!("could not save composer preference: {error}"));
            }
        }
        UpdateEffect::ClearConversation => driver.clear().await.map_err(anyhow::Error::msg)?,
        UpdateEffect::OpenModelPicker => state.open_model_picker(
            driver
                .models
                .iter()
                .map(|model| format!("{}/{}", state.connection, model.id))
                .collect(),
        ),
        UpdateEffect::OpenReasoningPicker => {
            let choices = driver
                .models
                .iter()
                .find(|model| model.id == state.model)
                .map(|model| {
                    model
                        .reasoning_efforts
                        .iter()
                        .map(|effort| effort.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            state.open_reasoning_picker(choices);
        }
        UpdateEffect::OpenSessionPicker => state.open_session_picker(),
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            let Some(before) = state.history_before() else {
                return Ok(None);
            };
            match workspace_host.conversation_history_page(&conversation, Some(before), 128) {
                Ok(Some(page)) => state.prepend_history_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load older history: {error}")),
            }
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::ArchiveConversation(conversation) => {
            let archived = match &conversation {
                ConversationRef::Managed {
                    connection,
                    thread_id,
                } if connection == &state.connection => driver
                    .archive(thread_id.clone())
                    .await
                    .map_err(anyhow::Error::msg)?,
                _ => workspace_host.archive_managed_conversation(&conversation)?,
            };
            if archived {
                state.archived_conversation(&conversation);
                state.refresh_sessions(workspace_host.snapshot()?);
            } else {
                state.set_status("Managed conversation was already absent from the local catalog");
            }
        }
        UpdateEffect::PersistActivity(activity) => {
            if let Err(error) = PresentationPreferences::set_activity(preferences_path, activity) {
                state.set_status(format!("could not save activity preference: {error}"));
            }
        }
        UpdateEffect::CopyText(text) => copy_text(state, clipboard, text),
        UpdateEffect::ArtifactAction { record, action } => {
            apply_artifact_action(state, artifact_store, record, action)?;
        }
        UpdateEffect::DecideManagedApproval(decision) => {
            let Some(reply) = pending_approval.take() else {
                state.set_status("Managed approval is no longer pending");
                return Ok(None);
            };
            if reply.send(decision).is_err() {
                state.set_status("Managed approval is no longer pending");
            }
        }
        UpdateEffect::DecideNativeApproval { .. } | UpdateEffect::DecideChildApproval { .. } => {
            state.set_status("Native approval cannot be sent to the managed runtime");
        }
    }
    Ok(None)
}

pub(super) fn apply_artifact_action(
    state: &mut TuiState,
    store: &crate::artifact::ArtifactStore,
    record: crate::artifact::ArtifactRecord,
    action: ArtifactAction,
) -> Result<()> {
    const PREVIEW_BYTES: usize = 64 * 1024;
    match action {
        ArtifactAction::Preview => {
            let preview = if record.media_type.starts_with("text/")
                || matches!(
                    record.media_type.as_str(),
                    "application/json" | "application/toml"
                ) {
                let bytes = store
                    .read_bounded(&record, PREVIEW_BYTES)
                    .context("could not read artifact preview")?;
                String::from_utf8(bytes)
                    .unwrap_or_else(|_| "[artifact text is not valid UTF-8]".to_owned())
            } else {
                format!(
                    "[binary preview omitted: {} · {} bytes]",
                    record.media_type, record.byte_len
                )
            };
            state.show_artifact_preview(record, preview);
        }
        ArtifactAction::InsertReference => state.insert_artifact_reference(&record),
        ArtifactAction::Reveal | ArtifactAction::Open => {
            let path = store
                .verified_path(&record, crate::artifact::MAX_ARTIFACT_BYTES)
                .context("could not verify artifact before opening it")?;
            open_artifact_path(&path, action == ArtifactAction::Reveal)
                .context("could not start the OS artifact action")?;
            state.set_status(if action == ArtifactAction::Reveal {
                "Artifact revealed in the OS file manager"
            } else {
                "Artifact opened with the OS default application"
            });
        }
    }
    Ok(())
}

fn copy_text(state: &mut TuiState, clipboard: &mut clipboard::Clipboard, text: String) {
    let characters = text.chars().count();
    match clipboard.set_text(text) {
        Ok(()) => state.set_status(format!(
            "Copied {characters} characters from the conversation"
        )),
        Err(error) => state.set_status(error),
    }
}

fn open_artifact_path(path: &std::path::Path, reveal: bool) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer.exe");
        if reveal {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        if reveal {
            command.arg("-R");
        }
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        command
    };
    command.spawn().map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_effect(
    effect: UpdateEffect,
    state: &mut TuiState,
    client: &EmbeddedClient,
    header: &ChatHeader,
    workspace_host: &WorkspaceHost,
    conversation: &ConversationRef,
    active_root: &mut Option<ActiveRootLease>,
    preferences_path: &std::path::Path,
    session_preferences: &mut session::SessionPreferenceStore,
    clipboard: &mut clipboard::Clipboard,
) -> Result<Option<ChatExit>> {
    match effect {
        UpdateEffect::None => {}
        UpdateEffect::Quit => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Quit));
        }
        UpdateEffect::NewConversation => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::NewConversation));
        }
        UpdateEffect::Doctor => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Doctor(Some(header.session_id))));
        }
        UpdateEffect::Reset => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Reset));
        }
        UpdateEffect::Setup(section) => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Setup(section)));
        }
        UpdateEffect::ControlCommand { family, arguments } => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::ControlCommand { family, arguments }));
        }
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
        } => {
            if !images.is_empty() {
                let descriptor = header
                    .models
                    .descriptor(&header.provider_name, &header.model)
                    .context("could not resolve selected model capabilities")?;
                if !descriptor.input_modalities.contains("image") {
                    state.restore_submission(
                        input,
                        images,
                        format!(
                            "{}/{} is not declared image-capable; refresh its catalog or add an explicit model override",
                            header.provider_name, header.model
                        ),
                    );
                    return Ok(None);
                }
            }
            let lease = match workspace_host.acquire_root(conversation.clone()) {
                Ok(lease) => lease,
                Err(error) => {
                    state.restore_submission(
                        input,
                        images,
                        format!("could not start turn: {error}"),
                    );
                    return Ok(None);
                }
            };
            let command = if images.is_empty() {
                RuntimeCommand::SubmitTurn {
                    operation_id,
                    input: input.clone(),
                }
            } else {
                RuntimeCommand::SubmitTurnWithImages {
                    operation_id,
                    input: input.clone(),
                    images: images.iter().map(|image| image.image.clone()).collect(),
                }
            };
            let result = client
                .send(command)
                .await
                .context("native TUI runtime stopped")?;
            if result.accepted {
                state.mark_submitted(operation_id, input);
                *active_root = Some(lease);
            } else {
                drop(lease);
                state.restore_submission(
                    input,
                    images,
                    result
                        .reason
                        .unwrap_or_else(|| "command rejected".to_owned()),
                );
            }
        }
        UpdateEffect::Interrupt { operation_id } => {
            let result = client
                .send(RuntimeCommand::InterruptOperation { operation_id })
                .await
                .context("native TUI runtime stopped during interrupt")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "interrupt was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::Steer {
            operation_id,
            input,
        } => {
            let result = client
                .send(RuntimeCommand::SteerOperation {
                    operation_id,
                    input,
                })
                .await
                .context("native TUI runtime stopped during steering")?;
            state.set_status(if result.accepted {
                "Steering update accepted".to_owned()
            } else {
                result
                    .reason
                    .unwrap_or_else(|| "steering update was rejected".to_owned())
            });
        }
        UpdateEffect::Attach(path) => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            if !descriptor.input_modalities.contains("image") {
                state.set_status(format!(
                    "{}/{} is not declared image-capable",
                    header.provider_name, header.model
                ));
            } else {
                match ImageIngestor::new(header.artifact_store.clone(), ImageLimits::default())
                    .ingest_path(&header.workspace_root, &path, header.owner)
                {
                    Ok(attachment) => state.stage_image(attachment),
                    Err(error) => state.set_status(format!("could not attach {path}: {error}")),
                }
            }
        }
        UpdateEffect::AttachClipboard => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            if !descriptor.input_modalities.contains("image") {
                state.set_status(format!(
                    "{}/{} is not declared image-capable",
                    header.provider_name, header.model
                ));
            } else {
                match clipboard.get_image(header.artifact_store.clone(), header.owner) {
                    Ok(attachment) => state.stage_image(attachment),
                    Err(error) => state.set_status(error),
                }
            }
        }
        UpdateEffect::SelectModel(selection) => {
            let Some((connection, model)) = selection.split_once('/') else {
                state.set_status("Model selection must be CONNECTION/MODEL");
                return Ok(None);
            };
            match header.models.select(connection, model) {
                Ok(_) => {
                    client
                        .send(RuntimeCommand::Shutdown)
                        .await
                        .context("could not stop the old model runtime")?;
                    return Ok(Some(ChatExit::Restart));
                }
                Err(error) => state.set_status(format!("could not select model: {error}")),
            }
        }
        UpdateEffect::SetReasoning(effort) => {
            match header.models.update_reasoning_effort(Some(effort)) {
                Ok(_) => {
                    client
                        .send(RuntimeCommand::Shutdown)
                        .await
                        .context("could not stop the old reasoning runtime")?;
                    return Ok(Some(ChatExit::Restart));
                }
                Err(error) => state.set_status(format!("could not select reasoning: {error}")),
            }
        }
        UpdateEffect::PersistComposer(preset) => {
            if let Err(error) = PresentationPreferences::set_composer(preferences_path, preset) {
                state.set_status(format!("could not save composer preference: {error}"));
            }
        }
        UpdateEffect::ClearConversation => {
            let result = client
                .send(RuntimeCommand::ClearConversation)
                .await
                .context("native TUI runtime stopped while clearing")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "clear was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::OpenModelPicker => {
            let choices = header
                .models
                .summaries()
                .into_iter()
                .flat_map(|summary| {
                    summary
                        .models
                        .into_iter()
                        .map(move |model| format!("{}/{}", summary.id, model.id))
                })
                .collect();
            state.open_model_picker(choices);
        }
        UpdateEffect::OpenReasoningPicker => {
            let choices = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .map(|descriptor| {
                    descriptor
                        .reasoning_efforts
                        .into_iter()
                        .map(|effort| effort.id)
                        .collect()
                })
                .unwrap_or_default();
            state.open_reasoning_picker(choices);
        }
        UpdateEffect::OpenSessionPicker => state.open_session_picker(),
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            let Some(before) = state.history_before() else {
                return Ok(None);
            };
            match workspace_host.conversation_history_page(&conversation, Some(before), 128) {
                Ok(Some(page)) => state.prepend_history_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load older history: {error}")),
            }
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::ArchiveConversation(conversation) => {
            if workspace_host.archive_managed_conversation(&conversation)? {
                state.archived_conversation(&conversation);
                state.refresh_sessions(workspace_host.snapshot()?);
            } else {
                state.set_status("Managed conversation was already absent from the local catalog");
            }
        }
        UpdateEffect::PersistActivity(activity) => {
            if let Err(error) = PresentationPreferences::set_activity(preferences_path, activity) {
                state.set_status(format!("could not save activity preference: {error}"));
            }
        }
        UpdateEffect::CopyText(text) => copy_text(state, clipboard, text),
        UpdateEffect::ArtifactAction { record, action } => {
            apply_artifact_action(state, &header.artifact_store, record, action)?;
        }
        UpdateEffect::DecideNativeApproval {
            operation_id,
            invocation_id,
            decision,
        } => {
            let result = client
                .send(RuntimeCommand::DecidePermission {
                    operation_id,
                    invocation_id,
                    decision,
                })
                .await
                .context("native TUI runtime stopped during approval")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "approval was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::DecideChildApproval {
            agent_id,
            operation_id,
            invocation_id,
            decision,
        } => {
            let result = client
                .send(RuntimeCommand::DecideChildPermission {
                    agent_id,
                    operation_id,
                    invocation_id,
                    decision,
                })
                .await
                .context("native child runtime stopped during approval")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "child approval was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::DecideManagedApproval(_) => {
            state.set_status("Managed approval cannot be sent through the native runtime");
        }
    }
    Ok(None)
}
