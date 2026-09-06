//! Governed selected-image turns for native Desktop/headless clients.
//! One live owner holds one plan/job. Tokens are not filesystem or replay authority.

mod execution;
mod reader;

#[cfg(test)]
mod tests;

use super::*;
use crate::{
    app::vision::{VisionPlan, VisionTurnRoute, VisionTurnService},
    identity::{PrincipalId, SessionId},
    operation::DurableOperationSender,
    vision::ImageRef,
};
use tokio_util::sync::CancellationToken;

pub use crate::vision::receipt::{
    VisionDestination, VisionReceipt, VisionSource, VisionStatus, VisionUsage,
};

/// One-use review capability. Public display fields must match the issued plan.
#[derive(Clone, PartialEq, Eq)]
pub struct DesktopVisionPlan {
    pub receipt: VisionReceipt,
    pub prompt: String,
    pub approval_required: bool,
    nonce: uuid::Uuid,
    operation: DesktopOperationId,
}

impl DesktopVisionPlan {
    pub fn operation_id(&self) -> DesktopOperationId {
        self.operation
    }
}

impl fmt::Debug for DesktopVisionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DesktopVisionPlan")
            .field("operation", &self.receipt.operation_id)
            .field("approval_required", &self.approval_required)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopVisionDecision {
    AllowOnce,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopVisionError {
    Busy,
    StalePlan,
    WrongOwner,
    Unsupported,
    Unavailable,
    InvalidImage,
    InvalidPrompt,
    Denied,
    Cancelled,
    ControllerLost,
    AnalysisFailed,
    StorageUnavailable,
}

impl fmt::Display for DesktopVisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vision {self:?}")
    }
}
impl std::error::Error for DesktopVisionError {}

/// Vision observations are control/content projections, never diagnostic logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopVisionUpdate {
    Planned {
        command_id: u64,
        plan: DesktopVisionPlan,
    },
    Receipt {
        command_id: u64,
        receipt: VisionReceipt,
    },
    Rejected {
        command_id: u64,
        reason: DesktopVisionError,
    },
}

pub(super) enum Command {
    Plan {
        operation: DesktopOperationId,
        prompt: String,
        attachments: Vec<DesktopAttachment>,
        route: Option<String>,
    },
    Decide {
        plan: Box<DesktopVisionPlan>,
        decision: DesktopVisionDecision,
        acknowledge_collision: bool,
    },
    Cancel {
        operation: DesktopOperationId,
    },
    Inspect {
        operation: DesktopOperationId,
    },
    Stage {
        upload: Upload,
    },
}

pub(super) struct Upload {
    bytes: Vec<u8>,
    media_type: String,
    counter: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for Upload {
    fn drop(&mut self) {
        self.counter
            .fetch_sub(self.bytes.len(), std::sync::atomic::Ordering::AcqRel);
    }
}

// Enqueue failures must not format the payload or image bytes into an error.
impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VisionCommand([REDACTED])")
    }
}

impl DesktopClient {
    /// Validate one bounded PNG/JPEG/GIF byte upload without a client filesystem path.
    pub fn stage_image_bytes(
        &self,
        bytes: Vec<u8>,
        media_type: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        let media_type = media_type.into();
        if bytes.is_empty()
            || bytes.len() > crate::resource::DEFAULT_STATIC_RASTER_BYTES
            || !matches!(
                media_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif"
            )
        {
            return Err(invalid(DesktopVisionError::InvalidImage));
        }
        self.vision_upload_bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| {
                    current
                        .checked_add(bytes.len())
                        .filter(|total| *total <= crate::vision::MAX_IMAGE_BYTES_PER_TURN as usize)
                },
            )
            .map_err(|_| invalid(DesktopVisionError::Busy))?;
        self.enqueue_control(BridgeCommandValue::Vision(Command::Stage {
            upload: Upload {
                bytes,
                media_type,
                counter: self.vision_upload_bytes.clone(),
            },
        }))
    }

    /// Plan native/default or an explicitly selected specialist route, with zero provider I/O.
    pub fn plan_vision_turn(
        &self,
        prompt: impl Into<String>,
        attachments: Vec<DesktopAttachment>,
        route: Option<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty()
            || prompt.len() > crate::focused_service::MAX_FOCUSED_PROMPT_BYTES
        {
            return Err(invalid(DesktopVisionError::InvalidPrompt));
        }
        if attachments.is_empty()
            || attachments.len() > crate::vision::MAX_IMAGES_PER_TURN
            || route.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
            || attachments.iter().any(|item| {
                item.name.len() > 1024
                    || item.id.len() > 128
                    || item.media_type.len() > 128
                    || item.kind.len() > 64
            })
        {
            return Err(invalid(DesktopVisionError::InvalidImage));
        }
        let operation = DesktopOperationId(OperationId::new());
        let command_id = self.enqueue(BridgeCommandValue::Vision(Command::Plan {
            operation,
            prompt,
            attachments,
            route,
        }))?;
        Ok(DesktopCommandReceipt {
            command_id,
            operation_id: Some(operation),
        })
    }

    /// Consume this exact plan once. Approval cannot change its sources or recipient.
    pub fn decide_vision(
        &self,
        plan: DesktopVisionPlan,
        decision: DesktopVisionDecision,
        acknowledge_workspace_write_collision: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        let session = plan
            .receipt
            .conversation_id
            .parse()
            .map_err(|_| invalid(DesktopVisionError::StalePlan))?;
        if !plan.receipt.valid_for(session)
            || plan.prompt.len() > crate::focused_service::MAX_FOCUSED_PROMPT_BYTES
        {
            return Err(invalid(DesktopVisionError::StalePlan));
        }
        self.enqueue_control(BridgeCommandValue::Vision(Command::Decide {
            plan: Box::new(plan),
            decision,
            acknowledge_collision: acknowledge_workspace_write_collision,
        }))
    }

    pub fn cancel_vision(
        &self,
        operation: DesktopOperationId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::Vision(Command::Cancel { operation }))
    }

    /// Inspect an exact receipt through the current Conversation's bounded history reader.
    pub fn inspect_vision_receipt(
        &self,
        operation: &str,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        if operation.len() > 36 {
            return Err(invalid(DesktopVisionError::StalePlan));
        }
        let operation = DesktopOperationId(
            operation
                .parse()
                .map_err(|_| invalid(DesktopVisionError::StalePlan))?,
        );
        self.enqueue_control(BridgeCommandValue::Vision(Command::Inspect { operation }))
    }
}

pub(super) fn invalid(error: DesktopVisionError) -> DesktopError {
    DesktopError::new(DesktopErrorCode::CommandRejected, error.to_string())
}

#[derive(Clone)]
struct Configuration {
    service: VisionTurnService,
    config_file: PathBuf,
    artifacts: crate::artifact::ArtifactStore,
    owner: PrincipalId,
    conversation: SessionId,
    native_images: bool,
    native_destination: VisionDestination,
    writer: DurableOperationSender,
}

struct Pending {
    public: DesktopVisionPlan,
    images: Vec<ImageRef>,
    specialist: Option<VisionPlan>,
    config_digest: String,
    controller_identity: String,
}

pub(super) struct State {
    config: Configuration,
    pending: Option<Pending>,
    job: Option<Job>,
    reserved_operation: Option<OperationId>,
    native_admission: Option<(u64, VisionReceipt)>,
}

struct Job {
    operation: OperationId,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<Finished>,
}

pub(super) struct Finished {
    pub(super) command_id: u64,
    operation: OperationId,
    result: Result<Work, DesktopVisionError>,
}

enum Work {
    Planned(Box<Pending>),
    Submitted {
        receipt: VisionReceipt,
        input: String,
        owner_input: Option<String>,
        images: Vec<ImageRef>,
        cancellation: CancellationToken,
        controller_identity: String,
    },
    Receipt(VisionReceipt),
    Staged(DesktopAttachment),
}

impl State {
    pub(super) fn with_service_certificate(
        &mut self,
        certificate: crate::http_client::ScopedServiceCertificate,
    ) -> anyhow::Result<()> {
        self.config.service = self
            .config
            .service
            .clone()
            .with_service_certificate(certificate)?;
        Ok(())
    }

    pub(super) fn is_busy(&self) -> bool {
        self.job.is_some()
    }
    pub(super) fn revoke_pending(&mut self) {
        self.pending = None;
    }
    pub(super) fn owns_operation(&self, operation: OperationId) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.operation == operation)
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.public.receipt.operation() == Some(operation))
    }
    pub(super) fn new(
        header: &ChatHeader,
        paths: &XanaPaths,
        writer: DurableOperationSender,
    ) -> Result<Self, DesktopError> {
        let native_images = header
            .models
            .descriptor(&header.provider_name, &header.model)
            .is_ok_and(|descriptor| descriptor.input_modalities.contains("image"));
        Ok(Self {
            config: Configuration {
                service: header.vision.clone(),
                config_file: paths.config_file().to_owned(),
                artifacts: header.artifact_store.clone(),
                owner: header.owner,
                conversation: header.session_id,
                native_images,
                native_destination: VisionDestination {
                    route: None,
                    connection: header.provider_name.clone(),
                    model: header.model.clone(),
                    adapter: "native".into(),
                    recipient: header.endpoint.clone(),
                    recipient_digest: blake3::hash(
                        format!(
                            "{}\n{}\n{}",
                            header.provider_name, header.model, header.endpoint
                        )
                        .as_bytes(),
                    )
                    .to_hex()
                    .to_string(),
                },
                writer,
            },
            pending: None,
            job: None,
            reserved_operation: None,
            native_admission: None,
        })
    }

    pub(super) async fn next(&mut self) -> Finished {
        let Some(job) = &mut self.job else {
            return std::future::pending().await;
        };
        let result = (&mut job.task).await.unwrap_or(Finished {
            command_id: 0,
            operation: job.operation,
            result: Err(DesktopVisionError::Unavailable),
        });
        self.job = None;
        result
    }

    pub(super) async fn stop(&mut self) {
        self.pending = None;
        if let Some(job) = self.job.as_ref() {
            job.cancellation.cancel();
        }
        if self.job.is_some() {
            let _ = tokio::time::timeout(Duration::from_secs(5), self.next()).await;
        }
        if let Some(job) = self.job.take() {
            job.task.abort();
            let _ = job.task.await;
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancellation.cancel();
            job.task.abort();
        }
    }
}

pub(super) async fn next(state: &mut Option<State>) -> Finished {
    match state {
        Some(state) => state.next().await,
        None => std::future::pending().await,
    }
}

pub(super) async fn stop(state: &mut Option<State>) {
    if let Some(state) = state {
        state.stop().await;
    }
}
