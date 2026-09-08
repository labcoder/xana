//! Pure application-owned projection from Xana's Desktop protocol to UI state.

mod window;

use gpui::{Image, SharedString};
use gpui_ai::prelude::{
    Attachment, AttachmentKind, ChatMessage, ChatRole, MessageActions, StreamedContent,
};
use std::{
    cell::OnceCell,
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use xana::desktop::{
    DesktopActivityItem, DesktopAuthority, DesktopContent, DesktopContentValue,
    DesktopConversationFacts, DesktopEvent, DesktopHostEvent, DesktopHostObservation,
    DesktopMessage, DesktopObservation, DesktopOperationId, DesktopOperationState,
    DesktopPermissionId, DesktopResource, DesktopResourceKind, DesktopRole,
    DesktopRoundBudgetSuspension, DesktopSnapshot, NotificationPolicy,
};

const MAX_INLINE_PREVIEWS: usize = 8;
const MAX_INLINE_PREVIEW_BYTES: u64 = 20 * 1024 * 1024;
const MAX_INLINE_PREVIEW_DECODED_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectedMessage {
    id: String,
    role: ChatRole,
    text: String,
    resources: Vec<DesktopResource>,
    lifecycle: MessageLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageLifecycle {
    Running,
    Complete,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingApproval {
    pub(crate) id: DesktopPermissionId,
    pub(crate) tool: String,
    pub(crate) effect: String,
    pub(crate) scope: String,
    pub(crate) public_web: bool,
}

/// Xana Desktop owns this state; `gpui-ai` receives immutable snapshots.
pub(crate) struct ConversationProjection {
    authority: DesktopAuthority,
    attached_to_foreground_host: bool,
    connection: String,
    execution_owner: String,
    model: String,
    reasoning_effort: Option<String>,
    artifact_count: usize,
    messages: VecDeque<ProjectedMessage>,
    cached_messages: OnceCell<Arc<[ChatMessage]>>,
    history_omitted: bool,
    image_previews: HashMap<String, Arc<Image>>,
    streams: HashMap<DesktopOperationId, String>,
    message_operations: HashMap<DesktopOperationId, String>,
    sequence: u64,
    active_operation: Option<DesktopOperationId>,
    pending_round_budget: Option<DesktopRoundBudgetSuspension>,
    notification_policy: NotificationPolicy,
    pending_approval_count: usize,
    pending_approvals: Vec<PendingApproval>,
    host_lifecycle: String,
    global_notice_count: usize,
    conversation_facts: DesktopConversationFacts,
    latest_activity: String,
    failure: Option<String>,
}

impl ConversationProjection {
    pub(crate) fn from_snapshot(snapshot: &DesktopSnapshot) -> Self {
        let mut projection = Self {
            authority: snapshot.authority,
            attached_to_foreground_host: snapshot.attached_to_foreground_host,
            connection: snapshot.connection.clone(),
            execution_owner: snapshot.execution_owner.clone(),
            model: snapshot.model.clone(),
            reasoning_effort: snapshot.reasoning_effort.clone(),
            artifact_count: snapshot.artifact_count,
            messages: snapshot.conversation.iter().map(project_message).collect(),
            cached_messages: OnceCell::new(),
            history_omitted: snapshot.conversation_truncated,
            image_previews: HashMap::new(),
            streams: HashMap::new(),
            message_operations: HashMap::new(),
            sequence: snapshot.sequence,
            active_operation: snapshot.active_operation,
            pending_round_budget: None,
            notification_policy: snapshot.notification_policy.clone(),
            pending_approval_count: snapshot.pending_approval_count,
            pending_approvals: snapshot
                .pending_approvals
                .iter()
                .map(|approval| PendingApproval {
                    public_web: approval.public_web,
                    id: approval.id,
                    tool: approval.tool.clone(),
                    effect: approval.effect.clone(),
                    scope: approval.scope.clone(),
                })
                .collect(),
            host_lifecycle: snapshot.host_lifecycle.clone(),
            global_notice_count: snapshot.global_notices.len(),
            conversation_facts: snapshot.conversation_facts.clone(),
            latest_activity: format!(
                "{} / {} · session {}",
                snapshot.connection, snapshot.model, snapshot.session_id
            ),
            failure: None,
        };
        projection.bound_history();
        projection
    }

    pub(crate) fn replace_snapshot(&mut self, snapshot: &DesktopSnapshot) {
        let previews = std::mem::take(&mut self.image_previews);
        *self = Self::from_snapshot(snapshot);
        self.image_previews = previews;
        let admitted = self
            .preview_window_ids()
            .into_iter()
            .map(str::to_owned)
            .collect();
        retain_admitted_previews(&mut self.image_previews, &admitted);
    }

    pub(crate) fn append_user(&mut self, operation_id: DesktopOperationId, text: String) {
        self.cached_messages.take();
        self.messages.push_back(ProjectedMessage {
            id: format!("desktop-user-{operation_id}"),
            role: ChatRole::User,
            text,
            resources: Vec::new(),
            lifecycle: MessageLifecycle::Complete,
        });
        self.active_operation = Some(operation_id);
        self.latest_activity = "Request queued".to_owned();
        self.failure = None;
        self.bound_history();
    }

    pub(crate) fn reject_user(&mut self, operation_id: DesktopOperationId) {
        self.cached_messages.take();
        let id = format!("desktop-user-{operation_id}");
        self.messages.retain(|message| message.id != id);
        if self.active_operation == Some(operation_id) {
            self.active_operation = None;
        }
    }

    /// Applies one observation. `false` means the caller must request a snapshot.
    pub(crate) fn apply(&mut self, observation: DesktopObservation) -> bool {
        if observation.sequence != self.sequence.saturating_add(1) {
            return false;
        }
        self.sequence = observation.sequence;
        match observation.event {
            DesktopEvent::OperationState {
                operation_id,
                state,
            } => {
                self.active_operation = matches!(
                    state,
                    DesktopOperationState::Running | DesktopOperationState::Suspended
                )
                .then_some(operation_id);
                self.latest_activity = operation_label(state).to_owned();
                if matches!(
                    state,
                    DesktopOperationState::Completed
                        | DesktopOperationState::Declined
                        | DesktopOperationState::Interrupted
                ) {
                    self.finish_stream(operation_id, None);
                } else if state == DesktopOperationState::Failed {
                    self.finish_stream(operation_id, Some("Operation failed".to_owned()));
                }
            }
            DesktopEvent::AssistantDelta { operation_id, text } => {
                self.append_stream(operation_id, text);
                self.latest_activity = "Xana is responding".to_owned();
            }
            DesktopEvent::ReasoningDelta { .. } => {
                self.latest_activity = "Xana is reasoning".to_owned();
            }
            DesktopEvent::ExecutionSelectionChanged {
                model,
                reasoning_effort,
                receipt,
            } => {
                self.model = model;
                self.reasoning_effort = reasoning_effort;
                self.latest_activity = receipt;
                self.failure = None;
            }
            DesktopEvent::MessageFinal {
                operation_id,
                message,
            } => self.replace_stream_with_final(operation_id, message),
            DesktopEvent::UserMessageCommitted {
                operation_id,
                message,
            } => {
                self.cached_messages.take();
                let optimistic = format!("desktop-user-{operation_id}");
                let projected = project_message(&message);
                if let Some(existing) = self
                    .messages
                    .iter_mut()
                    .find(|row| row.id == optimistic || row.id == projected.id)
                {
                    *existing = projected;
                } else {
                    self.messages.push_back(projected);
                }
            }
            DesktopEvent::ToolResult { message, activity } => {
                self.cached_messages.take();
                let projected = project_message(&message);
                if let Some(existing) = self.messages.iter_mut().find(|row| row.id == projected.id)
                {
                    *existing = projected;
                } else {
                    self.messages.push_back(projected);
                }
                if let Some(activity) = activity {
                    self.upsert_activity(activity);
                }
                self.latest_activity = "Tool finished".to_owned();
            }
            DesktopEvent::PermissionRequired {
                permission_id,
                tool,
                effect,
                scope,
                public_web,
            } => {
                if let Some(existing) = self
                    .pending_approvals
                    .iter_mut()
                    .find(|approval| approval.id == permission_id)
                {
                    *existing = PendingApproval {
                        public_web,
                        id: permission_id,
                        tool: tool.clone(),
                        effect,
                        scope,
                    };
                } else {
                    self.pending_approvals.push(PendingApproval {
                        public_web,
                        id: permission_id,
                        tool: tool.clone(),
                        effect,
                        scope,
                    });
                }
                self.pending_approval_count = self.pending_approvals.len();
                self.latest_activity = format!("Approval required for {tool}");
            }
            DesktopEvent::PermissionResolved { permission_id } => {
                self.pending_approvals
                    .retain(|approval| approval.id != permission_id);
                self.pending_approval_count = self.pending_approvals.len();
                self.latest_activity = "Approval resolved".to_owned();
            }
            DesktopEvent::WebProgress {
                operation_id,
                label,
            } => {
                if self.active_operation == Some(operation_id) {
                    self.latest_activity = label;
                }
            }
            DesktopEvent::RoundBudgetReached(suspension) => {
                self.latest_activity = format!(
                    "Round budget reached: {} / {}; {} remain",
                    suspension.rounds_consumed,
                    suspension.hard_round_limit,
                    suspension.remaining_rounds,
                );
                self.pending_round_budget = Some(suspension);
            }
            DesktopEvent::RoundBudgetDecision {
                suspension_id,
                continued,
            } => {
                if self
                    .pending_round_budget
                    .as_ref()
                    .is_some_and(|pending| pending.id == suspension_id)
                {
                    self.pending_round_budget = None;
                }
                self.latest_activity = if continued {
                    "Continuing the same operation".to_owned()
                } else {
                    "Stopping the suspended operation".to_owned()
                };
            }
            DesktopEvent::Usage { .. } => {
                self.latest_activity = "Usage updated".to_owned();
            }
            DesktopEvent::ConversationCleared => {
                self.cached_messages.take();
                self.messages.clear();
                self.history_omitted = false;
                self.streams.clear();
                self.message_operations.clear();
                self.active_operation = None;
                self.pending_round_budget = None;
                self.latest_activity = "Conversation cleared".to_owned();
            }
            DesktopEvent::Activity { label } => self.latest_activity = label,
            DesktopEvent::ActivityUpserted(activity) => {
                self.upsert_activity(activity);
            }
            DesktopEvent::Error(error) => {
                self.failure = Some(error.message.clone());
                self.latest_activity = error.message;
            }
        }
        self.bound_history();
        true
    }

    pub(crate) fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        if let Some(operation_id) = self.active_operation.take() {
            self.finish_stream(operation_id, Some(message.clone()));
        }
        self.failure = Some(message.clone());
        self.latest_activity = message;
        self.bound_history();
    }

    pub(crate) fn apply_host(&mut self, observation: &DesktopHostObservation) {
        match &observation.event {
            DesktopHostEvent::LifecycleChanged { state } => {
                self.host_lifecycle.clone_from(state);
            }
            DesktopHostEvent::GlobalNotice(_) => {
                self.global_notice_count = self.global_notice_count.saturating_add(1);
            }
            DesktopHostEvent::ControllerChanged { change, .. }
                if matches!(change.as_str(), "released" | "expired")
                    || change.starts_with("disconnected:") =>
            {
                self.latest_activity = "Conversation controller needs attention".to_owned();
            }
            DesktopHostEvent::ShutdownCompleted { .. } => {
                self.host_lifecycle = "stopped".to_owned();
            }
            _ => {}
        }
    }

    pub(crate) fn messages(&self) -> Arc<[ChatMessage]> {
        Arc::clone(self.cached_messages.get_or_init(|| {
            self.messages
                .iter()
                .map(|message| {
                    let content = match &message.lifecycle {
                        MessageLifecycle::Running => StreamedContent::running(message.text.clone()),
                        MessageLifecycle::Complete => StreamedContent::done(message.text.clone()),
                        MessageLifecycle::Failed(reason) => {
                            StreamedContent::failed(message.text.clone(), reason.clone())
                        }
                    };
                    ChatMessage::new(message.id.clone(), message.role, content)
                        .attachments(message.resources.iter().map(|resource| {
                            project_attachment(
                                resource,
                                self.image_previews.get(&resource.artifact_id),
                            )
                        }))
                        .actions(MessageActions::for_role(message.role))
                })
                .collect::<Vec<_>>()
                .into()
        }))
    }

    /// Saved pages offer copying only; they never inherit live retry/edit authority.
    pub(crate) fn saved_messages(messages: &[DesktopMessage]) -> Arc<[ChatMessage]> {
        messages
            .iter()
            .map(|message| {
                ChatMessage::new(
                    message.id.clone(),
                    project_role(message.role),
                    StreamedContent::done(
                        message
                            .content
                            .iter()
                            .map(project_content)
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                    ),
                )
                .actions(MessageActions::none().copy(true))
            })
            .collect::<Vec<_>>()
            .into()
    }

    pub(crate) fn history_omitted(&self) -> bool {
        self.history_omitted
    }

    pub(crate) fn connection(&self) -> &str {
        &self.connection
    }

    pub(crate) fn authority(&self) -> DesktopAuthority {
        self.authority
    }

    pub(crate) fn attached_to_foreground_host(&self) -> bool {
        self.attached_to_foreground_host
    }

    pub(crate) fn can_start_conversation(&self) -> bool {
        !self.attached_to_foreground_host
            && matches!(
                self.authority,
                DesktopAuthority::Controller | DesktopAuthority::Owner
            )
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn execution_owner(&self) -> &str {
        &self.execution_owner
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub(crate) fn artifact_count(&self) -> usize {
        self.artifact_count
    }

    /// Returns only the newest eligible resources within the compiled
    /// concurrent-preview budget. The runtime reader enforces per-resource
    /// policy again before any bytes cross the boundary.
    pub(crate) fn pending_preview_resources(&self) -> Vec<DesktopResource> {
        let admitted = self
            .preview_window_ids()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        self.messages
            .iter()
            .rev()
            .flat_map(|message| message.resources.iter().rev())
            .filter(|resource| {
                admitted.contains(resource.artifact_id.as_str())
                    && !self.image_previews.contains_key(&resource.artifact_id)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn install_image_preview(&mut self, artifact_id: String, image: Arc<Image>) {
        if self
            .preview_window_ids()
            .into_iter()
            .any(|candidate| candidate == artifact_id)
        {
            self.cached_messages.take();
            self.image_previews.insert(artifact_id, image);
        }
    }

    fn preview_window_ids(&self) -> Vec<&str> {
        select_preview_window(
            self.messages
                .iter()
                .rev()
                .flat_map(|message| message.resources.iter().rev())
                .map(|resource| PreviewFact {
                    id: resource.artifact_id.as_str(),
                    bytes: resource.byte_len,
                    decoded_bytes: estimated_rgba_bytes(resource),
                    eligible: resource.supports_inline_preview(),
                }),
        )
    }

    pub(crate) fn resource(&self, artifact_id: &str) -> Option<&DesktopResource> {
        self.messages
            .iter()
            .flat_map(|message| &message.resources)
            .find(|resource| resource.artifact_id == artifact_id)
    }

    pub(crate) fn recent_resources(&self) -> Vec<&DesktopResource> {
        let mut seen = std::collections::HashSet::new();
        self.messages
            .iter()
            .rev()
            .flat_map(|message| message.resources.iter().rev())
            .filter(|resource| seen.insert(resource.artifact_id.as_str()))
            .take(128)
            .collect()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.active_operation.is_some() && self.pending_round_budget.is_none()
    }

    pub(crate) fn active_operation(&self) -> Option<DesktopOperationId> {
        self.active_operation
    }

    pub(crate) fn notification_policy(&self) -> &NotificationPolicy {
        &self.notification_policy
    }

    pub(crate) fn pending_approval_count(&self) -> usize {
        self.pending_approval_count
    }

    pub(crate) fn pending_approvals(&self) -> &[PendingApproval] {
        &self.pending_approvals
    }

    pub(crate) fn host_lifecycle(&self) -> &str {
        &self.host_lifecycle
    }

    pub(crate) fn global_notice_count(&self) -> usize {
        self.global_notice_count
    }

    pub(crate) fn pending_round_budget(&self) -> Option<&DesktopRoundBudgetSuspension> {
        self.pending_round_budget.as_ref()
    }

    pub(crate) fn latest_activity(&self) -> &str {
        &self.latest_activity
    }

    pub(crate) fn conversation_facts(&self) -> &DesktopConversationFacts {
        &self.conversation_facts
    }

    pub(crate) fn set_activity(&mut self, activity: impl Into<String>) {
        self.latest_activity = activity.into();
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub(crate) fn operation_for_message(&self, message_id: &str) -> Option<DesktopOperationId> {
        self.streams
            .iter()
            .chain(self.message_operations.iter())
            .find_map(|(operation_id, candidate)| {
                (candidate == message_id).then_some(*operation_id)
            })
    }

    pub(crate) fn preceding_user_text(&self, message_id: &str) -> Option<&str> {
        let index = self
            .messages
            .iter()
            .position(|message| message.id == message_id)?;
        self.messages
            .range(..index)
            .rev()
            .find(|message| message.role == ChatRole::User)
            .map(|message| message.text.as_str())
    }

    fn append_stream(&mut self, operation_id: DesktopOperationId, delta: String) {
        self.cached_messages.take();
        let id = self
            .streams
            .entry(operation_id)
            .or_insert_with(|| format!("desktop-stream-{operation_id}"))
            .clone();
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            window::append_text(&mut message.text, &delta);
            message.lifecycle = MessageLifecycle::Running;
        } else {
            self.messages.push_back(ProjectedMessage {
                id,
                role: ChatRole::Assistant,
                text: delta,
                resources: Vec::new(),
                lifecycle: MessageLifecycle::Running,
            });
        }
    }

    fn replace_stream_with_final(
        &mut self,
        operation_id: DesktopOperationId,
        message: DesktopMessage,
    ) {
        self.cached_messages.take();
        if let Some(stream_id) = self.streams.remove(&operation_id)
            && let Some(index) = self
                .messages
                .iter()
                .position(|candidate| candidate.id == stream_id)
        {
            let projected = project_message(&message);
            self.message_operations
                .insert(operation_id, projected.id.clone());
            self.messages[index] = projected;
            return;
        }
        if !self
            .messages
            .iter()
            .any(|candidate| candidate.id == message.id)
        {
            let projected = project_message(&message);
            self.message_operations
                .insert(operation_id, projected.id.clone());
            self.messages.push_back(projected);
        }
    }

    fn finish_stream(&mut self, operation_id: DesktopOperationId, failure: Option<String>) {
        self.cached_messages.take();
        let id = self
            .streams
            .get(&operation_id)
            .or_else(|| self.message_operations.get(&operation_id))
            .cloned()
            .unwrap_or_else(|| {
                let id = format!("desktop-stream-{operation_id}");
                self.streams.insert(operation_id, id.clone());
                id
            });
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.lifecycle = failure
                .map(MessageLifecycle::Failed)
                .unwrap_or(MessageLifecycle::Complete);
        } else if let Some(reason) = failure {
            self.messages.push_back(ProjectedMessage {
                id,
                role: ChatRole::Assistant,
                text: "The Run ended before Xana received an assistant response.".to_owned(),
                resources: Vec::new(),
                lifecycle: MessageLifecycle::Failed(reason),
            });
        }
    }

    fn upsert_activity(&mut self, activity: DesktopActivityItem) {
        self.latest_activity = activity
            .disclosed_text
            .clone()
            .unwrap_or_else(|| activity.summary_code.clone());
        if let Some(existing) = self
            .conversation_facts
            .activity
            .iter_mut()
            .find(|candidate| candidate.id == activity.id)
        {
            *existing = activity;
        } else {
            self.conversation_facts.activity.push(activity);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewFact<'a> {
    id: &'a str,
    bytes: u64,
    decoded_bytes: Option<u64>,
    eligible: bool,
}

fn select_preview_window<'a>(facts: impl IntoIterator<Item = PreviewFact<'a>>) -> Vec<&'a str> {
    let mut selected = Vec::with_capacity(MAX_INLINE_PREVIEWS);
    let mut bytes = 0_u64;
    let mut decoded_bytes = 0_u64;
    for fact in facts {
        if selected.len() == MAX_INLINE_PREVIEWS || !fact.eligible || selected.contains(&fact.id) {
            continue;
        }
        let Some(fact_decoded_bytes) = fact.decoded_bytes else {
            continue;
        };
        let Some(next_bytes) = bytes.checked_add(fact.bytes) else {
            continue;
        };
        let Some(next_decoded_bytes) = decoded_bytes.checked_add(fact_decoded_bytes) else {
            continue;
        };
        if next_bytes > MAX_INLINE_PREVIEW_BYTES
            || next_decoded_bytes > MAX_INLINE_PREVIEW_DECODED_BYTES
        {
            continue;
        }
        bytes = next_bytes;
        decoded_bytes = next_decoded_bytes;
        selected.push(fact.id);
    }
    selected
}

fn estimated_rgba_bytes(resource: &DesktopResource) -> Option<u64> {
    u64::from(resource.metadata.width?)
        .checked_mul(u64::from(resource.metadata.height?))?
        .checked_mul(4)
}

fn retain_admitted_previews<T>(
    previews: &mut HashMap<String, T>,
    admitted: &std::collections::HashSet<String>,
) {
    previews.retain(|artifact_id, _| admitted.contains(artifact_id));
}

fn project_message(message: &DesktopMessage) -> ProjectedMessage {
    let mut resources = Vec::new();
    let mut parts = Vec::new();
    for content in &message.content {
        if let DesktopContentValue::Resource(resource) = &content.value {
            resources.push(resource.as_ref().clone());
        } else {
            parts.push(project_content(content));
        }
    }
    let text = if parts.is_empty() && !resources.is_empty() {
        "Attached resource.".to_owned()
    } else {
        parts.join("\n\n")
    };
    ProjectedMessage {
        id: message.id.clone(),
        role: project_role(message.role),
        text,
        resources,
        lifecycle: MessageLifecycle::Complete,
    }
}

fn project_role(role: DesktopRole) -> ChatRole {
    match role {
        DesktopRole::System => ChatRole::System,
        DesktopRole::User => ChatRole::User,
        DesktopRole::Assistant => ChatRole::Assistant,
        DesktopRole::Tool => ChatRole::Tool,
    }
}

fn project_content(content: &DesktopContent) -> String {
    match &content.value {
        DesktopContentValue::Text(text) => text.clone(),
        DesktopContentValue::Markdown(text) => sanitize_markdown_for_desktop(text),
        DesktopContentValue::Code { language, code } => fenced(language.as_deref(), code),
        DesktopContentValue::Table { columns, rows } => markdown_table(columns, rows),
        DesktopContentValue::Diff(patch) => fenced(Some("diff"), patch),
        DesktopContentValue::Math { source, display } => {
            if *display {
                format!("$$\n{source}\n$$")
            } else {
                format!("${source}$")
            }
        }
        DesktopContentValue::Link { label, url } => format!("[{label}]({url})"),
        DesktopContentValue::Resource(_) => content.fallback_text.clone(),
        DesktopContentValue::Unsupported { .. } => content.fallback_text.clone(),
    }
}

fn project_attachment(resource: &DesktopResource, thumbnail: Option<&Arc<Image>>) -> Attachment {
    let mut attachment = Attachment::new(resource.artifact_id.clone(), resource.display_name())
        .kind(attachment_kind(&resource.kind))
        .size_bytes(resource.byte_len)
        .detail(resource_detail(resource));
    if let Some(thumbnail) = thumbnail {
        attachment = attachment.thumbnail(thumbnail.clone());
    }
    attachment
}

fn attachment_kind(kind: &DesktopResourceKind) -> AttachmentKind {
    match kind {
        DesktopResourceKind::StaticRaster
        | DesktopResourceKind::AnimatedRaster
        | DesktopResourceKind::Svg
        | DesktopResourceKind::Lottie => AttachmentKind::Image,
        DesktopResourceKind::Audio => AttachmentKind::Audio,
        DesktopResourceKind::Video => AttachmentKind::Video,
        DesktopResourceKind::Binary | DesktopResourceKind::Unknown(_) => AttachmentKind::Other,
    }
}

fn resource_detail(resource: &DesktopResource) -> SharedString {
    let mut details = vec![resource.media_type().to_owned()];
    if let (Some(width), Some(height)) = (resource.metadata.width, resource.metadata.height) {
        details.push(format!("{width}×{height}"));
    }
    if let Some(duration) = resource.metadata.duration_millis {
        details.push(format!("{}.{:03}s", duration / 1_000, duration % 1_000));
    }
    if let Some(label) = &resource.accessibility_label {
        details.push(label.clone());
    }
    details.join(" · ").into()
}

fn markdown_table(columns: &[String], rows: &[Vec<String>]) -> String {
    let mut output = String::new();
    output.push('|');
    for column in columns {
        output.push(' ');
        output.push_str(&escape_table_cell(column));
        output.push_str(" |");
    }
    output.push('\n');
    output.push('|');
    for _ in columns {
        output.push_str(" --- |");
    }
    for row in rows {
        output.push('\n');
        output.push('|');
        for cell in row {
            output.push(' ');
            output.push_str(&escape_table_cell(cell));
            output.push_str(" |");
        }
    }
    output
}

fn escape_table_cell(cell: &str) -> String {
    cell.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn fenced(language: Option<&str>, body: &str) -> String {
    let longest = body
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest.max(2).saturating_add(1));
    format!("{fence}{}\n{body}\n{fence}", language.unwrap_or_default())
}

#[derive(Debug)]
struct MarkdownReplacement {
    start: usize,
    end: usize,
    text: String,
}

/// Keep GPUI's Markdown renderer useful without granting model-authored
/// markup implicit URL, image-fetch, or HTML authority.
fn sanitize_markdown_for_desktop(source: &str) -> String {
    let Ok(tree) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
        return fenced(None, source);
    };
    let mut replacements = Vec::new();
    collect_markdown_replacements(&tree, &mut replacements);
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.start));
    let mut sanitized = source.to_owned();
    let mut prior_start = source.len();
    for replacement in replacements {
        if replacement.start > replacement.end
            || replacement.end > prior_start
            || replacement.end > sanitized.len()
            || !sanitized.is_char_boundary(replacement.start)
            || !sanitized.is_char_boundary(replacement.end)
        {
            return fenced(None, source);
        }
        sanitized.replace_range(replacement.start..replacement.end, &replacement.text);
        prior_start = replacement.start;
    }
    sanitized
}

fn collect_markdown_replacements(
    node: &markdown::mdast::Node,
    replacements: &mut Vec<MarkdownReplacement>,
) {
    use markdown::mdast::Node;
    let replacement = match node {
        Node::Image(image) => Some((
            image.position.as_ref(),
            format!(
                "[remote image omitted: {}]",
                escape_markdown_text(&image.alt)
            ),
        )),
        Node::ImageReference(image) => Some((
            image.position.as_ref(),
            format!("[image omitted: {}]", escape_markdown_text(&image.alt)),
        )),
        Node::Html(html) => Some((html.position.as_ref(), escape_markdown_text(&html.value))),
        Node::MdxjsEsm(value) => Some((
            value.position.as_ref(),
            "[executable markup omitted]".to_owned(),
        )),
        Node::MdxFlowExpression(value) => Some((
            value.position.as_ref(),
            "[executable markup omitted]".to_owned(),
        )),
        Node::MdxTextExpression(value) => Some((
            value.position.as_ref(),
            "[executable markup omitted]".to_owned(),
        )),
        Node::MdxJsxFlowElement(value) => Some((
            value.position.as_ref(),
            "[executable markup omitted]".to_owned(),
        )),
        Node::MdxJsxTextElement(value) => Some((
            value.position.as_ref(),
            "[executable markup omitted]".to_owned(),
        )),
        Node::Link(link) if !safe_http_url(&link.url) => Some((
            link.position.as_ref(),
            format!(
                "{} [unsafe link omitted]",
                escape_markdown_text(
                    &link
                        .children
                        .iter()
                        .map(ToString::to_string)
                        .collect::<String>()
                )
            ),
        )),
        Node::Definition(definition) if !safe_http_url(&definition.url) => Some((
            definition.position.as_ref(),
            "[unsafe link definition omitted]".to_owned(),
        )),
        _ => None,
    };
    if let Some((Some(position), text)) = replacement {
        replacements.push(MarkdownReplacement {
            start: position.start.offset,
            end: position.end.offset,
            text,
        });
        return;
    }
    if let Some(children) = node.children() {
        for child in children {
            collect_markdown_replacements(child, replacements);
        }
    }
}

fn safe_http_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

fn escape_markdown_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
        ) {
            escaped.push('\\');
        }
        if matches!(character, '\r' | '\n') {
            escaped.push(' ');
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn operation_label(state: DesktopOperationState) -> &'static str {
    match state {
        DesktopOperationState::Running => "Working",
        DesktopOperationState::Suspended => "Waiting for input",
        DesktopOperationState::Completed => "Completed",
        DesktopOperationState::Failed => "Failed",
        DesktopOperationState::Declined => "Declined",
        DesktopOperationState::Interrupted => "Interrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xana::desktop::{
        DesktopActivityDisclosure, DesktopActivityOwner, DesktopActivityState, DesktopError,
        DesktopErrorCode, DesktopFactFreshness, DesktopFactSource,
    };

    fn text_content(text: &str) -> DesktopContent {
        DesktopContent {
            tier: xana::desktop::DesktopContentTier::Text,
            outcome: "content.text".to_owned(),
            fallback_text: text.to_owned(),
            value: DesktopContentValue::Text(text.to_owned()),
            actions: Vec::new(),
        }
    }

    fn empty_snapshot() -> DesktopSnapshot {
        DesktopSnapshot {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 0,
            authority: xana::desktop::DesktopAuthority::Owner,
            attached_to_foreground_host: false,
            session_id: "session".to_owned(),
            connection: "ollama".to_owned(),
            execution_owner: "native".to_owned(),
            model: "fixture".to_owned(),
            reasoning_effort: None,
            notification_policy: xana::desktop::NotificationPolicy::default(),
            conversation: Vec::new(),
            conversation_truncated: false,
            active_operation: None,
            pending_approval_count: 0,
            pending_approvals: Vec::new(),
            activity_count: 0,
            artifact_count: 0,
            host_sequence: 0,
            hosted_workspace_count: 1,
            hosted_conversation_count: 1,
            hosted_conversations: Vec::new(),
            attached_conversation: Some("native/session".to_owned()),
            controllers: Vec::new(),
            host_lifecycle: "running".into(),
            global_notices: Vec::new(),
            navigation: xana::desktop::DesktopNavigationSnapshot::empty(
                xana::desktop::DesktopSidebarMode::Full,
            ),
            layout: xana::desktop::DesktopResolvedLayout {
                layout: xana::desktop::DesktopWorkbenchLayout::recovery(),
                source: xana::desktop::DesktopLayoutSource::Recovery,
                warning: None,
            },
            settings: xana::desktop::DesktopSettingsSnapshot {
                version: 1,
                revision: "fixture".to_owned(),
                warnings: Vec::new(),
                entries: Vec::new(),
                truncated: false,
            },
            conversation_facts: xana::desktop::DesktopConversationFacts {
                terminal_diagnostics: Vec::new(),
                profile: Some("default".to_owned()),
                activity: Vec::new(),
                execution: Vec::new(),
                usage: Vec::new(),
                completions: Vec::new(),
                capabilities: Vec::new(),
                prompt_ledger: xana::desktop::DesktopPromptLedger {
                    details: Vec::new(),
                    operation_id: None,
                    estimated_input_tokens: None,
                    input_budget_tokens: None,
                    context_window_tokens: None,
                    context_window_source: None,
                    attachment_count: None,
                    attachment_bytes: None,
                    omitted_source_count: None,
                    unavailable_reason: Some("not observed".to_owned()),
                },
            },
        }
    }

    #[test]
    fn stale_web_progress_does_not_replace_the_current_activity() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let current = DesktopOperationId::new();
        projection.active_operation = Some(current);
        for (sequence, operation_id, label) in [
            (1, current, "reading"),
            (2, DesktopOperationId::new(), "stale"),
        ] {
            projection.apply(DesktopObservation {
                conversation_start: 0,
                conversation_total: 0,
                version: xana::desktop::PROTOCOL_VERSION,
                sequence,
                event: DesktopEvent::WebProgress {
                    operation_id,
                    label: label.into(),
                },
            });
        }
        assert_eq!(projection.latest_activity, "reading");
    }

    #[test]
    fn markdown_keeps_safe_structure_and_removes_ambient_authority() {
        let source = concat!(
            "# Résumé\n\n",
            "[safe](https://example.test/docs) ",
            "[run](javascript:alert(1))\n\n",
            "![tracker](https://attacker.test/pixel.png)\n\n",
            "<img src=\"https://attacker.test/html.png\">\n",
        );

        let sanitized = sanitize_markdown_for_desktop(source);

        assert!(sanitized.contains("# Résumé"));
        assert!(sanitized.contains("[safe](https://example.test/docs)"));
        assert!(!sanitized.contains("javascript:"));
        assert!(!sanitized.contains("attacker.test"));
        assert!(sanitized.contains("unsafe link omitted"));
        assert!(sanitized.contains("remote image omitted"));
    }

    #[test]
    fn typed_code_tables_diffs_and_math_have_readable_markdown_fallbacks() {
        let code = DesktopContent {
            tier: xana::desktop::DesktopContentTier::Rich,
            outcome: "content.code.rich".to_owned(),
            fallback_text: "code".to_owned(),
            value: DesktopContentValue::Code {
                language: Some("rust".to_owned()),
                code: "let fence = ```;".to_owned(),
            },
            actions: Vec::new(),
        };
        let table = DesktopContent {
            tier: xana::desktop::DesktopContentTier::Rich,
            outcome: "content.table.rich".to_owned(),
            fallback_text: "table".to_owned(),
            value: DesktopContentValue::Table {
                columns: vec!["name".to_owned(), "state".to_owned()],
                rows: vec![vec!["Xa|na".to_owned(), "ready".to_owned()]],
            },
            actions: Vec::new(),
        };
        let math = DesktopContent {
            tier: xana::desktop::DesktopContentTier::Text,
            outcome: "content.math.source_fallback".to_owned(),
            fallback_text: "x^2".to_owned(),
            value: DesktopContentValue::Math {
                source: "x^2".to_owned(),
                display: true,
            },
            actions: Vec::new(),
        };

        assert!(project_content(&code).starts_with("````rust\n"));
        assert!(project_content(&table).contains("Xa\\|na"));
        assert_eq!(project_content(&math), "$$\nx^2\n$$");
    }

    #[test]
    fn sequence_gap_requires_an_atomic_snapshot() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();

        assert!(!projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::OperationState {
                operation_id,
                state: DesktopOperationState::Running,
            },
        }));
        assert!(!projection.is_running());
    }

    #[test]
    fn saved_history_messages_keep_content_without_live_mutation_actions() {
        let rows = ConversationProjection::saved_messages(&[DesktopMessage {
            id: "old-input".into(),
            role: DesktopRole::User,
            content: vec![text_content("retained owner input")],
        }]);
        assert_eq!(rows[0].content().text(), "retained owner input");
        assert_eq!(rows[0].message_actions(), MessageActions::none().copy(true));
    }

    #[test]
    fn progressive_delta_is_replaced_by_authoritative_final() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::AssistantDelta {
                operation_id,
                text: "Hel".to_owned(),
            },
        }));
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![text_content("Hello")],
                },
            },
        }));

        let messages = projection.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id().as_ref(), "final");
        assert_eq!(messages[0].content().text(), "Hello");
    }

    #[test]
    fn committed_user_input_converges_for_sender_observer_and_reattach() {
        let operation_id = DesktopOperationId::new();
        let committed = DesktopMessage {
            id: format!("conversation:{operation_id}:user"),
            role: DesktopRole::User,
            content: vec![text_content("same owner input")],
        };
        let event = |sequence| DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence,
            event: DesktopEvent::UserMessageCommitted {
                operation_id,
                message: committed.clone(),
            },
        };
        let mut sender = ConversationProjection::from_snapshot(&empty_snapshot());
        sender.append_user(operation_id, "same owner input".into());
        let mut observer = ConversationProjection::from_snapshot(&empty_snapshot());
        assert!(sender.apply(event(1)));
        assert!(observer.apply(event(1)));
        for projection in [&sender, &observer] {
            let rows = projection.messages();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id().as_ref(), committed.id);
            assert_eq!(rows[0].content().text(), "same owner input");
        }
        // Retried transport facts deduplicate by identity, not source text.
        assert!(sender.apply(event(2)));
        assert_eq!(sender.messages().len(), 1);
        let next_operation = DesktopOperationId::new();
        sender.append_user(next_operation, "same owner input".into());
        assert!(sender.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 3,
            event: DesktopEvent::UserMessageCommitted {
                operation_id: next_operation,
                message: DesktopMessage {
                    id: format!("conversation:{next_operation}:user"),
                    ..committed.clone()
                },
            },
        }));
        assert_eq!(sender.messages().len(), 2);
        let mut snapshot = empty_snapshot();
        snapshot.sequence = 1;
        snapshot.conversation.push(committed);
        observer.replace_snapshot(&snapshot);
        assert_eq!(observer.messages().len(), 1);
        assert_eq!(
            observer
                .messages
                .front()
                .expect("reattached snapshot retains the committed owner input")
                .role,
            ChatRole::User
        );
    }

    #[test]
    fn ten_thousand_deltas_converge_without_partial_retransmission() {
        let mut snapshot = empty_snapshot();
        snapshot.conversation = (0..512)
            .map(|index| DesktopMessage {
                id: format!("history-{index}"),
                role: if index % 2 == 0 {
                    DesktopRole::User
                } else {
                    DesktopRole::Assistant
                },
                content: vec![text_content(&format!("history {index}"))],
            })
            .collect();
        let mut projection = ConversationProjection::from_snapshot(&snapshot);
        let operation_id = DesktopOperationId::new();
        for sequence in 1..=10_000 {
            assert!(projection.apply(DesktopObservation {
                conversation_start: 0,
                conversation_total: 0,
                version: xana::desktop::PROTOCOL_VERSION,
                sequence,
                event: DesktopEvent::AssistantDelta {
                    operation_id,
                    text: "x".to_owned(),
                },
            }));
        }

        let messages = projection.messages();
        assert_eq!(messages.len(), 512);
        assert!(projection.history_omitted());
        assert_eq!(
            messages
                .last()
                .expect("stream projection should append one message")
                .content()
                .text()
                .len(),
            10_000
        );

        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 10_001,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "authoritative-final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![text_content(&"x".repeat(10_000))],
                },
            },
        }));
        let messages = projection.messages();
        assert_eq!(messages.len(), 512);
        assert_eq!(
            messages
                .last()
                .expect("authoritative final should remain projected")
                .id()
                .as_ref(),
            "authoritative-final"
        );
        assert_eq!(
            messages
                .last()
                .expect("authoritative final should remain projected")
                .content()
                .text()
                .len(),
            10_000
        );
    }

    #[test]
    #[ignore = "manual release-profile M4 projection measurement; GPUI paint requires reference-system observation"]
    fn m4_reference_desktop_projection_probe() {
        let mut snapshot = empty_snapshot();
        snapshot.conversation = (0..512)
            .map(|index| DesktopMessage {
                id: format!("history-{index}"),
                role: if index % 2 == 0 {
                    DesktopRole::User
                } else {
                    DesktopRole::Assistant
                },
                content: vec![text_content(&format!(
                    "history {index}: {}",
                    "x".repeat(128)
                ))],
            })
            .collect();
        let mut projection = ConversationProjection::from_snapshot(&snapshot);
        let operation_id = DesktopOperationId::new();
        const STREAMED_DELTAS: u64 = 10_000;
        const BATCH_SIZE: u64 = 64;
        let mut batches = Vec::with_capacity(STREAMED_DELTAS.div_ceil(BATCH_SIZE) as usize);
        let mut sequence = 0_u64;
        while sequence < STREAMED_DELTAS {
            let started = std::time::Instant::now();
            for _ in 0..(STREAMED_DELTAS - sequence).min(BATCH_SIZE) {
                sequence += 1;
                assert!(projection.apply(DesktopObservation {
                    conversation_start: 0,
                    conversation_total: 0,
                    version: xana::desktop::PROTOCOL_VERSION,
                    sequence,
                    event: DesktopEvent::AssistantDelta {
                        operation_id,
                        text: "x".to_owned(),
                    },
                }));
            }
            let projected = projection.messages();
            std::hint::black_box(projected);
            batches.push(started.elapsed());
        }
        batches.sort_unstable();
        let p95 = batches[(batches.len() * 95 / 100).min(batches.len() - 1)];
        let p99 = batches[(batches.len() * 99 / 100).min(batches.len() - 1)];
        let messages = projection.messages();

        println!(
            concat!(
                "m4_metric {{\"name\":\"desktop_projection\",",
                "\"source_messages\":512,\"streamed_deltas\":{},",
                "\"retained_messages\":{},\"projected_text_bytes\":{},",
                "\"batch_size\":64,\"p95_us\":{},\"p99_us\":{}}}"
            ),
            sequence,
            messages.len(),
            messages
                .iter()
                .map(|message| message.content().text().len())
                .sum::<usize>(),
            p95.as_micros(),
            p99.as_micros(),
        );
    }

    #[test]
    #[ignore = "release-profile M6 client-window measurement; not a durable-store or paint benchmark"]
    fn m6_long_history_projection_probe() {
        for count in [10_000, 100_000] {
            for run in 0..5 {
                let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
                let started = std::time::Instant::now();
                for index in 0..count {
                    let text = if index % 251 == 0 {
                        "🦀中".repeat(4096)
                    } else {
                        format!(
                            "message {index}: {}",
                            "mixed 🦀 text ".repeat(4 + index % 32)
                        )
                    };
                    projection.append_user(DesktopOperationId::new(), text);
                }
                let ingest_ms = started.elapsed().as_millis();
                let mut samples = Vec::new();
                for _ in 0..20 {
                    let started = std::time::Instant::now();
                    std::hint::black_box(projection.messages());
                    samples.push(started.elapsed().as_micros());
                }
                samples.sort_unstable();
                let mut changed_samples = Vec::new();
                let operation_id = DesktopOperationId::new();
                for sequence in 1..=64 {
                    let started = std::time::Instant::now();
                    projection.apply(DesktopObservation {
                        conversation_start: 0,
                        conversation_total: 0,
                        version: xana::desktop::PROTOCOL_VERSION,
                        sequence,
                        event: DesktopEvent::AssistantDelta {
                            operation_id,
                            text: "stream 🦀 ".repeat(8),
                        },
                    });
                    std::hint::black_box(projection.messages());
                    changed_samples.push(started.elapsed().as_micros());
                }
                changed_samples.sort_unstable();
                println!(
                    "m6_changed_snapshot count={count} run={run} p95_us={}",
                    changed_samples[60]
                );
                println!(
                    "m6_projection count={count} run={run} retained={} text_bytes={} ingest_ms={ingest_ms} snapshot_p95_us={}",
                    projection.messages.len(),
                    projection
                        .messages
                        .iter()
                        .map(|message| message.text.len())
                        .sum::<usize>(),
                    samples[18]
                );
            }
        }
    }

    #[test]
    fn long_history_has_a_bounded_window_including_bytes_and_operation_indexes() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        for index in 0..1200 {
            projection.append_user(
                DesktopOperationId::new(),
                format!("{index}:{}", "🦀".repeat(4096)),
            );
        }
        let messages = projection.messages();
        assert!(messages.len() <= 512);
        assert!(
            messages
                .iter()
                .map(|message| message.content().text().len())
                .sum::<usize>()
                <= 2 * 1024 * 1024
        );
        assert!(
            messages
                .last()
                .expect("bounded window retains the latest message")
                .content()
                .text()
                .starts_with("1199:")
        );
    }

    #[test]
    fn unchanged_messages_reuse_the_snapshot_but_stream_updates_do_not() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        projection.append_user(DesktopOperationId::new(), "question".into());
        let first = projection.messages();
        projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::Activity {
                label: "Working".into(),
            },
        });
        assert!(Arc::ptr_eq(&first, &projection.messages()));
        projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::AssistantDelta {
                operation_id: DesktopOperationId::new(),
                text: "answer".into(),
            },
        });
        let changed = projection.messages();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(first.len(), 1);
        assert_eq!(
            changed.last().expect("streamed message").content().text(),
            "answer"
        );
    }

    #[test]
    fn live_tool_evidence_does_not_replace_or_finalize_the_assistant_stream() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();
        projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::AssistantDelta {
                operation_id,
                text: "partial".to_owned(),
            },
        });
        projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::ToolResult {
                message: DesktopMessage {
                    id: "tool-evidence".to_owned(),
                    role: DesktopRole::Tool,
                    content: vec![text_content("bounded result")],
                },
                activity: None,
            },
        });
        assert_eq!(projection.messages().len(), 2);
        projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 3,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "assistant-final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![text_content("final answer")],
                },
            },
        });
        let messages = projection.messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content().text(), "final answer");
        assert_eq!(messages[1].content().text(), "bounded result");
    }

    #[test]
    fn terminal_failure_marks_the_authoritative_message_retryable() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let operation_id = DesktopOperationId::new();
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::MessageFinal {
                operation_id,
                message: DesktopMessage {
                    id: "managed-final".to_owned(),
                    role: DesktopRole::Assistant,
                    content: vec![text_content("partial result")],
                },
            },
        }));
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::OperationState {
                operation_id,
                state: DesktopOperationState::Failed,
            },
        }));

        let messages = projection.messages();
        assert!(matches!(
            messages[0].content().state(),
            gpui_ai::prelude::ProgressState::Failed(reason) if reason.as_ref() == "Operation failed"
        ));
        assert_eq!(
            projection.operation_for_message("managed-final"),
            Some(operation_id)
        );
    }

    #[test]
    fn managed_selection_receipt_updates_only_later_turn_controls() {
        let mut snapshot = empty_snapshot();
        snapshot.execution_owner = "managed_codex".to_owned();
        let mut projection = ConversationProjection::from_snapshot(&snapshot);
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::ExecutionSelectionChanged {
                model: "gpt-next".to_owned(),
                reasoning_effort: Some("high".to_owned()),
                receipt: "thread retained".to_owned(),
            },
        }));

        assert_eq!(projection.model(), "gpt-next");
        assert_eq!(projection.reasoning_effort(), Some("high"));
        assert_eq!(projection.latest_activity(), "thread retained");
    }

    #[test]
    fn fatal_update_remains_visible() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let error = DesktopError {
            code: DesktopErrorCode::RuntimeCrashed,
            message: "runtime stopped".to_owned(),
        };
        projection.fail(error.message.clone());

        assert_eq!(projection.failure(), Some("runtime stopped"));
    }

    #[test]
    fn activity_updates_replace_by_stable_identity() {
        let mut projection = ConversationProjection::from_snapshot(&empty_snapshot());
        let activity = |state, detail: &str| DesktopActivityItem {
            id: "tool-1".to_owned(),
            parent_id: None,
            operation_id: Some("run-1".to_owned()),
            owner: DesktopActivityOwner::XanaRoot,
            state,
            summary_code: "tool.state".to_owned(),
            summary_parameters: Vec::new(),
            disclosed_text: Some(detail.to_owned()),
            disclosure: DesktopActivityDisclosure::Summary,
            source: DesktopFactSource::Runtime,
            freshness: DesktopFactFreshness {
                observed_at_unix_millis: 1,
                max_age_millis: None,
            },
            started_at_unix_millis: Some(1),
            finished_at_unix_millis: None,
        };

        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 1,
            event: DesktopEvent::ActivityUpserted(activity(
                DesktopActivityState::Working,
                "Running",
            )),
        }));
        assert!(projection.apply(DesktopObservation {
            conversation_start: 0,
            conversation_total: 0,
            version: xana::desktop::PROTOCOL_VERSION,
            sequence: 2,
            event: DesktopEvent::ActivityUpserted(activity(
                DesktopActivityState::Completed,
                "Done",
            )),
        }));

        assert_eq!(projection.conversation_facts().activity.len(), 1);
        assert_eq!(
            projection.conversation_facts().activity[0].state,
            DesktopActivityState::Completed
        );
        assert_eq!(projection.latest_activity(), "Done");
    }

    #[test]
    fn preview_window_is_newest_first_unique_and_bounded_by_count_and_bytes() {
        let ids = (0..20)
            .map(|index| format!("resource-{index}"))
            .collect::<Vec<_>>();
        let facts = ids.iter().enumerate().rev().map(|(index, id)| PreviewFact {
            id,
            bytes: if index == 18 {
                MAX_INLINE_PREVIEW_BYTES + 1
            } else {
                1024
            },
            decoded_bytes: Some(1024 * 1024),
            eligible: index != 17,
        });

        let selected = select_preview_window(facts);

        assert_eq!(selected.len(), MAX_INLINE_PREVIEWS);
        assert_eq!(selected[0], "resource-19");
        assert!(!selected.contains(&"resource-18"));
        assert!(!selected.contains(&"resource-17"));
        assert_eq!(selected.last().copied(), Some("resource-10"));
    }

    #[test]
    fn preview_window_rejects_overflow_and_duplicate_accounting() {
        let selected = select_preview_window([
            PreviewFact {
                id: "same",
                bytes: MAX_INLINE_PREVIEW_BYTES,
                decoded_bytes: Some(MAX_INLINE_PREVIEW_DECODED_BYTES),
                eligible: true,
            },
            PreviewFact {
                id: "same",
                bytes: MAX_INLINE_PREVIEW_BYTES,
                decoded_bytes: Some(MAX_INLINE_PREVIEW_DECODED_BYTES),
                eligible: true,
            },
            PreviewFact {
                id: "overflow",
                bytes: u64::MAX,
                decoded_bytes: Some(u64::MAX),
                eligible: true,
            },
        ]);

        assert_eq!(selected, ["same"]);
    }

    #[test]
    fn preview_window_bounds_estimated_decoded_bytes_and_requires_dimensions() {
        let selected = select_preview_window([
            PreviewFact {
                id: "4k",
                bytes: 1024,
                decoded_bytes: Some(3840 * 2160 * 4),
                eligible: true,
            },
            PreviewFact {
                id: "would-exceed-budget",
                bytes: 1024,
                decoded_bytes: Some(1024 * 1024),
                eligible: true,
            },
            PreviewFact {
                id: "unknown-dimensions",
                bytes: 1024,
                decoded_bytes: None,
                eligible: true,
            },
            PreviewFact {
                id: "small",
                bytes: 1024,
                decoded_bytes: Some(64 * 64 * 4),
                eligible: true,
            },
        ]);

        assert_eq!(selected, ["4k", "small"]);
    }

    #[test]
    fn cache_eviction_keeps_only_the_current_preview_window() {
        let mut cache =
            HashMap::from([("retained".to_owned(), 1_u8), ("evicted".to_owned(), 2_u8)]);
        let admitted = std::collections::HashSet::from(["retained".to_owned(), "new".to_owned()]);

        retain_admitted_previews(&mut cache, &admitted);

        assert_eq!(cache, HashMap::from([("retained".to_owned(), 1_u8)]));
    }
}
