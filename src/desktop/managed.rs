//! Managed-runtime adapter for the repository-private Desktop boundary.
//!
//! Codex owns its thread, history, and inner loop. This adapter owns only the
//! bounded, current-process Desktop projection and application coordination.

use super::*;
use crate::{
    frontend::{
        ClientEvent, ClientObservation, ClientSnapshot, ClientSnapshotSeed,
        semantic::HostLocationV1,
    },
    managed::codex::{ApprovalDecision, ApprovalRequest, CodexAppServer, ManagedTokenUsage},
    managed_execution::{ManagedChatConfig, ManagedTuiDriver, ManagedTuiEvent},
    message::ContentBlock,
    model_catalog::{ModelDescriptor, ModelManager, ModelSelection},
};
use std::{collections::HashMap, time::SystemTime};
use tokio::sync::oneshot;

const MAX_MANAGED_MESSAGES: usize = 512;
const MAX_MANAGED_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANAGED_ACTIVITY: usize = 128;
const MAX_MANAGED_RECEIPTS: usize = 128;

enum PendingApproval {
    Vendor {
        request: ApprovalRequest,
        reply: oneshot::Sender<ApprovalDecision>,
    },
    Memory {
        request: Box<crate::permission::PermissionRequest>,
        reply: oneshot::Sender<crate::permission::ControllerDecision>,
    },
}

#[cfg(test)]
mod memory_approval_tests {
    use super::*;

    #[tokio::test]
    async fn exact_memory_proposal_is_visible_and_uses_xana_once_only_decisions() {
        for allow_once in [false, true] {
            let operation = OperationId::new();
            let request = crate::permission::PermissionRequest {
                operation_id: operation,
                invocation_id: crate::identity::ToolInvocationId::new(),
                tool_name: "memory_update".into(),
                effect_class: crate::tool::EffectClass::Write,
                final_arguments: serde_json::json!({"action":"remember","statement":"I prefer red","quote":"private original owner wording","source_id":"private-source"}),
                scope: crate::permission::PermissionScope::PersonalMemory {
                    scope: "user".into(),
                    review: true,
                },
                outbound_review: None,
            };
            let (reply, receiver) = oneshot::channel();
            let pending = PendingApproval::Memory {
                request: Box::new(request),
                reply,
            };
            let projection = pending.projection(operation, 1);
            assert_eq!(projection.tool, "Xana memory_update");
            assert!(projection.scope.contains("user"));
            assert!(projection.scope.contains("I prefer red"));
            assert!(!projection.scope.contains("private original"));
            assert!(!projection.scope.contains("private-source"));
            pending.resolve(allow_once).unwrap();
            assert_eq!(
                receiver.await.unwrap(),
                if allow_once {
                    crate::permission::ControllerDecision::AllowOnce
                } else {
                    crate::permission::ControllerDecision::Deny
                }
            );
        }
    }
}

impl PendingApproval {
    fn projection(&self, operation: OperationId, id: u64) -> DesktopPendingApproval {
        match self {
            Self::Vendor { request, .. } => project_managed_approval(operation, id, request),
            Self::Memory { request, .. } => DesktopPendingApproval {
                public_web: false,
                id: DesktopPermissionId::managed(operation, id),
                tool: bounded_text(format!("Xana {}", request.tool_name), MAX_PUBLIC_TEXT_BYTES),
                effect: format!("{:?}", request.effect_class).to_ascii_lowercase(),
                scope: bounded_text(
                    super::permission_review_label(request),
                    MAX_PUBLIC_TEXT_BYTES,
                ),
            },
        }
    }

    fn resolve(self, allow_once: bool) -> Result<(), DesktopError> {
        let delivered = match self {
            Self::Vendor { request, reply } => {
                let decision = if allow_once {
                    ApprovalDecision::AcceptOnce
                } else if request.available_decisions.contains("decline") {
                    ApprovalDecision::Decline
                } else {
                    ApprovalDecision::Cancel
                };
                reply.send(decision).is_ok()
            }
            Self::Memory { reply, .. } => reply
                .send(if allow_once {
                    crate::permission::ControllerDecision::AllowOnce
                } else {
                    crate::permission::ControllerDecision::Deny
                })
                .is_ok(),
        };
        delivered.then_some(()).ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "managed runtime stopped before receiving the approval decision",
            )
        })
    }
}

struct ManagedDesktopState {
    snapshot: ClientSnapshot,
    facts: DesktopConversationFacts,
    conversation: ConversationRef,
    thread_id: String,
    desktop_sequence: u64,
    runtime_sequence: u64,
    active_operation: Option<OperationId>,
    assistant: String,
    activity_text: HashMap<String, String>,
    pending_approvals: HashMap<u64, PendingApproval>,
    next_approval_id: u64,
    usage_sequence: u64,
    last_usage: Option<ManagedTokenUsage>,
    turn_status: Option<(String, Option<String>)>,
}

impl ManagedDesktopState {
    fn new(
        config: &ManagedChatConfig,
        conversation: ConversationRef,
        thread_id: String,
        models: &[ModelDescriptor],
    ) -> Self {
        let conversation_id = conversation
            .conversation_id()
            .expect("hosted managed Conversation has a durable identity");
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: crate::identity::SessionId::new(),
                connection: config.connection.clone(),
                execution_owner: "managed_codex".to_owned(),
                model: config.model.clone(),
                reasoning_effort: config.selection.reasoning_effort.clone(),
                host_location: HostLocationV1::Embedded,
                approval_policy: "on-request".to_owned(),
                children: Vec::new(),
                resource_policy: config.resource_policy.clone(),
            },
            Vec::new(),
        );
        snapshot.semantic.conversation_id = Some(conversation_id);
        let now = observed_at_unix_millis();
        let selected = models.iter().find(|model| model.id == config.model);
        let mut capabilities = vec![DesktopRunCapability {
            id: "text_input".to_owned(),
            availability: DesktopAvailability::Available,
            selected: true,
            authorized: true,
            source: DesktopFactSource::ManagedRuntime,
            freshness: freshness(now),
        }];
        capabilities.push(DesktopRunCapability {
            id: "image_input".to_owned(),
            availability: if selected.is_some_and(|model| model.input_modalities.contains("image"))
            {
                DesktopAvailability::Available
            } else {
                DesktopAvailability::Unsupported
            },
            selected: false,
            authorized: true,
            source: DesktopFactSource::ManagedRuntime,
            freshness: freshness(now),
        });
        capabilities.push(DesktopRunCapability {
            id: "reasoning_summary".to_owned(),
            availability: if selected.is_some_and(|model| model.reasoning == Some(true)) {
                DesktopAvailability::Available
            } else {
                DesktopAvailability::Unavailable {
                    code: "model_did_not_advertise_reasoning".to_owned(),
                }
            },
            selected: config.selection.reasoning_summary.is_some(),
            authorized: true,
            source: DesktopFactSource::ManagedRuntime,
            freshness: freshness(now),
        });
        Self {
            snapshot,
            facts: DesktopConversationFacts {
                profile: Some(config.profile_name.clone()),
                activity: Vec::new(),
                execution: Vec::new(),
                usage: Vec::new(),
                completions: Vec::new(),
                terminal_diagnostics: Vec::new(),
                capabilities,
                prompt_ledger: DesktopPromptLedger {
                    details: Vec::new(),
                    operation_id: None,
                    estimated_input_tokens: None,
                    input_budget_tokens: None,
                    context_window_tokens: None,
                    context_window_source: None,
                    attachment_count: None,
                    attachment_bytes: None,
                    omitted_source_count: None,
                    unavailable_reason: Some(
                        "Codex owns prompt assembly; Xana receives no native prompt ledger"
                            .to_owned(),
                    ),
                },
            },
            conversation,
            thread_id,
            desktop_sequence: 0,
            runtime_sequence: 0,
            active_operation: None,
            assistant: String::new(),
            activity_text: HashMap::new(),
            pending_approvals: HashMap::new(),
            next_approval_id: 1,
            usage_sequence: 0,
            last_usage: None,
            turn_status: None,
        }
    }

    fn project_snapshot(
        &self,
        host: &crate::execution_host::ExecutionHostSnapshot,
        policy: &NotificationPolicy,
        navigation: &DesktopNavigationSnapshot,
        layout: &DesktopResolvedLayout,
        settings: &DesktopSettingsSnapshot,
    ) -> DesktopSnapshot {
        let mut snapshot =
            super::project_snapshot(&self.snapshot, host, policy, navigation, layout, settings);
        snapshot.sequence = self.desktop_sequence;
        snapshot.session_id.clone_from(&self.thread_id);
        snapshot.active_operation = self.active_operation.map(DesktopOperationId);
        snapshot.pending_approval_count = self.pending_approvals.len();
        snapshot.pending_approvals = self
            .pending_approvals
            .iter()
            .filter_map(|(request_id, pending)| {
                let operation_id = self.active_operation?;
                Some(pending.projection(operation_id, *request_id))
            })
            .collect();
        snapshot
            .pending_approvals
            .sort_by_key(|approval| approval.id.to_string());
        snapshot.activity_count = self.facts.activity.len();
        snapshot.conversation_facts = self.facts.clone();
        snapshot
    }

    fn begin_run(&mut self, operation_id: OperationId, input: String, images: &[ImageAttachment]) {
        self.active_operation = Some(operation_id);
        self.snapshot.active_operation = Some(operation_id);
        self.assistant.clear();
        self.turn_status = None;
        let mut content = vec![ContentBlock::Text(input)];
        content.extend(
            images
                .iter()
                .map(|image| ContentBlock::Image(image.image.clone())),
        );
        self.snapshot.conversation.push(Message {
            role: Role::User,
            content,
        });
        self.bound_transcript();
        let execution = DesktopExecutionFact {
            operation_id: operation_id.to_string(),
            owner: "managed".to_owned(),
            host_location: "embedded".to_owned(),
            workspace_authority: "workspace-write sandbox (managed runtime)".to_owned(),
            tool_authority: vec!["Codex-managed tool policy".to_owned()],
            connection: Some(self.snapshot.connection.clone()),
            model: Some(self.snapshot.model.clone()),
            capability_grants: self
                .facts
                .capabilities
                .iter()
                .filter(|capability| {
                    matches!(capability.availability, DesktopAvailability::Available)
                })
                .map(|capability| capability.id.clone())
                .collect(),
            egress_policy: Some("Codex-managed provider and tool egress".to_owned()),
            controller: Some("Desktop foreground controller".to_owned()),
            approval_policy: self.snapshot.approval_policy.clone(),
            source: DesktopFactSource::ManagedRuntime,
            freshness: freshness(observed_at_unix_millis()),
        };
        upsert_bounded(
            &mut self.facts.execution,
            execution,
            MAX_MANAGED_RECEIPTS,
            |candidate| candidate.operation_id == operation_id.to_string(),
        );
    }

    fn apply_selection(&mut self, selection: &ModelSelection, models: &[ModelDescriptor]) {
        self.snapshot.model.clone_from(&selection.model);
        self.snapshot
            .reasoning_effort
            .clone_from(&selection.reasoning_effort);
        let descriptor = models.iter().find(|model| model.id == selection.model);
        if let Some(image) = self
            .facts
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == "image_input")
        {
            image.availability =
                if descriptor.is_some_and(|model| model.input_modalities.contains("image")) {
                    DesktopAvailability::Available
                } else {
                    DesktopAvailability::Unsupported
                };
            image.freshness = freshness(observed_at_unix_millis());
        }
        if let Some(reasoning) = self
            .facts
            .capabilities
            .iter_mut()
            .find(|capability| capability.id == "reasoning_summary")
        {
            reasoning.availability =
                if descriptor.is_some_and(|model| model.reasoning == Some(true)) {
                    DesktopAvailability::Available
                } else {
                    DesktopAvailability::Unavailable {
                        code: "model_did_not_advertise_reasoning".to_owned(),
                    }
                };
            reasoning.freshness = freshness(observed_at_unix_millis());
        }
    }

    fn finish_run(&mut self, operation_id: OperationId, error: Option<String>) -> DesktopMessage {
        self.pending_approvals.clear();
        let (reported_status, reported_error) = self.turn_status.take().unwrap_or_else(|| {
            (
                if error.is_some() {
                    "failed"
                } else {
                    "completed"
                }
                .to_owned(),
                None,
            )
        });
        let error = error.or(reported_error);
        let status = if error.is_some() || reported_status != "completed" {
            "failed"
        } else {
            "completed"
        };
        let text = if self.assistant.is_empty() {
            error
                .as_deref()
                .map_or_else(String::new, |error| format!("Managed turn failed: {error}"))
        } else {
            std::mem::take(&mut self.assistant)
        };
        let message = Message::text(Role::Assistant, text.clone());
        self.snapshot.conversation.push(message);
        self.bound_transcript();
        self.active_operation = None;
        self.snapshot.active_operation = None;
        let execution = self
            .facts
            .execution
            .iter()
            .rev()
            .find(|fact| fact.operation_id == operation_id.to_string())
            .cloned()
            .unwrap_or_else(|| fallback_execution(&self.snapshot, operation_id));
        let now = observed_at_unix_millis();
        let receipt = DesktopCompletionReceipt {
            id: format!("managed:{}:{operation_id}", self.thread_id),
            operation_id: operation_id.to_string(),
            status: status.to_owned(),
            completion_evidence: None,
            execution,
            artifact_ids: Vec::new(),
            checks: vec![DesktopCompletionCheck {
                code: "managed_runtime_reported_terminal_state".to_owned(),
                passed: error.is_none(),
            }],
            input_tokens: self.last_usage.map(|usage| usage.input_tokens),
            output_tokens: self.last_usage.map(|usage| usage.output_tokens),
            request_count: None,
            warnings: error
                .into_iter()
                .map(|error| bounded_text(error, MAX_PUBLIC_TEXT_BYTES))
                .collect(),
            source: DesktopFactSource::ManagedRuntime,
            authority: DesktopFactAuthority::ProviderReported,
            freshness: freshness(now),
        };
        upsert_bounded(
            &mut self.facts.completions,
            receipt,
            MAX_MANAGED_RECEIPTS,
            |candidate| candidate.operation_id == operation_id.to_string(),
        );
        content::project_message(
            format!("managed-{}-{operation_id}", self.thread_id),
            &Message::text(Role::Assistant, text),
            &self.snapshot.semantic.attachment_policy.configured,
        )
    }

    fn observation(&mut self, event: DesktopEvent) -> DesktopObservation {
        self.desktop_sequence = self.desktop_sequence.saturating_add(1);
        self.snapshot.sequence = self.desktop_sequence;
        DesktopObservation {
            version: FRONTEND_PROTOCOL_VERSION,
            sequence: self.desktop_sequence,
            conversation_start: self.snapshot.conversation_start,
            conversation_total: self.snapshot.conversation_total,
            event,
        }
    }

    fn record_managed(
        &mut self,
        host: &ExecutionHost,
        event: &crate::frontend::ManagedClientEvent,
    ) -> Result<(), DesktopError> {
        self.runtime_sequence = self.runtime_sequence.saturating_add(1);
        host.record_runtime_observation(
            &self.conversation,
            &ClientObservation {
                version: FRONTEND_PROTOCOL_VERSION,
                sequence: self.runtime_sequence,
                event: ClientEvent::Managed(Box::new(event.clone())),
            },
        )
        .map_err(host_error)
    }

    fn activity(
        &mut self,
        suffix: &str,
        state: DesktopActivityState,
        code: &str,
        delta: Option<&str>,
        disclosure: DesktopActivityDisclosure,
    ) -> DesktopActivityItem {
        let operation = self.active_operation;
        let key = operation.map_or_else(
            || format!("managed:thread:{suffix}"),
            |operation| format!("managed:{operation}:{suffix}"),
        );
        let detail = delta.map(|delta| {
            let retained = self.activity_text.entry(key.clone()).or_default();
            if state == DesktopActivityState::Working
                && matches!(
                    code,
                    "managed.reasoning_summary"
                        | "managed.reasoning_detail"
                        | "managed.plan"
                        | "managed.command_output"
                )
            {
                retained.push_str(delta);
            } else {
                delta.clone_into(retained);
            }
            *retained = bounded_text(std::mem::take(retained), MAX_PUBLIC_TEXT_BYTES);
            retained.clone()
        });
        let now = observed_at_unix_millis();
        let item = DesktopActivityItem {
            id: key,
            parent_id: None,
            operation_id: operation.map(|operation| operation.to_string()),
            owner: DesktopActivityOwner::Managed {
                runtime: self.snapshot.connection.clone(),
            },
            state,
            summary_code: code.to_owned(),
            summary_parameters: Vec::new(),
            disclosed_text: detail,
            disclosure,
            source: DesktopFactSource::ManagedRuntime,
            freshness: freshness(now),
            started_at_unix_millis: Some(now),
            finished_at_unix_millis: matches!(
                state,
                DesktopActivityState::Completed
                    | DesktopActivityState::Failed
                    | DesktopActivityState::Cancelled
            )
            .then_some(now),
        };
        upsert_bounded(
            &mut self.facts.activity,
            item.clone(),
            MAX_MANAGED_ACTIVITY,
            |candidate| candidate.id == item.id,
        );
        item
    }

    fn observe_usage(&mut self, usage: ManagedTokenUsage) {
        self.usage_sequence = self.usage_sequence.saturating_add(1);
        self.last_usage = Some(usage);
        let now = observed_at_unix_millis();
        let fact = DesktopUsageFact {
            id: format!("managed:{}:usage", self.thread_id),
            scope: format!("conversation:{}", self.conversation),
            period: self.thread_id.clone(),
            accounting: format!("cumulative_snapshot:{}", self.usage_sequence),
            input_tokens: Some(usage.input_tokens),
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: Some(usage.output_tokens),
            reasoning_tokens: usage.reasoning_tokens,
            request_count: None,
            context_input_tokens: usage.context_input_tokens,
            context_capacity_tokens: usage.context_window_tokens,
            cost_microunits: None,
            availability: DesktopAvailability::Available,
            source: DesktopFactSource::ManagedRuntime,
            authority: DesktopFactAuthority::ProviderReported,
            freshness: freshness(now),
        };
        upsert_bounded(&mut self.facts.usage, fact, 16, |candidate| {
            candidate.period == self.thread_id
        });
    }

    fn bound_transcript(&mut self) {
        if self.snapshot.conversation.len() > MAX_MANAGED_MESSAGES {
            let excess = self.snapshot.conversation.len() - MAX_MANAGED_MESSAGES;
            self.snapshot.conversation.drain(..excess);
            self.snapshot.conversation_truncated = true;
        }
        while !self.snapshot.conversation.is_empty()
            && serde_json::to_vec(&self.snapshot.conversation)
                .map_or(true, |encoded| encoded.len() > MAX_MANAGED_TRANSCRIPT_BYTES)
        {
            self.snapshot.conversation.remove(0);
            self.snapshot.conversation_truncated = true;
        }
        self.snapshot.artifact_count = self
            .snapshot
            .conversation
            .iter()
            .flat_map(|message| &message.content)
            .filter(|content| matches!(content, ContentBlock::Image(_)))
            .count();
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_managed(
    server: CodexAppServer,
    models: ModelManager,
    config: ManagedChatConfig,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
    bridge: Bridge,
    paths: &XanaPaths,
    notification_policy: NotificationPolicy,
) -> anyhow::Result<ChatExit> {
    let workspace = workspace_host.workspace().to_owned();
    let attachments = DesktopAttachmentService {
        workspace,
        store: config.artifact_store.clone(),
        ingestor: ImageIngestor::new(config.artifact_store.clone(), ImageLimits::default()),
        resource_policy: config.resource_policy.clone(),
        owner: config.owner,
    };
    let (driver, conversation) =
        ManagedTuiDriver::start_hosted(server, models, config.clone(), conversation).await?;
    let thread_id = match &conversation {
        ConversationRef::Managed { thread_id, .. } => thread_id.clone(),
        _ => anyhow::bail!("managed Desktop startup did not produce a durable Codex thread"),
    };
    let execution_host = ExecutionHost::new();
    execution_host.register(
        workspace_host,
        ConversationRegistration::new(
            conversation.clone(),
            config.connection.clone(),
            config.model.clone(),
            Some(config.profile_name.clone()),
            "on-request",
        ),
    )?;
    execution_host.attach(&conversation)?;
    let navigation_store = navigation::DesktopNavigationStore::open(paths, &config.workspace)?;
    let navigation = navigation_store.snapshot(Some(&conversation.to_string()))?;
    let _ = navigation_store.record_recent(Some(&conversation.to_string()));
    let layout_store = layout::DesktopLayoutStore::open(paths);
    let layout = layout_store.resolve(&conversation.to_string());
    let settings =
        settings::DesktopSettingsState::open(crate::settings::SettingsManager::new(paths))?;
    let state = ManagedDesktopState::new(&config, conversation.clone(), thread_id, &driver.models);
    bridge
        .serve_managed(
            driver,
            execution_host,
            state,
            notification_policy,
            DesktopFrontendState {
                vision: None,
                navigation,
                navigation_store,
                layout,
                layout_store,
                settings,
                attachments,
            },
        )
        .await
        .map_err(anyhow::Error::new)
}

impl Bridge {
    async fn serve_managed(
        mut self,
        mut driver: ManagedTuiDriver,
        execution_host: ExecutionHost,
        mut state: ManagedDesktopState,
        notification_policy: NotificationPolicy,
        frontend: DesktopFrontendState,
    ) -> Result<ChatExit, DesktopError> {
        self.notification_policy = notification_policy;
        let DesktopFrontendState {
            vision: _,
            mut navigation,
            navigation_store,
            mut layout,
            layout_store,
            mut settings,
            attachments,
        } = frontend;
        let mut commands = self.commands.lock().await;
        let controller = DesktopController {
            conversation: state.conversation.clone(),
            client_id: ControllerClientId::new(),
        };
        let _controller_grant = execution_host
            .acquire_controller(
                &controller.conversation,
                controller.client_id,
                None,
                Instant::now(),
            )
            .map_err(host_error)?;
        let host_snapshot = execution_host.snapshot().map_err(host_error)?;
        let mut host_cursor = host_snapshot.sequence;
        let initial = state.project_snapshot(
            &host_snapshot,
            &self.notification_policy,
            &navigation,
            &layout,
            settings.snapshot(),
        );
        if !self.startup.ready(initial.clone()) {
            self.publish_critical(DesktopUpdate::Snapshot(Box::new(initial)))
                .await?;
        }

        let mut active_run: Option<HostedRun> = None;
        let mut exit = ChatExit::Quit;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    if command.version != FRONTEND_PROTOCOL_VERSION {
                        self.publish_command_result(
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
                    if let Some(requested_exit) = self
                        .handle_managed_command(
                            command,
                            &driver,
                            &execution_host,
                            &controller,
                            &mut state,
                            &mut active_run,
                            &mut navigation,
                            &navigation_store,
                            &mut layout,
                            &layout_store,
                            &mut settings,
                            &attachments,
                        )
                        .await?
                    {
                        exit = requested_exit;
                        break;
                    }
                    self.publish_host_changes(
                        &execution_host,
                        &state.snapshot,
                        &mut host_cursor,
                        &navigation,
                        &layout,
                        settings.snapshot(),
                    ).await?;
                }
                event = driver.next_event() => {
                    let Some(event) = event else {
                        if active_run.is_some() {
                            return Err(DesktopError::new(
                                DesktopErrorCode::RuntimeCrashed,
                                "managed Codex runtime stopped during an active Run",
                            ));
                        }
                        break;
                    };
                    self.handle_managed_event(
                        event,
                        &execution_host,
                        &controller,
                        &mut state,
                        &mut active_run,
                    ).await?;
                    self.publish_host_changes(
                        &execution_host,
                        &state.snapshot,
                        &mut host_cursor,
                        &navigation,
                        &layout,
                        settings.snapshot(),
                    ).await?;
                }
            }
        }

        execution_host.request_shutdown().map_err(host_error)?;
        drop(active_run);
        let cleanup = match driver.shutdown().await {
            Ok(()) => crate::host_lifecycle::OwnedExecutionCleanup::Clean,
            Err(error) => {
                self.publish_critical(DesktopUpdate::Observation(state.observation(
                    DesktopEvent::Error(DesktopError::new(
                        DesktopErrorCode::RuntimeUnavailable,
                        error.to_string(),
                    )),
                )))
                .await?;
                crate::host_lifecycle::OwnedExecutionCleanup::Unresolved
            }
        };
        execution_host
            .complete_shutdown(crate::host_lifecycle::ShutdownProof {
                durable_state_flushed: true,
                owned_execution: cleanup,
            })
            .map_err(host_error)?;
        self.publish_host_changes(
            &execution_host,
            &state.snapshot,
            &mut host_cursor,
            &navigation,
            &layout,
            settings.snapshot(),
        )
        .await?;
        if exit == ChatExit::Quit {
            self.publish_critical(DesktopUpdate::BackendStopped {
                expected: true,
                error: None,
            })
            .await?;
        }
        Ok(exit)
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_managed_command(
        &self,
        command: BridgeCommand,
        driver: &ManagedTuiDriver,
        execution_host: &ExecutionHost,
        controller: &DesktopController,
        state: &mut ManagedDesktopState,
        active_run: &mut Option<HostedRun>,
        navigation: &mut DesktopNavigationSnapshot,
        navigation_store: &navigation::DesktopNavigationStore,
        layout: &mut DesktopResolvedLayout,
        layout_store: &layout::DesktopLayoutStore,
        settings: &mut settings::DesktopSettingsState,
        attachments: &DesktopAttachmentService,
    ) -> Result<Option<ChatExit>, DesktopError> {
        let command_id = command.command_id;
        if !matches!(
            &command.value,
            BridgeCommandValue::RequestSnapshot
                | BridgeCommandValue::SetSidebarMode { .. }
                | BridgeCommandValue::SaveLayout { .. }
                | BridgeCommandValue::ResetLayout
                | BridgeCommandValue::SaveLayoutAsDefault
                | BridgeCommandValue::ClearDefaultLayout
                | BridgeCommandValue::BeginSettings
                | BridgeCommandValue::SetSetting { .. }
                | BridgeCommandValue::ResetSetting { .. }
                | BridgeCommandValue::RevertSetting { .. }
                | BridgeCommandValue::ValidateSettings { .. }
                | BridgeCommandValue::CommitSettings { .. }
                | BridgeCommandValue::DiscardSettings { .. }
                | BridgeCommandValue::ReloadSettings
        ) && let Err(error) =
            execution_host.require_controller(&controller.conversation, controller.client_id)
        {
            self.publish_command_result(command_id, Err(host_error(error)))
                .await?;
            return Ok(None);
        }

        match command.value {
            BridgeCommandValue::Vision(_) => {
                self.publish_command_result(
                    command_id,
                    Err(vision::invalid(DesktopVisionError::Unsupported)),
                )
                .await?;
            }
            BridgeCommandValue::BrowserControl(_) => {
                self.publish_command_result(command_id, Err(DesktopError::new(
                    DesktopErrorCode::UnsupportedExecutionOwner,
                    "Xana browser controls require a native runtime; the managed vendor owns its browser tools",
                ))).await?;
            }
            BridgeCommandValue::RequestSnapshot => {
                let host = execution_host.snapshot().map_err(host_error)?;
                self.publish_critical(DesktopUpdate::Snapshot(Box::new(state.project_snapshot(
                    &host,
                    &self.notification_policy,
                    navigation,
                    layout,
                    settings.snapshot(),
                ))))
                .await?;
                self.publish_command_result(command_id, Ok(())).await?;
            }
            BridgeCommandValue::SetSidebarMode { mode } => {
                let result = navigation_store.set_sidebar_mode(mode);
                if result.is_ok() {
                    navigation.sidebar_mode = mode;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::SwitchConversation { conversation_id } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                let result = navigation_store.resolve_conversation(&conversation_id);
                match result {
                    Ok(Some(destination)) => {
                        self.publish_command_result(command_id, Ok(())).await?;
                        return Ok(Some(ChatExit::DesktopSwitchConversation {
                            workspace: destination.workspace,
                            conversation: destination.conversation,
                        }));
                    }
                    Ok(None) => {
                        self.publish_command_result(
                            command_id,
                            Err(DesktopError::new(
                                DesktopErrorCode::StateInvalid,
                                format!("Conversation {conversation_id} is no longer available"),
                            )),
                        )
                        .await?;
                    }
                    Err(error) => self.publish_command_result(command_id, Err(error)).await?,
                }
            }
            BridgeCommandValue::NewConversation { project_id } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                match navigation_store.resolve_new_workspace(project_id.as_deref()) {
                    Ok(workspace) => {
                        self.publish_command_result(command_id, Ok(())).await?;
                        return Ok(Some(ChatExit::DesktopNewConversation { workspace }));
                    }
                    Err(error) => self.publish_command_result(command_id, Err(error)).await?,
                }
            }
            BridgeCommandValue::ArchiveManagedConversation { conversation_id } => {
                let result = navigation_store
                    .archive_managed_conversation(&conversation_id)
                    .and_then(|archived| {
                        if archived {
                            Ok(())
                        } else {
                            Err(DesktopError::new(
                                DesktopErrorCode::StateInvalid,
                                format!("Conversation {conversation_id} was already absent"),
                            ))
                        }
                    });
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&state.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::RenameProject { project_id, name } => {
                let result = navigation_store.rename_project(&project_id, &name);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&state.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::SetProjectArchived {
                project_id,
                archived,
            } => {
                let result = navigation_store.set_project_archived(&project_id, archived);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&state.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::UngroupConversation { conversation_id } => {
                let result = navigation_store.ungroup_conversation(&conversation_id);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&state.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::MoveConversation {
                conversation_id,
                project_id,
                allow_fresh_continuation,
            } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                match navigation_store.move_conversation(
                    &conversation_id,
                    &project_id,
                    allow_fresh_continuation,
                ) {
                    Ok(Some(destination)) => {
                        self.publish_command_result(command_id, Ok(())).await?;
                        return Ok(Some(ChatExit::DesktopSwitchConversation {
                            workspace: destination.workspace,
                            conversation: destination.conversation,
                        }));
                    }
                    Ok(None) => {
                        *navigation =
                            navigation_store.snapshot(Some(&state.conversation.to_string()))?;
                        self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                            .await?;
                        self.publish_command_result(command_id, Ok(())).await?;
                    }
                    Err(error) => self.publish_command_result(command_id, Err(error)).await?,
                }
            }
            BridgeCommandValue::BranchConversation {
                conversation_id,
                source_point,
            } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                match navigation_store.branch_conversation(&conversation_id, &source_point) {
                    Ok(destination) => {
                        self.publish_command_result(command_id, Ok(())).await?;
                        return Ok(Some(ChatExit::DesktopSwitchConversation {
                            workspace: destination.workspace,
                            conversation: destination.conversation,
                        }));
                    }
                    Err(error) => self.publish_command_result(command_id, Err(error)).await?,
                }
            }
            BridgeCommandValue::SaveLayout { layout: candidate } => {
                let result = candidate.validate().and_then(|()| {
                    layout_store.save_conversation(&state.conversation.to_string(), &candidate)
                });
                if result.is_ok() {
                    *layout = DesktopResolvedLayout {
                        layout: candidate,
                        source: DesktopLayoutSource::Conversation,
                        warning: None,
                    };
                    self.publish_critical(DesktopUpdate::Layout(Box::new(layout.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::ResetLayout => {
                let result = layout_store.clear_conversation(&state.conversation.to_string());
                if result.is_ok() {
                    *layout = layout_store.resolve(&state.conversation.to_string());
                    self.publish_critical(DesktopUpdate::Layout(Box::new(layout.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::SaveLayoutAsDefault => {
                self.publish_command_result(command_id, layout_store.save_default(&layout.layout))
                    .await?;
            }
            BridgeCommandValue::ClearDefaultLayout => {
                let result = layout_store.clear_default();
                if result.is_ok() {
                    *layout = layout_store.resolve(&state.conversation.to_string());
                    self.publish_critical(DesktopUpdate::Layout(Box::new(layout.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::BeginSettings => {
                let result = settings.begin();
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::SetSetting {
                draft_id,
                key,
                value,
            } => {
                let result = settings.set(draft_id, &key, &value);
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::ResetSetting { draft_id, key } => {
                let result = settings.reset(draft_id, &key);
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::RevertSetting { draft_id, key } => {
                let result = settings.revert(draft_id, &key);
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::ValidateSettings { draft_id } => {
                let result = settings.validate(draft_id);
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::CommitSettings { draft_id } => {
                let result = settings.commit(draft_id);
                if let Ok((receipt, snapshot)) = &result {
                    self.publish_critical(DesktopUpdate::SettingsReceipt(receipt.clone()))
                        .await?;
                    self.publish_critical(DesktopUpdate::Settings(snapshot.clone()))
                        .await?;
                    self.publish_critical(DesktopUpdate::SettingsDraft(None))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::DiscardSettings { draft_id } => {
                let result = settings.discard(draft_id);
                if result.is_ok() {
                    self.publish_critical(DesktopUpdate::SettingsDraft(None))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::ReloadSettings => {
                let result = settings.reload();
                if let Ok(snapshot) = &result {
                    self.publish_critical(DesktopUpdate::Settings(snapshot.clone()))
                        .await?;
                    self.publish_critical(DesktopUpdate::SettingsDraft(None))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::StageResource {
                path,
                external_approved,
            } => {
                let service = attachments.clone();
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
                    self.publish_critical(DesktopUpdate::AttachmentStaged {
                        command_id,
                        attachment: attachment.clone(),
                    })
                    .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::StageClipboardImage => {
                let service = attachments.clone();
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
                    self.publish_critical(DesktopUpdate::AttachmentStaged {
                        command_id,
                        attachment: attachment.clone(),
                    })
                    .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
            }
            BridgeCommandValue::SaveArtifact {
                artifact_id,
                destination,
            } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    &state.snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Save(destination),
                )
                .await;
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::RevealArtifact { artifact_id } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    &state.snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Reveal,
                )
                .await;
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::OpenArtifact { artifact_id } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    &state.snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Open,
                )
                .await;
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::Submit {
                operation_id,
                input,
                attachments,
                acknowledge_workspace_write_collision,
                correlation,
            } => {
                if correlation.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::UnsupportedExecutionOwner,
                            "managed command outcomes are not exposed by this adapter",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "a managed Run is already active",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                let run = match execution_host
                    .begin_foreground_run(
                        &controller.conversation,
                        operation_id.0,
                        RunAccess::WorkspaceWrite,
                        if acknowledge_workspace_write_collision {
                            WriteCollisionDecision::Acknowledge
                        } else {
                            WriteCollisionDecision::Reject
                        },
                    )
                    .await
                {
                    Ok(run) => run,
                    Err(error) => {
                        self.publish_command_result(command_id, Err(host_error(error)))
                            .await?;
                        return Ok(None);
                    }
                };
                let images = match validate_managed_attachments(attachments) {
                    Ok(images) => images,
                    Err(error) => {
                        execution_host
                            .finish_run(run, Err(error.message.clone()))
                            .map_err(host_error)?;
                        self.publish_command_result(command_id, Err(error)).await?;
                        return Ok(None);
                    }
                };
                state.begin_run(operation_id.0, input.clone(), &images);
                let result = driver
                    .submit(operation_id.0, input, images)
                    .await
                    .map_err(|error| {
                        DesktopError::new(DesktopErrorCode::RuntimeUnavailable, error)
                    });
                if result.is_ok() {
                    *active_run = Some(run);
                    self.publish_critical(DesktopUpdate::Observation(state.observation(
                        DesktopEvent::OperationState {
                            operation_id,
                            state: DesktopOperationState::Running,
                        },
                    )))
                    .await?;
                } else {
                    let reason = result.as_ref().err().map_or_else(
                        || "managed runtime rejected the Run".to_owned(),
                        ToString::to_string,
                    );
                    execution_host
                        .finish_run(run, Err(reason))
                        .map_err(host_error)?;
                    state.active_operation = None;
                    state.snapshot.active_operation = None;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::Clear => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                let workspace = navigation_store.resolve_new_workspace(None)?;
                self.publish_command_result(command_id, Ok(())).await?;
                return Ok(Some(ChatExit::DesktopNewConversation { workspace }));
            }
            BridgeCommandValue::Interrupt { operation_id } => {
                let result = if driver.interrupt(operation_id.0) {
                    Ok(())
                } else {
                    Err(DesktopError::new(
                        DesktopErrorCode::StateInvalid,
                        "the selected managed Run is no longer active",
                    ))
                };
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::SelectManagedModel { model } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                match driver.select_model(model).await {
                    Ok(selection) => {
                        state.apply_selection(&selection, &driver.models);
                        let receipt = format!(
                            "Managed model changed to {}; existing Codex thread and context retained",
                            selection.model
                        );
                        self.publish_critical(DesktopUpdate::Observation(state.observation(
                            DesktopEvent::ExecutionSelectionChanged {
                                model: selection.model,
                                reasoning_effort: selection.reasoning_effort,
                                receipt,
                            },
                        )))
                        .await?;
                        self.publish_command_result(command_id, Ok(())).await?;
                    }
                    Err(error) => {
                        self.publish_command_result(
                            command_id,
                            Err(DesktopError::new(DesktopErrorCode::CommandRejected, error)),
                        )
                        .await?;
                    }
                }
            }
            BridgeCommandValue::SetManagedReasoning { effort } => {
                if reject_while_running(self, command_id, active_run).await? {
                    return Ok(None);
                }
                match driver.set_reasoning(effort).await {
                    Ok(selection) => {
                        state.apply_selection(&selection, &driver.models);
                        let receipt = format!(
                            "Managed reasoning changed to {}; model and existing Codex context retained",
                            selection.reasoning_effort.as_deref().unwrap_or("auto")
                        );
                        self.publish_critical(DesktopUpdate::Observation(state.observation(
                            DesktopEvent::ExecutionSelectionChanged {
                                model: selection.model,
                                reasoning_effort: selection.reasoning_effort,
                                receipt,
                            },
                        )))
                        .await?;
                        self.publish_command_result(command_id, Ok(())).await?;
                    }
                    Err(error) => {
                        self.publish_command_result(
                            command_id,
                            Err(DesktopError::new(DesktopErrorCode::CommandRejected, error)),
                        )
                        .await?;
                    }
                }
            }
            BridgeCommandValue::DecidePermission {
                permission_id,
                decision,
            } => {
                let DesktopPermissionTarget::Managed {
                    operation_id,
                    request_id,
                } = permission_id.0
                else {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::CommandRejected,
                            "native approval was sent to the managed runtime",
                        )),
                    )
                    .await?;
                    return Ok(None);
                };
                let Some(pending) = state.pending_approvals.remove(&request_id) else {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::StateInvalid,
                            "managed approval is no longer pending",
                        )),
                    )
                    .await?;
                    return Ok(None);
                };
                let result = if state.active_operation == Some(operation_id)
                    && matches!(
                        decision,
                        ControllerDecision::AllowOnce | ControllerDecision::Deny
                    ) {
                    pending.resolve(decision == ControllerDecision::AllowOnce)
                } else {
                    Err(DesktopError::new(
                        DesktopErrorCode::StateInvalid,
                        "memory or managed approval belongs to an inactive operation",
                    ))
                };
                if result.is_ok() {
                    self.publish_critical(DesktopUpdate::Observation(state.observation(
                        DesktopEvent::PermissionResolved {
                            permission_id: DesktopPermissionId::managed(operation_id, request_id),
                        },
                    )))
                    .await?;
                }
                self.publish_command_result(command_id, result).await?;
            }
            BridgeCommandValue::DecideRoundBudget { .. } => {
                self.publish_command_result(command_id, Err(DesktopError::new(
                    DesktopErrorCode::UnsupportedExecutionOwner,
                    "Codex owns its turn budget; Xana cannot continue a native round suspension for a managed Run",
                ))).await?;
            }
            BridgeCommandValue::Shutdown => {
                execution_host.request_shutdown().map_err(host_error)?;
                self.publish_command_result(command_id, Ok(())).await?;
                return Ok(Some(ChatExit::Quit));
            }
        }
        Ok(None)
    }

    async fn handle_managed_event(
        &self,
        event: ManagedTuiEvent,
        execution_host: &ExecutionHost,
        controller: &DesktopController,
        state: &mut ManagedDesktopState,
        active_run: &mut Option<HostedRun>,
    ) -> Result<(), DesktopError> {
        match event {
            ManagedTuiEvent::MemoryAudit(fact) => {
                let activity = state.activity(
                    "memory_permission",
                    DesktopActivityState::Completed,
                    "memory.permission",
                    Some(&format!(
                        "Xana {}: {:?}",
                        fact.request.tool_name, fact.effective
                    )),
                    DesktopActivityDisclosure::Summary,
                );
                self.publish_critical(DesktopUpdate::Observation(
                    state.observation(DesktopEvent::ActivityUpserted(activity)),
                ))
                .await?;
            }
            ManagedTuiEvent::Notification(event) => {
                state.record_managed(execution_host, &event)?;
                let projected = project_managed_event(state, &event);
                self.publish_critical(DesktopUpdate::Observation(state.observation(projected)))
                    .await?;
            }
            ManagedTuiEvent::Approval { request, reply } => {
                let Some(operation_id) = state.active_operation else {
                    let _ = reply.send(ApprovalDecision::Cancel);
                    return Err(DesktopError::new(
                        DesktopErrorCode::StateInvalid,
                        "managed runtime requested approval without an active Run",
                    ));
                };
                let request_id = state.next_approval_id;
                state.next_approval_id = state.next_approval_id.saturating_add(1);
                let approval = project_managed_approval(operation_id, request_id, &request);
                state
                    .pending_approvals
                    .insert(request_id, PendingApproval::Vendor { request, reply });
                self.publish_critical(DesktopUpdate::Observation(state.observation(
                    DesktopEvent::PermissionRequired {
                        public_web: false,
                        permission_id: approval.id,
                        tool: approval.tool,
                        effect: approval.effect,
                        scope: approval.scope,
                    },
                )))
                .await?;
            }
            ManagedTuiEvent::MemoryApproval { request, reply } => {
                let operation_id = request.operation_id;
                if state.active_operation != Some(operation_id) {
                    let _ = reply.send(crate::permission::ControllerDecision::Deny);
                    return Ok(());
                }
                let request_id = state.next_approval_id;
                state.next_approval_id = state.next_approval_id.saturating_add(1);
                let pending = PendingApproval::Memory {
                    request: Box::new(request),
                    reply,
                };
                let approval = pending.projection(operation_id, request_id);
                state.pending_approvals.insert(request_id, pending);
                self.publish_critical(DesktopUpdate::Observation(state.observation(
                    DesktopEvent::PermissionRequired {
                        public_web: false,
                        permission_id: approval.id,
                        tool: approval.tool,
                        effect: approval.effect,
                        scope: approval.scope,
                    },
                )))
                .await?;
            }
            ManagedTuiEvent::ThreadOpened(thread_id) => {
                state.thread_id = thread_id;
                let activity = state.activity(
                    "thread",
                    DesktopActivityState::Completed,
                    "managed.thread_ready",
                    Some("Managed thread is ready"),
                    DesktopActivityDisclosure::Summary,
                );
                self.publish_critical(DesktopUpdate::Observation(
                    state.observation(DesktopEvent::ActivityUpserted(activity)),
                ))
                .await?;
            }
            ManagedTuiEvent::TurnFinished {
                operation_id,
                error,
            } => {
                let message = state.finish_run(operation_id, error.clone());
                self.publish_critical(DesktopUpdate::Observation(state.observation(
                    DesktopEvent::MessageFinal {
                        operation_id: DesktopOperationId(operation_id),
                        message,
                    },
                )))
                .await?;
                let (desktop_state, outcome) = terminal_outcome(error.as_deref());
                self.publish_critical(DesktopUpdate::Observation(state.observation(
                    DesktopEvent::OperationState {
                        operation_id: DesktopOperationId(operation_id),
                        state: desktop_state,
                    },
                )))
                .await?;
                if let Some(run) = active_run.take() {
                    execution_host
                        .finish_run(run, outcome)
                        .map_err(host_error)?;
                }
            }
            ManagedTuiEvent::Cleared => {
                self.publish_critical(DesktopUpdate::Observation(
                    state.observation(DesktopEvent::ConversationCleared),
                ))
                .await?;
            }
        }
        execution_host
            .require_controller(&controller.conversation, controller.client_id)
            .map_err(host_error)?;
        Ok(())
    }
}

fn project_managed_approval(
    operation_id: OperationId,
    request_id: u64,
    request: &ApprovalRequest,
) -> DesktopPendingApproval {
    DesktopPendingApproval {
        public_web: false,
        id: DesktopPermissionId::managed(operation_id, request_id),
        tool: bounded_text(
            request
                .command
                .clone()
                .unwrap_or_else(|| request.method.clone()),
            MAX_PUBLIC_TEXT_BYTES,
        ),
        effect: bounded_text(
            request
                .reason
                .clone()
                .unwrap_or_else(|| request.method.clone()),
            MAX_PUBLIC_TEXT_BYTES,
        ),
        scope: bounded_text(
            request
                .cwd
                .clone()
                .unwrap_or_else(|| "managed runtime scope".to_owned()),
            MAX_PUBLIC_TEXT_BYTES,
        ),
    }
}

async fn reject_while_running(
    bridge: &Bridge,
    command_id: u64,
    active_run: &Option<HostedRun>,
) -> Result<bool, DesktopError> {
    if active_run.is_none() {
        return Ok(false);
    }
    bridge
        .publish_command_result(
            command_id,
            Err(DesktopError::new(
                DesktopErrorCode::HostBusy,
                "wait for or interrupt the active Run before changing Conversations",
            )),
        )
        .await?;
    Ok(true)
}

fn project_managed_event(
    state: &mut ManagedDesktopState,
    event: &crate::frontend::ManagedClientEvent,
) -> DesktopEvent {
    use crate::frontend::ManagedClientEvent;
    match event {
        ManagedClientEvent::TerminalDiagnostic(diagnostic) => {
            state.facts.terminal_diagnostics.push(diagnostic.clone());
            if state.facts.terminal_diagnostics.len() > 64 {
                state.facts.terminal_diagnostics.remove(0);
            }
            DesktopEvent::ActivityUpserted(state.activity(
                "terminal-diagnostic",
                DesktopActivityState::Failed,
                "managed.terminal_diagnostic",
                Some(&format!(
                    "{:?}: {:?}",
                    diagnostic.outcome, diagnostic.failure.category
                )),
                DesktopActivityDisclosure::Summary,
            ))
        }
        ManagedClientEvent::ThreadReady => DesktopEvent::ActivityUpserted(state.activity(
            "thread",
            DesktopActivityState::Completed,
            "managed.thread_ready",
            Some("Managed thread is ready"),
            DesktopActivityDisclosure::Summary,
        )),
        ManagedClientEvent::AssistantDelta(delta) => {
            state.assistant.push_str(delta);
            state.assistant =
                bounded_text(std::mem::take(&mut state.assistant), MAX_PUBLIC_TEXT_BYTES);
            DesktopEvent::AssistantDelta {
                operation_id: DesktopOperationId(
                    state
                        .active_operation
                        .expect("managed delta belongs to an active Run"),
                ),
                text: delta.clone(),
            }
        }
        ManagedClientEvent::ReasoningSummaryDelta(delta) => {
            DesktopEvent::ActivityUpserted(state.activity(
                "reasoning-summary",
                DesktopActivityState::Working,
                "managed.reasoning_summary",
                Some(delta),
                DesktopActivityDisclosure::Summary,
            ))
        }
        ManagedClientEvent::ReasoningSummaryPartAdded => {
            DesktopEvent::ActivityUpserted(state.activity(
                "reasoning-summary",
                DesktopActivityState::Working,
                "managed.reasoning_summary",
                None,
                DesktopActivityDisclosure::Summary,
            ))
        }
        ManagedClientEvent::ReasoningDelta(delta) => {
            DesktopEvent::ActivityUpserted(state.activity(
                "reasoning",
                DesktopActivityState::Working,
                "managed.reasoning_detail",
                Some(delta),
                DesktopActivityDisclosure::Detail,
            ))
        }
        ManagedClientEvent::PlanDelta(delta) => DesktopEvent::ActivityUpserted(state.activity(
            "plan",
            DesktopActivityState::Working,
            "managed.plan",
            Some(delta),
            DesktopActivityDisclosure::Detail,
        )),
        ManagedClientEvent::PlanUpdated {
            explanation,
            steps,
            truncated,
        } => {
            let mut detail = explanation.clone().unwrap_or_default();
            for step in steps {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str(&format!("[{}] {}", step.status, step.step));
            }
            if *truncated {
                detail.push_str("\n… additional plan steps omitted");
            }
            DesktopEvent::ActivityUpserted(state.activity(
                "plan",
                DesktopActivityState::Working,
                "managed.plan_updated",
                Some(&detail),
                DesktopActivityDisclosure::Detail,
            ))
        }
        ManagedClientEvent::CommandOutputDelta(delta) => {
            DesktopEvent::ActivityUpserted(state.activity(
                "command-output",
                DesktopActivityState::Working,
                "managed.command_output",
                Some(delta),
                DesktopActivityDisclosure::Detail,
            ))
        }
        ManagedClientEvent::DiffUpdated { preview, truncated } => {
            let detail = if *truncated {
                format!("{preview}\n… diff omitted")
            } else {
                preview.clone()
            };
            DesktopEvent::ActivityUpserted(state.activity(
                "diff",
                DesktopActivityState::Working,
                "managed.diff_updated",
                Some(&detail),
                DesktopActivityDisclosure::Detail,
            ))
        }
        ManagedClientEvent::ItemStarted(item) => DesktopEvent::ActivityUpserted(state.activity(
            &format!("item:{}", item.id),
            DesktopActivityState::Working,
            "managed.item_started",
            Some(&format!("{}\n{}", item.label, item.details)),
            DesktopActivityDisclosure::Detail,
        )),
        ManagedClientEvent::ItemCompleted(item) => DesktopEvent::ActivityUpserted(state.activity(
            &format!("item:{}", item.id),
            DesktopActivityState::Completed,
            "managed.item_completed",
            Some(&format!("{}\n{}", item.label, item.details)),
            DesktopActivityDisclosure::Detail,
        )),
        ManagedClientEvent::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => {
            state.snapshot.model.clone_from(to_model);
            DesktopEvent::ActivityUpserted(state.activity(
                "model-route",
                DesktopActivityState::Completed,
                "managed.model_rerouted",
                Some(&format!("{from_model} → {to_model}: {reason}")),
                DesktopActivityDisclosure::Summary,
            ))
        }
        ManagedClientEvent::TurnCompleted { status, error } => {
            state.turn_status = Some((status.clone(), error.clone()));
            DesktopEvent::ActivityUpserted(state.activity(
                "turn",
                if error.is_some() || status != "completed" {
                    DesktopActivityState::Failed
                } else {
                    DesktopActivityState::Completed
                },
                "managed.turn_completed",
                Some(error.as_deref().unwrap_or(status)),
                DesktopActivityDisclosure::Summary,
            ))
        }
        ManagedClientEvent::TokenUsageUpdated {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_tokens,
            total_tokens,
            context_input_tokens,
            context_window_tokens,
        } => {
            state.observe_usage(ManagedTokenUsage {
                input_tokens: *input_tokens,
                cached_input_tokens: *cached_input_tokens,
                output_tokens: *output_tokens,
                reasoning_tokens: *reasoning_tokens,
                total_tokens: *total_tokens,
                context_input_tokens: *context_input_tokens,
                context_window_tokens: *context_window_tokens,
            });
            DesktopEvent::Usage {
                operation_id: DesktopOperationId(
                    state
                        .active_operation
                        .expect("managed usage belongs to an active Run"),
                ),
                input_tokens: Some(*input_tokens),
                output_tokens: Some(*output_tokens),
                total_tokens: Some(*total_tokens),
                requests: 1,
            }
        }
        ManagedClientEvent::Warning(message) => DesktopEvent::ActivityUpserted(state.activity(
            "warning",
            DesktopActivityState::Failed,
            "managed.warning",
            Some(message),
            DesktopActivityDisclosure::Detail,
        )),
    }
}

fn terminal_outcome(
    error: Option<&str>,
) -> (DesktopOperationState, Result<OperationOutcome, String>) {
    match error {
        None => (
            DesktopOperationState::Completed,
            Ok(OperationOutcome::Completed),
        ),
        Some(error) if error.contains("cancel") || error.contains("interrupt") => (
            DesktopOperationState::Interrupted,
            Ok(OperationOutcome::Interrupted),
        ),
        Some(error) => (DesktopOperationState::Failed, Err(error.to_owned())),
    }
}

fn validate_managed_attachments(
    attachments: Vec<DesktopAttachment>,
) -> Result<Vec<ImageAttachment>, DesktopError> {
    if attachments.len() > crate::vision::MAX_IMAGES_PER_TURN {
        return Err(DesktopError::new(
            DesktopErrorCode::StateInvalid,
            format!(
                "turn has {} images; limit is {}",
                attachments.len(),
                crate::vision::MAX_IMAGES_PER_TURN
            ),
        ));
    }
    let mut ids = std::collections::HashSet::new();
    let mut bytes = 0_u64;
    let mut images = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        if !ids.insert(attachment.id.clone()) {
            return Err(DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("attachment {} appears more than once", attachment.id),
            ));
        }
        bytes = bytes.saturating_add(attachment.byte_len);
        images.push(attachment.into_managed_image()?);
    }
    if bytes > crate::vision::MAX_IMAGE_BYTES_PER_TURN {
        return Err(DesktopError::new(
            DesktopErrorCode::StateInvalid,
            format!(
                "turn images total {bytes} bytes; limit is {}",
                crate::vision::MAX_IMAGE_BYTES_PER_TURN
            ),
        ));
    }
    Ok(images)
}

fn freshness(observed_at_unix_millis: u64) -> DesktopFactFreshness {
    DesktopFactFreshness {
        observed_at_unix_millis,
        max_age_millis: None,
    }
}

fn observed_at_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn fallback_execution(
    snapshot: &ClientSnapshot,
    operation_id: OperationId,
) -> DesktopExecutionFact {
    DesktopExecutionFact {
        operation_id: operation_id.to_string(),
        owner: "managed".to_owned(),
        host_location: "embedded".to_owned(),
        workspace_authority: "workspace-write sandbox (managed runtime)".to_owned(),
        tool_authority: vec!["Codex-managed tool policy".to_owned()],
        connection: Some(snapshot.connection.clone()),
        model: Some(snapshot.model.clone()),
        capability_grants: Vec::new(),
        egress_policy: Some("Codex-managed provider and tool egress".to_owned()),
        controller: Some("Desktop foreground controller".to_owned()),
        approval_policy: snapshot.approval_policy.clone(),
        source: DesktopFactSource::ManagedRuntime,
        freshness: freshness(observed_at_unix_millis()),
    }
}

fn upsert_bounded<T, F>(values: &mut Vec<T>, value: T, limit: usize, matches: F)
where
    F: Fn(&T) -> bool,
{
    if let Some(existing) = values.iter_mut().find(|candidate| matches(candidate)) {
        *existing = value;
    } else {
        values.push(value);
        if values.len() > limit {
            values.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{frontend::ManagedClientEvent, resource::ResourcePolicyV1};

    fn test_state() -> ManagedDesktopState {
        let conversation = ConversationRef::Managed {
            conversation_id: crate::identity::ConversationId::new(),
            connection: "codex".to_owned(),
            thread_id: "thread-1".to_owned(),
        };
        let mut snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: crate::identity::SessionId::new(),
                connection: "codex".to_owned(),
                execution_owner: "managed_codex".to_owned(),
                model: "test-model".to_owned(),
                reasoning_effort: Some("medium".to_owned()),
                host_location: HostLocationV1::Embedded,
                approval_policy: "on-request".to_owned(),
                children: Vec::new(),
                resource_policy: ResourcePolicyV1::default(),
            },
            Vec::new(),
        );
        snapshot.semantic.conversation_id = conversation.conversation_id();
        ManagedDesktopState {
            snapshot,
            facts: DesktopConversationFacts {
                profile: Some("default".to_owned()),
                activity: Vec::new(),
                execution: Vec::new(),
                usage: Vec::new(),
                completions: Vec::new(),
                capabilities: Vec::new(),
                terminal_diagnostics: Vec::new(),
                prompt_ledger: DesktopPromptLedger {
                    details: Vec::new(),
                    operation_id: None,
                    estimated_input_tokens: None,
                    input_budget_tokens: None,
                    context_window_tokens: None,
                    context_window_source: None,
                    attachment_count: None,
                    attachment_bytes: None,
                    omitted_source_count: None,
                    unavailable_reason: Some("managed".to_owned()),
                },
            },
            conversation,
            thread_id: "thread-1".to_owned(),
            desktop_sequence: 0,
            runtime_sequence: 0,
            active_operation: Some(OperationId::new()),
            assistant: String::new(),
            activity_text: HashMap::new(),
            pending_approvals: HashMap::new(),
            next_approval_id: 1,
            usage_sequence: 0,
            last_usage: None,
            turn_status: None,
        }
    }

    #[test]
    fn managed_reasoning_deltas_replace_one_stable_activity_row() {
        let mut state = test_state();

        let first = project_managed_event(
            &mut state,
            &ManagedClientEvent::ReasoningSummaryDelta("Checking ".to_owned()),
        );
        let second = project_managed_event(
            &mut state,
            &ManagedClientEvent::ReasoningSummaryDelta("the workspace".to_owned()),
        );

        let (DesktopEvent::ActivityUpserted(first), DesktopEvent::ActivityUpserted(second)) =
            (first, second)
        else {
            panic!("reasoning must project as Activity")
        };
        assert_eq!(first.id, second.id);
        assert_eq!(state.facts.activity.len(), 1);
        assert_eq!(
            state.facts.activity[0].disclosed_text.as_deref(),
            Some("Checking the workspace")
        );
    }

    #[test]
    fn managed_usage_is_cumulative_and_source_qualified() {
        let mut state = test_state();
        let event = ManagedClientEvent::TokenUsageUpdated {
            input_tokens: 120,
            cached_input_tokens: Some(20),
            output_tokens: 30,
            reasoning_tokens: Some(10),
            total_tokens: 150,
            context_input_tokens: Some(120),
            context_window_tokens: Some(8_192),
        };

        let projected = project_managed_event(&mut state, &event);
        assert!(matches!(projected, DesktopEvent::Usage { requests: 1, .. }));
        assert_eq!(state.facts.usage.len(), 1);
        assert_eq!(
            state.facts.usage[0].source,
            DesktopFactSource::ManagedRuntime
        );
        assert_eq!(
            state.facts.usage[0].authority,
            DesktopFactAuthority::ProviderReported
        );
        assert_eq!(state.facts.usage[0].cached_input_tokens, Some(20));
    }

    #[test]
    fn managed_finalization_retains_bounded_transcript_and_receipt() {
        let mut state = test_state();
        let operation_id = state.active_operation.unwrap();
        state.assistant = "final answer".to_owned();
        state.last_usage = Some(ManagedTokenUsage {
            input_tokens: 10,
            cached_input_tokens: None,
            output_tokens: 4,
            reasoning_tokens: None,
            total_tokens: 14,
            context_input_tokens: None,
            context_window_tokens: None,
        });

        let message = state.finish_run(operation_id, None);

        assert!(matches!(
            message.content.as_slice(),
            [DesktopContent {
                value: DesktopContentValue::Text(text),
                ..
            }] if text == "final answer"
        ));
        assert_eq!(state.facts.completions.len(), 1);
        assert_eq!(state.facts.completions[0].status, "completed");
        assert_eq!(state.facts.completions[0].input_tokens, Some(10));
        assert!(state.active_operation.is_none());
    }

    #[test]
    fn terminal_error_classifies_interruption_without_losing_reason() {
        assert_eq!(
            terminal_outcome(Some("turn cancelled by user")),
            (
                DesktopOperationState::Interrupted,
                Ok(OperationOutcome::Interrupted)
            )
        );
        assert_eq!(
            terminal_outcome(Some("provider failed")),
            (
                DesktopOperationState::Failed,
                Err("provider failed".to_owned())
            )
        );
    }
}
