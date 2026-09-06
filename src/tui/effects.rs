//! Native and managed execution-owner interpretation of TUI update effects.

use super::{
    clipboard, session,
    state::{ArtifactAction, TuiState, UpdateEffect},
};
use crate::{
    app::{ChatExit, ChatHeader},
    command_catalog::{ColorCapability, PresentationCapabilities},
    frontend::{
        EmbeddedClient,
        semantic::{
            AttachmentProvenanceV1, AttachmentV1, ResourceCapabilityContextV1,
            project_resource_capabilities,
        },
    },
    managed::codex::ApprovalDecision,
    managed_execution::ManagedTuiDriver,
    native_runtime::RuntimeCommand,
    presentation::PresentationPreferences,
    resource::{
        LocalResourcePath, ResourcePolicyV1, classify_local_path, inspection::ResourceIngestor,
    },
    vision::{
        DroppedImagePath, ImageAttachment, ImageIngestor, ImageLimits, MAX_IMAGE_BYTES_PER_TURN,
        MAX_IMAGES_PER_TURN, classify_dropped_image_path,
    },
    workspace_host::{ActiveRootLease, ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use tokio::sync::oneshot;
use uuid::Uuid;

struct ClassifiedImagePaths {
    paths: Vec<String>,
    external_paths: Vec<String>,
}

fn load_older_history(state: &mut TuiState, host: &WorkspaceHost, conversation: &ConversationRef) {
    if state.needs_history_snapshot() {
        match host.conversation_history_page(conversation, None, 128) {
            Ok(Some(page)) => state.begin_saved_history_page(page),
            Ok(None) => {
                state.set_status("Managed history remains owned by its runtime");
                return;
            }
            Err(error) => {
                state.set_status(format!("could not inspect saved history: {error}"));
                return;
            }
        }
    }
    let Some(before) = state.history_before() else {
        return;
    };
    match host.conversation_history_page(conversation, Some(before), 128) {
        Ok(Some(page)) => state.prepend_history_page(page),
        Ok(None) => state.set_status("Managed history remains owned by its runtime"),
        Err(error) => state.set_status(format!("could not load older history: {error}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_typed_path(
    state: &mut TuiState,
    workspace: &std::path::Path,
    artifact_store: &crate::artifact::ArtifactStore,
    owner: crate::identity::PrincipalId,
    policy: ResourcePolicyV1,
    presentation: PresentationCapabilities,
    path: String,
    approved_external: bool,
    image_capable: bool,
    provenance: AttachmentProvenanceV1,
) {
    if !approved_external {
        match classify_local_path(workspace, &path) {
            Ok(LocalResourcePath::External { .. }) => {
                state.request_external_resource_approval(path);
                return;
            }
            Ok(LocalResourcePath::Workspace { .. }) => {}
            Err(error) => {
                state.set_status(format!("could not resolve resource {path}: {error}"));
                return;
            }
        }
    }

    if image_capable && is_provider_image_path(&path) {
        let ingestor = ImageIngestor::new(artifact_store.clone(), ImageLimits::default());
        let result = if approved_external {
            ingestor.ingest_approved_dropped_path(workspace, &path, owner)
        } else {
            ingestor.ingest_dropped_path(workspace, &path, owner)
        };
        match result {
            Ok(attachment) => state.stage_image(attachment),
            Err(error) => state.set_status(format!("could not attach image {path}: {error}")),
        }
        return;
    }

    let ingestor = match ResourceIngestor::new(artifact_store.clone(), policy.clone()) {
        Ok(ingestor) => ingestor,
        Err(error) => {
            state.set_status(format!("could not initialize resource validation: {error}"));
            return;
        }
    };
    let result = if approved_external {
        ingestor.ingest_approved_path(workspace, &path, owner)
    } else {
        ingestor.ingest_path(workspace, &path, owner)
    };
    match result {
        Ok(staged) => {
            let capabilities = project_resource_capabilities(
                &staged.resource,
                &ResourceCapabilityContextV1 {
                    presentation,
                    policy,
                    observed_at_unix_millis: observed_at_unix_millis(),
                    exact_route_facts: Vec::new(),
                },
            );
            match capabilities {
                Ok(capabilities) => state.stage_resource(AttachmentV1 {
                    id: Uuid::new_v4(),
                    resource: staged.resource,
                    provenance,
                    source_label: Some(staged.source_label),
                    capabilities,
                }),
                Err(error) => {
                    state.set_status(format!("resource capability validation failed: {error}"))
                }
            }
        }
        Err(error) => state.set_status(format!("could not attach resource {path}: {error}")),
    }
}

fn is_provider_image_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif"
            )
        })
}

fn baseline_tui_capabilities() -> PresentationCapabilities {
    PresentationCapabilities::tui(ColorCapability::None, true, true, true, false)
}

fn observed_at_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn classify_image_paths(
    workspace: &std::path::Path,
    paths: Vec<String>,
) -> Result<ClassifiedImagePaths, String> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| format!("could not resolve launch workspace: {error}"))?;
    let mut resolved = Vec::with_capacity(paths.len());
    let mut external_paths = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        match classify_dropped_image_path(workspace, &path) {
            Ok(DroppedImagePath::Workspace { relative }) => {
                if seen.insert(canonical_workspace.join(&relative)) {
                    resolved.push(relative);
                }
            }
            Ok(DroppedImagePath::External { canonical }) => {
                if !seen.insert(canonical.clone()) {
                    continue;
                }
                let canonical = canonical.to_string_lossy().into_owned();
                external_paths.push(canonical.clone());
                resolved.push(canonical);
            }
            Err(error) => return Err(format!("could not attach image {path}: {error}")),
        }
    }
    Ok(ClassifiedImagePaths {
        paths: resolved,
        external_paths,
    })
}

fn ingest_image_paths(
    ingestor: &ImageIngestor,
    workspace: &std::path::Path,
    paths: &[String],
    owner: crate::identity::PrincipalId,
    approved_external: bool,
) -> Result<Vec<ImageAttachment>, String> {
    if paths.len() > MAX_IMAGES_PER_TURN {
        return Err(format!(
            "at most {MAX_IMAGES_PER_TURN} images may be attached to one turn"
        ));
    }
    let mut total_bytes = 0_u64;
    for path in paths {
        let path = std::path::Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        let byte_len = std::fs::metadata(&path)
            .map_err(|error| format!("could not inspect image {}: {error}", path.display()))?
            .len();
        total_bytes = total_bytes.saturating_add(byte_len);
        if total_bytes > MAX_IMAGE_BYTES_PER_TURN {
            return Err(format!(
                "image attachments exceed the {} MiB per-turn budget",
                MAX_IMAGE_BYTES_PER_TURN / (1024 * 1024)
            ));
        }
    }
    paths
        .iter()
        .map(|path| {
            let attachment = if approved_external {
                ingestor.ingest_approved_dropped_path(workspace, path, owner)
            } else {
                ingestor.ingest_dropped_path(workspace, path, owner)
            };
            attachment.map_err(|error| format!("could not attach image {path}: {error}"))
        })
        .collect()
}

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
        // File completion is frontend-local and is consumed by the event
        // runner before runtime effects are dispatched.
        UpdateEffect::CompleteFile { .. } => {}
        UpdateEffect::Quit => return Ok(Some(ChatExit::Quit)),
        UpdateEffect::NewConversation => return Ok(Some(ChatExit::NewConversation)),
        UpdateEffect::Doctor => return Ok(Some(ChatExit::Doctor(None))),
        UpdateEffect::Reset => return Ok(Some(ChatExit::Reset)),
        UpdateEffect::Setup(section) => return Ok(Some(ChatExit::Setup(section))),
        UpdateEffect::Settings(section) => return Ok(Some(ChatExit::Settings(section))),
        UpdateEffect::ControlCommand { family, arguments } => {
            return Ok(Some(ChatExit::ControlCommand { family, arguments }));
        }
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
            vision_route,
        } => {
            let model = driver.models.iter().find(|model| model.id == state.model);
            if vision_route.is_some() {
                state.restore_submission(
                    input,
                    images,
                    vision_route,
                    "Managed Codex owns image interpretation; specialist vision routing is available in native Xana conversations".to_owned(),
                );
            } else if !images.is_empty()
                && !model.is_some_and(|model| model.input_modalities.contains("image"))
            {
                state.restore_submission(
                    input,
                    images,
                    None,
                    format!(
                        "{}/{} is not advertised as image-capable",
                        state.connection, state.model
                    ),
                );
            } else if let Err(reason) = driver
                .submit(operation_id, input.clone(), images.clone())
                .await
            {
                state.restore_submission(input, images, None, reason);
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
            state.restore_submission(input, Vec::new(), None, "Codex app-server does not advertise same-turn steering; message retained as a draft".to_owned());
        }
        UpdateEffect::PrepareVision {
            input,
            images,
            plan,
            ..
        } => state.restore_submission(
            input,
            images,
            Some(plan.route.name),
            "Specialist vision routing is available in native Xana conversations; Codex manages its own image input".to_owned(),
        ),
        UpdateEffect::Attach(path) => {
            let image_capable = driver
                .models
                .iter()
                .find(|model| model.id == state.model)
                .is_some_and(|model| model.input_modalities.contains("image"));
            stage_typed_path(
                state,
                workspace,
                artifact_store,
                owner,
                ResourcePolicyV1::default(),
                baseline_tui_capabilities(),
                path,
                false,
                image_capable,
                AttachmentProvenanceV1::UserSelected,
            );
        }
        UpdateEffect::AttachDropped(path) => {
            let image_capable = driver
                .models
                .iter()
                .find(|model| model.id == state.model)
                .is_some_and(|model| model.input_modalities.contains("image"));
            stage_typed_path(
                state,
                workspace,
                artifact_store,
                owner,
                ResourcePolicyV1::default(),
                baseline_tui_capabilities(),
                path,
                false,
                image_capable,
                AttachmentProvenanceV1::DragAndDrop,
            );
        }
        UpdateEffect::AttachApproved(path) => {
            let image_capable = driver
                .models
                .iter()
                .find(|model| model.id == state.model)
                .is_some_and(|model| model.input_modalities.contains("image"));
            stage_typed_path(
                state,
                workspace,
                artifact_store,
                owner,
                ResourcePolicyV1::default(),
                baseline_tui_capabilities(),
                path,
                true,
                image_capable,
                AttachmentProvenanceV1::DragAndDrop,
            );
        }
        UpdateEffect::AttachAndSubmit {
            operation_id,
            input,
            paths,
            approved_external,
        } => {
            let model = driver.models.iter().find(|model| model.id == state.model);
            if !model.is_some_and(|model| model.input_modalities.contains("image")) {
                let next = state.submit_without_auto_attachment(input);
                return Box::pin(dispatch_managed_effect(
                    next,
                    state,
                    driver,
                    workspace_host,
                    workspace,
                    artifact_store,
                    owner,
                    preferences_path,
                    session_preferences,
                    pending_approval,
                    clipboard,
                ))
                .await;
            }
            let classified = match classify_image_paths(workspace, paths) {
                Ok(classified) => classified,
                Err(reason) => {
                    state.restore_auto_attachment_draft(input, reason);
                    return Ok(None);
                }
            };
            if !classified.external_paths.is_empty() && !approved_external {
                state.request_external_image_approval(
                    operation_id,
                    input,
                    classified.paths,
                    classified.external_paths,
                );
                return Ok(None);
            }
            let ingestor = ImageIngestor::new(artifact_store.clone(), ImageLimits::default());
            match ingest_image_paths(
                &ingestor,
                workspace,
                &classified.paths,
                owner,
                approved_external,
            ) {
                Ok(attachments) => {
                    let next = state.attach_and_submit(input, attachments);
                    return Box::pin(dispatch_managed_effect(
                        next,
                        state,
                        driver,
                        workspace_host,
                        workspace,
                        artifact_store,
                        owner,
                        preferences_path,
                        session_preferences,
                        pending_approval,
                        clipboard,
                    ))
                    .await;
                }
                Err(reason) => state.restore_auto_attachment_draft(input, reason),
            }
        }
        UpdateEffect::AttachClipboard => match clipboard.get_image(artifact_store.clone(), owner) {
            Ok(attachment) => state.stage_image(attachment),
            Err(error) => state.set_status(error),
        },
        UpdateEffect::SelectModel(selection) => {
            let selection = selection.split_whitespace().next().unwrap_or(&selection);
            let requested = selection
                .split_once('/')
                .map_or(selection, |(_, model)| model);
            if !driver.models.iter().any(|model| model.id == requested) {
                state.set_status(format!("Codex does not advertise model {requested:?}"));
            } else {
                let selected = driver
                    .select_model(requested.to_owned())
                    .await
                    .map_err(anyhow::Error::msg)?;
                state.set_model(selected.model);
            }
        }
        UpdateEffect::SetReasoning(effort) => {
            let selected = driver
                .set_reasoning((effort != "auto").then_some(effort))
                .await
                .map_err(anyhow::Error::msg)?;
            state.set_status(format!(
                "Reasoning {} for subsequent turns; managed context is unchanged",
                selected.reasoning_effort.as_deref().unwrap_or("auto")
            ));
        }
        UpdateEffect::PersistComposer(preset) => {
            if let Err(error) = PresentationPreferences::set_composer(preferences_path, preset) {
                state.set_status(format!("could not save composer preference: {error}"));
            }
        }
        UpdateEffect::ClearConversation => driver.clear().await.map_err(anyhow::Error::msg)?,
        UpdateEffect::CompactConversation { .. } => {
            state.set_status("This managed runtime owns its context; Xana compaction is unavailable")
        }
        UpdateEffect::BrowserControl(_) => state.set_status("Managed runtimes own their browser tools; Xana's local browser controls require native execution"),
        UpdateEffect::DecideRoundBudget { .. } => state
            .set_status("Managed runtimes own their continuation and do not use Xana round tranches"),
        UpdateEffect::OpenModelPicker => state.open_model_picker(
            driver
                .models
                .iter()
                .map(|model| format_model_choice(&state.connection, model))
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
        UpdateEffect::SwitchConversation(conversation) => {
            return Ok(Some(ChatExit::SwitchConversation(conversation)));
        }
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            load_older_history(state, workspace_host, &conversation);
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::LoadNewer(conversation) => {
            let Some(start) = state.history_newer_start() else { return Ok(None) };
            match workspace_host.conversation_history_from(&conversation, start, 128) {
                Ok(Some(page)) => state.replace_newer_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load newer history: {error}")),
            }
        }
        UpdateEffect::ArchiveConversation(conversation) => {
            let archived = match &conversation {
                ConversationRef::Managed {
                    connection,
                    thread_id,
                    ..
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
            apply_artifact_action(state, artifact_store, workspace, clipboard, record, action)?;
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
    workspace: &std::path::Path,
    clipboard: &mut clipboard::Clipboard,
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
                let range = store
                    .read_verified_range(
                        &record,
                        0,
                        PREVIEW_BYTES - 128,
                        crate::artifact::MAX_ARTIFACT_BYTES,
                    )
                    .context("could not read artifact preview")?;
                let mut text = String::from_utf8_lossy(&range.bytes).into_owned();
                if range.truncated_after {
                    text.push_str("\n[preview truncated; complete artifact retained]");
                }
                text
            } else {
                format!(
                    "[binary preview omitted: {} · {} bytes]",
                    record.media_type, record.byte_len
                )
            };
            state.show_artifact_preview(record, preview);
        }
        ArtifactAction::CopyReference => {
            let reference = format!("artifact:{}", record.reference.id);
            clipboard.set_text(reference).map_err(anyhow::Error::msg)?;
            state.set_status("Immutable artifact reference copied");
        }
        ArtifactAction::Save => save_artifact_copy(state, store, workspace, &record)?,
        ArtifactAction::InsertReference => state.insert_artifact_reference(&record),
        ArtifactAction::Reveal | ArtifactAction::Open => {
            crate::artifact_action::launch_verified(
                store,
                &record,
                crate::artifact::MAX_ARTIFACT_BYTES,
                if action == ArtifactAction::Reveal {
                    crate::artifact_action::ExternalArtifactAction::Reveal
                } else {
                    crate::artifact_action::ExternalArtifactAction::Open
                },
            )?;
            state.set_status(if action == ArtifactAction::Reveal {
                "Artifact revealed in the OS file manager"
            } else {
                "Artifact opened with the OS default application"
            });
        }
    }
    Ok(())
}

fn save_artifact_copy(
    state: &mut TuiState,
    store: &crate::artifact::ArtifactStore,
    workspace: &std::path::Path,
    record: &crate::artifact::ArtifactRecord,
) -> Result<()> {
    let directory = workspace.join("xana-artifacts");
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "could not create artifact export directory {}",
            directory.display()
        )
    })?;
    let extension = crate::artifact_action::extension_for_media_type(&record.media_type);
    let destination = directory.join(format!("{}.{extension}", record.reference.id));
    store
        .copy_verified_create_new(record, &destination, crate::artifact::MAX_ARTIFACT_BYTES)
        .with_context(|| {
            format!(
                "could not create verified artifact copy at {}; existing exports are never overwritten",
                destination.display()
            )
        })?;
    state.set_status(format!(
        "Saved verified artifact copy to {}",
        destination.display()
    ));
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
        // File completion is frontend-local and is consumed by the event
        // runner before runtime effects are dispatched.
        UpdateEffect::CompleteFile { .. } => {}
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
        UpdateEffect::Settings(section) => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::Settings(section)));
        }
        UpdateEffect::ControlCommand { family, arguments } => {
            let _ = client.send(RuntimeCommand::Shutdown).await;
            return Ok(Some(ChatExit::ControlCommand { family, arguments }));
        }
        UpdateEffect::Submit {
            operation_id,
            input,
            images,
            vision_route,
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
                        vision_route,
                        format!(
                            "{}/{} is not declared image-capable; refresh its catalog or add an explicit model override",
                            header.provider_name, header.model
                        ),
                    );
                    return Ok(None);
                }
            }
            let lease = match workspace_host
                .acquire_foreground_root(conversation.clone())
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    state.restore_submission(
                        input,
                        images,
                        vision_route,
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
                    vision_route,
                    result
                        .reason
                        .unwrap_or_else(|| "command rejected".to_owned()),
                );
            }
        }
        UpdateEffect::PrepareVision {
            input,
            images,
            plan,
            ..
        } => state.restore_submission(
            input,
            images,
            Some(plan.route.name),
            "Vision preparation was not claimed by the native execution owner".to_owned(),
        ),
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
            } else {
                *active_root = None;
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
            stage_typed_path(
                state,
                &header.workspace_root,
                &header.artifact_store,
                header.owner,
                header.resource_policy.clone(),
                header.presentation.tui_capabilities(true, true, false),
                path,
                false,
                descriptor.input_modalities.contains("image"),
                AttachmentProvenanceV1::UserSelected,
            );
        }
        UpdateEffect::AttachDropped(path) => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            stage_typed_path(
                state,
                &header.workspace_root,
                &header.artifact_store,
                header.owner,
                header.resource_policy.clone(),
                header.presentation.tui_capabilities(true, true, false),
                path,
                false,
                descriptor.input_modalities.contains("image"),
                AttachmentProvenanceV1::DragAndDrop,
            );
        }
        UpdateEffect::AttachApproved(path) => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            stage_typed_path(
                state,
                &header.workspace_root,
                &header.artifact_store,
                header.owner,
                header.resource_policy.clone(),
                header.presentation.tui_capabilities(true, true, false),
                path,
                true,
                descriptor.input_modalities.contains("image"),
                AttachmentProvenanceV1::DragAndDrop,
            );
        }
        UpdateEffect::AttachAndSubmit {
            operation_id,
            input,
            paths,
            approved_external,
        } => {
            let descriptor = header
                .models
                .descriptor(&header.provider_name, &header.model)
                .context("could not resolve selected model capabilities")?;
            if !descriptor.input_modalities.contains("image") {
                let next = state.submit_without_auto_attachment(input);
                return Box::pin(dispatch_effect(
                    next,
                    state,
                    client,
                    header,
                    workspace_host,
                    conversation,
                    active_root,
                    preferences_path,
                    session_preferences,
                    clipboard,
                ))
                .await;
            }
            let classified = match classify_image_paths(&header.workspace_root, paths) {
                Ok(classified) => classified,
                Err(reason) => {
                    state.restore_auto_attachment_draft(input, reason);
                    return Ok(None);
                }
            };
            if !classified.external_paths.is_empty() && !approved_external {
                state.request_external_image_approval(
                    operation_id,
                    input,
                    classified.paths,
                    classified.external_paths,
                );
                return Ok(None);
            }
            let ingestor =
                ImageIngestor::new(header.artifact_store.clone(), ImageLimits::default());
            match ingest_image_paths(
                &ingestor,
                &header.workspace_root,
                &classified.paths,
                header.owner,
                approved_external,
            ) {
                Ok(attachments) => {
                    let next = state.attach_and_submit(input, attachments);
                    return Box::pin(dispatch_effect(
                        next,
                        state,
                        client,
                        header,
                        workspace_host,
                        conversation,
                        active_root,
                        preferences_path,
                        session_preferences,
                        clipboard,
                    ))
                    .await;
                }
                Err(reason) => state.restore_auto_attachment_draft(input, reason),
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
            let selection = selection.split_whitespace().next().unwrap_or(&selection);
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
        UpdateEffect::CompactConversation { operation_id } => {
            let result = client
                .send(RuntimeCommand::CompactConversation { operation_id })
                .await
                .context("native TUI runtime stopped while compacting")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "compaction was rejected".to_owned()),
                );
            }
        }
        UpdateEffect::BrowserControl(action) => {
            let result = client
                .send(RuntimeCommand::BrowserControl { action })
                .await
                .context("native TUI runtime unavailable")?;
            if !result.accepted {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "Browser control rejected".into()),
                );
            }
        }
        UpdateEffect::DecideRoundBudget { suspension, action } => {
            let mut lease = None;
            if action == crate::native_runtime::RoundBudgetAction::Continue && active_root.is_none()
            {
                match workspace_host
                    .acquire_foreground_root(conversation.clone())
                    .await
                {
                    Ok(acquired) => lease = Some(acquired),
                    Err(error) => {
                        state.set_status(format!("could not continue turn: {error}"));
                        return Ok(None);
                    }
                }
            }
            let result = client
                .send(RuntimeCommand::DecideRoundBudget {
                    operation_id: suspension.operation_id,
                    suspension_id: suspension.id,
                    action,
                })
                .await
                .context("native TUI runtime stopped during round-budget decision")?;
            if result.accepted {
                if let Some(lease) = lease {
                    *active_root = Some(lease);
                }
                if action == crate::native_runtime::RoundBudgetAction::Stop {
                    *active_root = None;
                }
            } else {
                state.set_status(
                    result
                        .reason
                        .unwrap_or_else(|| "round-budget decision was rejected".to_owned()),
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
                        .map(move |model| format_model_choice(&summary.id, &model))
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
        UpdateEffect::SwitchConversation(conversation) => {
            return Ok(Some(ChatExit::SwitchConversation(conversation)));
        }
        UpdateEffect::ViewSession(conversation) => {
            match workspace_host.conversation_history_page(&conversation, None, 128) {
                Ok(page) => state.view_session_page(conversation, page),
                Err(error) => state.set_status(format!("could not inspect conversation: {error}")),
            }
        }
        UpdateEffect::LoadOlder(conversation) => {
            load_older_history(state, workspace_host, &conversation);
        }
        UpdateEffect::PersistRail(expanded) => {
            if let Err(error) = session_preferences.set_rail_expanded(expanded) {
                state.set_status(format!("could not save session rail preference: {error}"));
            }
        }
        UpdateEffect::LoadNewer(conversation) => {
            let Some(start) = state.history_newer_start() else {
                return Ok(None);
            };
            match workspace_host.conversation_history_from(&conversation, start, 128) {
                Ok(Some(page)) => state.replace_newer_page(page),
                Ok(None) => state.set_status("Managed history remains owned by its runtime"),
                Err(error) => state.set_status(format!("could not load newer history: {error}")),
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
            apply_artifact_action(
                state,
                &header.artifact_store,
                &header.workspace_root,
                clipboard,
                record,
                action,
            )?;
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

fn format_model_choice(connection: &str, model: &crate::model_catalog::ModelDescriptor) -> String {
    let mut capabilities = model.input_modalities.iter().cloned().collect::<Vec<_>>();
    if model.tools == Some(true) {
        capabilities.push("tools".to_owned());
    }
    if model.reasoning == Some(true) {
        capabilities.push("reasoning".to_owned());
    }
    if let Some(context) = model.context_tokens {
        capabilities.push(format!("{}k ctx", context.div_ceil(1_000)));
    }
    if !model.output_modalities.is_empty() {
        capabilities.push(format!(
            "out {}",
            model
                .output_modalities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("+")
        ));
    }
    if let Some(pricing) = model.pricing.summary() {
        capabilities.push(pricing);
    }
    format!("{connection}/{}  [{}]", model.id, capabilities.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_text_artifact_opens_a_bounded_preview_without_clipboard_access() {
        let data = tempfile::tempdir().unwrap();
        let store = crate::artifact::ArtifactStore::new(data.path().join("artifacts"));
        let (record, _) = store
            .put(
                &vec![b'x'; 80 * 1024],
                "application/json",
                crate::identity::PrincipalId::new(),
            )
            .unwrap();
        let mut state = TuiState::starting(crate::presentation::ComposerPreset::Submit);
        apply_artifact_action(
            &mut state,
            &store,
            data.path(),
            &mut clipboard::Clipboard::default(),
            record,
            ArtifactAction::Preview,
        )
        .unwrap();
        let Some(super::super::state::Overlay::Artifact {
            preview: Some(preview),
            ..
        }) = &state.overlay
        else {
            panic!("expected artifact preview")
        };
        assert!(preview.len() <= 64 * 1024);
        assert!(preview.contains("preview truncated; complete artifact retained"));
    }

    #[test]
    fn image_path_classification_preserves_mixed_input_order_and_external_review() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("inside.png"), b"inside").unwrap();
        let external = outside.path().join("outside.png");
        std::fs::write(&external, b"outside").unwrap();

        let classified = classify_image_paths(
            workspace.path(),
            vec![
                "inside.png".to_owned(),
                external.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();

        assert_eq!(classified.paths[0], "inside.png");
        assert_eq!(
            classified.paths[1],
            external.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(classified.external_paths, vec![classified.paths[1].clone()]);
    }
}
