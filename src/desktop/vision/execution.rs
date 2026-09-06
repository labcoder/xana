//! Single-slot planning/analysis with exact owner rechecks and joined cancellation.

use super::*;
use crate::outbound::OutboundApprovalDecision;

impl State {
    pub(in crate::desktop) async fn command(
        &mut self,
        command_id: u64,
        command: Command,
        bridge: &Bridge,
        host: &ExecutionHost,
        controller: &DesktopController,
        active_run: &mut Option<HostedRun>,
    ) -> Result<(), DesktopError> {
        if host
            .require_controller(&controller.conversation, controller.client_id)
            .is_err()
        {
            return reject(bridge, command_id, DesktopVisionError::ControllerLost).await;
        }
        if let Command::Cancel { operation } = command {
            if let Some(job) = &self.job
                && job.operation == operation.0
            {
                job.cancellation.cancel();
                return bridge.publish_command_result(command_id, Ok(())).await;
            }
            if let Some(pending) = self.pending.take() {
                if pending.public.receipt.operation() == Some(operation.0) {
                    self.start_job(command_id, operation.0, move |config, _| async move {
                        register_sources(&config, &pending.images).await?;
                        let mut receipt = pending.public.receipt;
                        receipt.status = VisionStatus::Cancelled;
                        persist(&config, &receipt).await?;
                        Ok(Work::Receipt(receipt))
                    });
                    return Ok(());
                }
                self.pending = Some(pending);
            }
            return reject(bridge, command_id, DesktopVisionError::StalePlan).await;
        }
        if self.job.is_some() || active_run.is_some() {
            return reject(bridge, command_id, DesktopVisionError::Busy).await;
        }
        match command {
            Command::Plan {
                operation,
                prompt,
                attachments,
                route,
            } => {
                // A newer draft revokes the previous one before any I/O starts.
                self.pending = None;
                let Ok(controller_identity) = controller_identity(host, controller) else {
                    return reject(bridge, command_id, DesktopVisionError::ControllerLost).await;
                };
                self.start_job(
                    command_id,
                    operation.0,
                    move |config, cancellation| async move {
                        let pending = tokio::task::spawn_blocking(move || {
                            plan(
                                &config,
                                operation.0,
                                prompt,
                                attachments,
                                route,
                                controller_identity,
                            )
                        })
                        .await
                        .map_err(|_| DesktopVisionError::Unavailable)??;
                        if cancellation.is_cancelled() {
                            return Err(DesktopVisionError::Cancelled);
                        }
                        Ok(Work::Planned(Box::new(pending)))
                    },
                );
            }
            Command::Stage { upload } => {
                self.start_job(
                    command_id,
                    OperationId::new(),
                    move |config, cancellation| async move {
                        let attachment = tokio::task::spawn_blocking(move || {
                            if !matches!(
                                upload.media_type.as_str(),
                                "image/png" | "image/jpeg" | "image/gif"
                            ) {
                                return Err(DesktopVisionError::InvalidImage);
                            }
                            let value =
                                ImageIngestor::new(config.artifacts, ImageLimits::default())
                                    .ingest_bytes("selected-image", &upload.bytes, config.owner)
                                    .map_err(|_| DesktopVisionError::InvalidImage)?;
                            if value.image.media_type != upload.media_type {
                                return Err(DesktopVisionError::InvalidImage);
                            }
                            Ok(DesktopAttachment::from_image(value))
                        })
                        .await
                        .map_err(|_| DesktopVisionError::Unavailable)??;
                        if cancellation.is_cancelled() {
                            return Err(DesktopVisionError::Cancelled);
                        }
                        Ok(Work::Staged(attachment))
                    },
                );
            }
            Command::Inspect { operation } => {
                self.start_job(command_id, operation.0, move |config, _| async move {
                    tokio::task::spawn_blocking(move || reader::load(&config, operation.0))
                        .await
                        .map_err(|_| DesktopVisionError::Unavailable)?
                        .map(Work::Receipt)
                });
            }
            Command::Decide {
                plan,
                decision,
                acknowledge_collision,
            } => {
                let Some(pending) = self.pending.take() else {
                    return reject(bridge, command_id, DesktopVisionError::StalePlan).await;
                };
                if pending.public != *plan
                    || plan.receipt.conversation_id != self.config.conversation.to_string()
                    || Some(&pending.controller_identity)
                        != controller_identity(host, controller).ok().as_ref()
                {
                    return reject(bridge, command_id, DesktopVisionError::StalePlan).await;
                }
                let operation = plan
                    .receipt
                    .operation()
                    .ok_or_else(|| invalid(DesktopVisionError::StalePlan))?;
                if decision == DesktopVisionDecision::AllowOnce {
                    let Ok(run) = host
                        .begin_foreground_run(
                            &controller.conversation,
                            operation,
                            RunAccess::WorkspaceWrite,
                            if acknowledge_collision {
                                WriteCollisionDecision::Acknowledge
                            } else {
                                WriteCollisionDecision::Reject
                            },
                        )
                        .await
                    else {
                        return reject(bridge, command_id, DesktopVisionError::Busy).await;
                    };
                    *active_run = Some(run);
                    self.reserved_operation = Some(operation);
                }
                let host = host.clone();
                let controller = controller.clone();
                self.start_job(
                    command_id,
                    operation,
                    move |config, cancellation| async move {
                        execute(config, pending, decision, host, controller, cancellation).await
                    },
                );
            }
            Command::Cancel { .. } => unreachable!("handled above"),
        }
        bridge.publish_command_result(command_id, Ok(())).await
    }

    fn start_job<F, Fut>(&mut self, command_id: u64, operation: OperationId, work: F)
    where
        F: FnOnce(Configuration, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Work, DesktopVisionError>> + Send + 'static,
    {
        let config = self.config.clone();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        self.job = Some(Job {
            operation,
            cancellation,
            task: tokio::spawn(async move {
                Finished {
                    command_id,
                    operation,
                    result: work(config, token).await,
                }
            }),
        });
    }

    pub(in crate::desktop) async fn finished(
        &mut self,
        finished: Finished,
        bridge: &Bridge,
        owner: &crate::frontend::EmbeddedOwner,
        host: &ExecutionHost,
        controller: &DesktopController,
        active_run: &mut Option<HostedRun>,
    ) -> Result<(), DesktopError> {
        let command_id = finished.command_id;
        let owns_run = self.reserved_operation == Some(finished.operation);
        match finished.result {
            Ok(Work::Planned(pending)) => {
                let plan = pending.public.clone();
                self.pending = Some(*pending);
                bridge
                    .publish_critical(DesktopUpdate::Vision(DesktopVisionUpdate::Planned {
                        command_id,
                        plan,
                    }))
                    .await
            }
            Ok(Work::Staged(attachment)) => {
                bridge
                    .publish_critical(DesktopUpdate::AttachmentStaged {
                        command_id,
                        attachment,
                    })
                    .await
            }
            Ok(Work::Receipt(receipt)) => {
                if owns_run && let Some(run) = active_run.take() {
                    self.reserved_operation = None;
                    host.finish_run(
                        run,
                        Err("vision did not dispatch a conversational turn".into()),
                    )
                    .map_err(host_error)?;
                }
                bridge
                    .publish_critical(DesktopUpdate::Vision(DesktopVisionUpdate::Receipt {
                        command_id,
                        receipt,
                    }))
                    .await
            }
            Ok(Work::Submitted {
                receipt,
                input,
                owner_input,
                images,
                cancellation,
                controller_identity: expected_controller,
            }) => {
                if cancellation.is_cancelled() {
                    if owns_run && let Some(run) = active_run.take() {
                        self.reserved_operation = None;
                        host.finish_run(run, Err("vision continuation cancelled".into()))
                            .map_err(host_error)?;
                    }
                    return reject(bridge, command_id, DesktopVisionError::Cancelled).await;
                }
                if host
                    .require_controller(&controller.conversation, controller.client_id)
                    .is_err()
                    || controller_identity(host, controller).ok().as_ref()
                        != Some(&expected_controller)
                {
                    if owns_run && let Some(run) = active_run.take() {
                        self.reserved_operation = None;
                        host.finish_run(run, Err("vision controller was lost".into()))
                            .map_err(host_error)?;
                    }
                    return reject(bridge, command_id, DesktopVisionError::ControllerLost).await;
                }
                let operation_id = receipt
                    .operation()
                    .ok_or_else(|| invalid(DesktopVisionError::StalePlan))?;
                let command = if let Some(owner_input) = owner_input {
                    RuntimeCommand::SubmitDerivedTurn {
                        operation_id,
                        input,
                        owner_input,
                    }
                } else if images.is_empty() {
                    RuntimeCommand::SubmitTurn {
                        operation_id,
                        input,
                    }
                } else {
                    RuntimeCommand::SubmitTurnWithImages {
                        operation_id,
                        input,
                        images,
                    }
                };
                let admitted = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => false,
                    result = owner.send(ClientCommand::new(command)) => result.is_ok_and(|ack| ack.accepted),
                };
                if !admitted {
                    if owns_run && let Some(run) = active_run.take() {
                        self.reserved_operation = None;
                        host.finish_run(run, Err("vision runtime admission unavailable".into()))
                            .map_err(host_error)?;
                    }
                    return reject(
                        bridge,
                        command_id,
                        if cancellation.is_cancelled() {
                            DesktopVisionError::Cancelled
                        } else {
                            DesktopVisionError::Unavailable
                        },
                    )
                    .await;
                }
                if receipt.status == VisionStatus::NativeSubmitted {
                    self.native_admission = Some((command_id, receipt));
                    return Ok(());
                }
                self.reserved_operation = None;
                bridge
                    .publish_critical(DesktopUpdate::Vision(DesktopVisionUpdate::Receipt {
                        command_id,
                        receipt,
                    }))
                    .await
            }
            Err(reason) => {
                if owns_run && let Some(run) = active_run.take() {
                    self.reserved_operation = None;
                    host.finish_run(run, Err(reason.to_string()))
                        .map_err(host_error)?;
                }
                reject(bridge, command_id, reason).await
            }
        }
    }

    pub(in crate::desktop) async fn observe(
        &mut self,
        event: &ClientEvent,
        bridge: &Bridge,
    ) -> Result<(), DesktopError> {
        let Some((_, receipt)) = &self.native_admission else {
            return Ok(());
        };
        if matches!(event, ClientEvent::Runtime(event)
            if matches!(event.as_ref(), AgentEvent::TurnStartUnavailable { operation_id }
                if Some(*operation_id) == receipt.operation()))
        {
            let (command_id, _) = self
                .native_admission
                .take()
                .expect("selected native admission");
            self.reserved_operation = None;
            // There may have been a durable acceptance followed by an interrupted
            // boundary. Retain the Dispatching intent for exact Unknown inspection;
            // this observation is not evidence of either submission or failure.
            return reject(bridge, command_id, DesktopVisionError::Unavailable).await;
        }
        let status = match event {
            ClientEvent::Runtime(event) => match event.as_ref() {
                AgentEvent::OperationStateChanged {
                    operation_id,
                    state,
                } if Some(*operation_id) == receipt.operation() => match state {
                    OperationState::Running => Some(VisionStatus::NativeSubmitted),
                    OperationState::Finished(OperationOutcome::Interrupted) => {
                        Some(VisionStatus::Cancelled)
                    }
                    OperationState::Finished(OperationOutcome::Declined) => {
                        Some(VisionStatus::Denied)
                    }
                    OperationState::Finished(_) => Some(VisionStatus::Failed),
                    OperationState::Suspended => None,
                },
                _ => None,
            },
            _ => None,
        };
        if let Some(status) = status {
            let (command_id, mut receipt) = self
                .native_admission
                .take()
                .expect("selected native admission");
            self.reserved_operation = None;
            receipt.status = status;
            if persist(&self.config, &receipt).await.is_err() {
                return reject(bridge, command_id, DesktopVisionError::StorageUnavailable).await;
            }
            bridge
                .publish_critical(DesktopUpdate::Vision(DesktopVisionUpdate::Receipt {
                    command_id,
                    receipt,
                }))
                .await?;
        }
        Ok(())
    }
}

fn controller_identity(
    host: &ExecutionHost,
    controller: &DesktopController,
) -> Result<String, DesktopError> {
    host.snapshot()
        .map_err(host_error)?
        .conversations
        .into_iter()
        .find(|value| value.conversation == controller.conversation)
        .and_then(|value| value.controller)
        .map(|lease| format!("{}:{}", lease.controller_id, lease.generation))
        .ok_or_else(|| invalid(DesktopVisionError::ControllerLost))
}

async fn reject(
    bridge: &Bridge,
    command_id: u64,
    reason: DesktopVisionError,
) -> Result<(), DesktopError> {
    bridge
        .publish_critical(DesktopUpdate::Vision(DesktopVisionUpdate::Rejected {
            command_id,
            reason,
        }))
        .await?;
    bridge
        .publish_command_result(command_id, Err(invalid(reason)))
        .await
}

fn current_config(config: &Configuration) -> Result<String, DesktopVisionError> {
    let bytes = crate::bounded_file::read(&config.config_file, 1024 * 1024)
        .map_err(|_| DesktopVisionError::Unavailable)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| DesktopVisionError::Unavailable)?;
    let registry = crate::config::XanaConfig::parse_registry(text)
        .map_err(|_| DesktopVisionError::Unavailable)?;
    if !config.service.matches_registry(&registry) {
        return Err(DesktopVisionError::StalePlan);
    }
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn plan(
    config: &Configuration,
    operation: OperationId,
    prompt: String,
    attachments: Vec<DesktopAttachment>,
    route: Option<String>,
    controller_identity: String,
) -> Result<Pending, DesktopVisionError> {
    let config_digest = current_config(config)?;
    let images =
        validate_desktop_attachments(attachments).map_err(|_| DesktopVisionError::InvalidImage)?;
    if images.is_empty() {
        return Err(DesktopVisionError::InvalidImage);
    }
    let mut total = 0u64;
    for image in &images {
        if image.artifact.owner != config.owner {
            return Err(DesktopVisionError::WrongOwner);
        }
        let bytes = config
            .artifacts
            .read_bounded(&image.artifact, crate::artifact::MAX_ARTIFACT_BYTES)
            .map_err(|_| DesktopVisionError::InvalidImage)?;
        let metadata = crate::vision::inspect_image(&bytes, ImageLimits::default())
            .map_err(|_| DesktopVisionError::InvalidImage)?;
        if metadata.media_type != image.media_type || image.byte_len != bytes.len() as u64 {
            return Err(DesktopVisionError::InvalidImage);
        }
        total = total.saturating_add(bytes.len() as u64);
    }
    if total > crate::vision::MAX_IMAGE_BYTES_PER_TURN {
        return Err(DesktopVisionError::InvalidImage);
    }
    let (specialist, approval_required) = match config
        .service
        .route_turn(config.native_images, route.as_deref())
        .map_err(|_| DesktopVisionError::Unsupported)?
    {
        VisionTurnRoute::Native => (None, false),
        VisionTurnRoute::SpecialistDenied(_) => return Err(DesktopVisionError::Denied),
        VisionTurnRoute::SpecialistAllowed(plan) => (Some(*plan), false),
        VisionTurnRoute::SpecialistApprovalRequired(plan) => (Some(*plan), true),
    };
    let destination = if let Some(plan) = &specialist {
        let recipient = crate::app::vision::vision_recipient(&plan.route)
            .map_err(|_| DesktopVisionError::Unsupported)?;
        VisionDestination {
            route: Some(plan.route.name.clone()),
            connection: plan.route.connection.clone(),
            model: plan.route.model.clone(),
            adapter: plan.route.adapter.clone(),
            recipient: recipient.destination,
            recipient_digest: recipient.identity_digest,
        }
    } else {
        config.native_destination.clone()
    };
    let sources = images.iter().map(|image| source(&image.artifact)).collect();
    let mut receipt = VisionReceipt {
        version: 1,
        conversation_id: config.conversation.to_string(),
        operation_id: operation.to_string(),
        revision: 1,
        plan_digest: "0".repeat(64),
        prompt_digest: blake3::hash(prompt.as_bytes()).to_hex().to_string(),
        destination,
        sources,
        status: VisionStatus::Dispatching,
        usage: VisionUsage::default(),
        derivative: None,
        untrusted_derivative: true,
    };
    let nonce = uuid::Uuid::new_v4();
    receipt.plan_digest = blake3::hash(
        &serde_json::to_vec(&(&receipt, &config_digest, &controller_identity, nonce))
            .map_err(|_| DesktopVisionError::Unavailable)?,
    )
    .to_hex()
    .to_string();
    if !receipt.valid_for(config.conversation) {
        return Err(DesktopVisionError::InvalidImage);
    }
    Ok(Pending {
        public: DesktopVisionPlan {
            receipt,
            prompt,
            approval_required,
            nonce,
            operation: DesktopOperationId(operation),
        },
        images,
        specialist,
        config_digest,
        controller_identity,
    })
}

fn source(artifact: &crate::artifact::ArtifactRecord) -> VisionSource {
    VisionSource {
        artifact_id: artifact.reference.id.to_string(),
        digest: artifact.reference.content_hash.as_str().into(),
        media_type: artifact.media_type.clone(),
        byte_len: artifact.byte_len,
    }
}

async fn persist(
    config: &Configuration,
    receipt: &VisionReceipt,
) -> Result<(), DesktopVisionError> {
    config
        .writer
        .append(
            crate::session::SessionRecord::VisionReceiptRecorded {
                receipt: receipt.clone(),
            },
            None,
        )
        .await
        .map_err(|_| DesktopVisionError::StorageUnavailable)
}

async fn execute(
    config: Configuration,
    pending: Pending,
    decision: DesktopVisionDecision,
    host: ExecutionHost,
    controller: DesktopController,
    cancellation: CancellationToken,
) -> Result<Work, DesktopVisionError> {
    register_sources(&config, &pending.images).await?;
    let mut receipt = pending.public.receipt;
    if decision == DesktopVisionDecision::Deny {
        receipt.status = VisionStatus::Denied;
        persist(&config, &receipt).await?;
        return Ok(Work::Receipt(receipt));
    }
    if current_config(&config)? != pending.config_digest {
        return Err(DesktopVisionError::StalePlan);
    }
    if cancellation.is_cancelled() {
        return Err(DesktopVisionError::Cancelled);
    }
    if host
        .require_controller(&controller.conversation, controller.client_id)
        .is_err()
        || controller_identity(&host, &controller).ok().as_ref()
            != Some(&pending.controller_identity)
    {
        return Err(DesktopVisionError::ControllerLost);
    }
    // Immutable dispatch intent precedes network work. A crash here is Unknown
    // on inspection, never a reusable approval or an automatic retry.
    persist(&config, &receipt).await?;
    let Some(specialist) = pending.specialist else {
        receipt.revision = 2;
        receipt.status = VisionStatus::NativeSubmitted;
        return Ok(Work::Submitted {
            receipt,
            input: pending.public.prompt,
            owner_input: None,
            images: pending.images,
            cancellation,
            controller_identity: pending.controller_identity,
        });
    };
    let checked_config = config.clone();
    let checked_host = host.clone();
    let checked_controller = controller.clone();
    let expected_config = pending.config_digest;
    let expected_controller = pending.controller_identity.clone();
    let service = config
        .service
        .clone()
        .with_dispatch_check(Arc::new(move || {
            checked_host
                .require_controller(
                    &checked_controller.conversation,
                    checked_controller.client_id,
                )
                .is_ok()
                && controller_identity(&checked_host, &checked_controller)
                    .ok()
                    .as_ref()
                    == Some(&expected_controller)
                && current_config(&checked_config).ok().as_ref() == Some(&expected_config)
        }));
    let operation = receipt.operation().ok_or(DesktopVisionError::StalePlan)?;
    let analysis = service.execute(
        operation,
        pending.public.prompt,
        pending.images,
        specialist,
        Some(OutboundApprovalDecision::AllowOnce),
        cancellation.clone(),
    );
    tokio::pin!(analysis);
    let mut revoked = false;
    let result = loop {
        tokio::select! {
            result = &mut analysis => break result,
            () = tokio::time::sleep(Duration::from_millis(50)) => {
                if host.require_controller(&controller.conversation, controller.client_id).is_err() { revoked = true; cancellation.cancel(); }
            }
        }
    };
    receipt.revision = 2;
    match result {
        Ok(prepared) if !cancellation.is_cancelled() => {
            let artifacts = config.artifacts.clone();
            let owner = config.owner;
            let derived = prepared.derived_text;
            let artifact = tokio::task::spawn_blocking(move || {
                artifacts.put(derived.as_bytes(), "text/plain", owner)
            })
            .await
            .map_err(|_| DesktopVisionError::StorageUnavailable)?
            .map_err(|_| DesktopVisionError::StorageUnavailable)?
            .0;
            config
                .writer
                .append(
                    crate::session::SessionRecord::ArtifactRegistered {
                        artifact: artifact.clone(),
                    },
                    None,
                )
                .await
                .map_err(|_| DesktopVisionError::StorageUnavailable)?;
            receipt.derivative = Some(source(&artifact));
            receipt.usage = VisionUsage {
                input_tokens: prepared.receipt.usage.input_units,
                output_tokens: prepared.receipt.usage.output_units,
                cost_microusd: prepared.receipt.usage.cost_microusd,
            };
            receipt.status = VisionStatus::AnalysisReady;
            persist(&config, &receipt).await?;
            Ok(Work::Submitted {
                receipt,
                input: prepared.model_input,
                owner_input: Some(prepared.owner_input),
                images: Vec::new(),
                cancellation,
                controller_identity: pending.controller_identity,
            })
        }
        result => {
            receipt.status = if revoked {
                VisionStatus::ControllerLost
            } else if cancellation.is_cancelled() {
                VisionStatus::Cancelled
            } else {
                result
                    .as_ref()
                    .err()
                    .map_or(VisionStatus::Cancelled, classify)
            };
            persist(&config, &receipt).await?;
            Ok(Work::Receipt(receipt))
        }
    }
}

fn classify(error: &anyhow::Error) -> VisionStatus {
    if matches!(
        error.downcast_ref::<crate::outbound::OutboundError>(),
        Some(crate::outbound::OutboundError::Denied(_))
    ) {
        return VisionStatus::Denied;
    }
    use crate::focused_service::FocusedServiceError as E;
    match error.downcast_ref::<E>() {
        Some(E::Cancelled) => VisionStatus::Cancelled,
        Some(E::Authentication | E::MissingCredentialReference(_) | E::AdapterUnavailable(_)) => {
            VisionStatus::Unavailable
        }
        _ => VisionStatus::Failed,
    }
}

async fn register_sources(
    config: &Configuration,
    images: &[ImageRef],
) -> Result<(), DesktopVisionError> {
    let store = config
        .artifacts
        .protected_home()
        .ok_or(DesktopVisionError::Unsupported)?;
    for image in images {
        let records = store
            .history_records_for(
                config.conversation,
                crate::storage::HistorySubject::Artifact(image.artifact.reference.id),
            )
            .map_err(|_| DesktopVisionError::StorageUnavailable)?;
        if records.is_empty() {
            config
                .writer
                .append(
                    crate::session::SessionRecord::ArtifactRegistered {
                        artifact: image.artifact.clone(),
                    },
                    None,
                )
                .await
                .map_err(|_| DesktopVisionError::StorageUnavailable)?;
        } else if records.len() != 1
            || !matches!(&records[0].record, crate::session::SessionRecord::ArtifactRegistered { artifact } if artifact == &image.artifact)
        {
            return Err(DesktopVisionError::StorageUnavailable);
        }
    }
    Ok(())
}
