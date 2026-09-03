//! Repository-private Desktop application boundary.
//!
//! This module is public only so the `xana-desktop` workspace member can use
//! it. It is not a stable SDK. Presentation code receives bounded projections
//! and typed intent; provider adapters, credentials, tools, paths, and runtime
//! ownership stay in this package.

mod attached;
mod content;
mod conversation;
mod instance;
mod layout;
mod managed;
mod management;
mod navigation;
mod settings;

pub use content::{
    DesktopArtifactReader, DesktopCapabilitySource, DesktopContent, DesktopContentAction,
    DesktopContentTier, DesktopContentValue, DesktopImagePreview, DesktopMessage, DesktopResource,
    DesktopResourceAvailability, DesktopResourceCapability, DesktopResourceKind,
    DesktopResourceLineage, DesktopResourceMetadata, DesktopResourceOperation,
    DesktopResourceValidation, DesktopRole,
};
pub use conversation::{
    DesktopActivityDisclosure, DesktopActivityItem, DesktopActivityOwner, DesktopActivityState,
    DesktopAvailability, DesktopCompletionCheck, DesktopCompletionReceipt,
    DesktopConversationFacts, DesktopExecutionFact, DesktopFactAuthority, DesktopFactFreshness,
    DesktopFactSource, DesktopPromptLedger, DesktopRunCapability, DesktopUsageFact,
};

pub use instance::{
    DesktopInstanceClaim, DesktopInstanceLease, DesktopLaunchIntent, DesktopNativePaths,
    DesktopNavigationTarget,
};
pub use layout::{
    DesktopDockPlacement, DesktopLayoutNode, DesktopLayoutSource, DesktopPanelId,
    DesktopResolvedLayout, DesktopSplitAxis, DesktopWorkbenchLayout,
};
pub use management::{
    DesktopCapabilityFact, DesktopCapabilitySnapshot, DesktopConnection,
    DesktopConnectionMutationReceipt, DesktopConnectionOperationReceipt,
    DesktopConnectionRemovalPlan, DesktopConnectionSnapshot, DesktopControlPlane,
    DesktopCredentialInput, DesktopCredentialState, DesktopDiagnosticEntry,
    DesktopDiagnosticsSnapshot, DesktopDoctorFinding, DesktopDoctorRepairReceipt,
    DesktopDoctorRepairResult, DesktopDoctorSeverity, DesktopDoctorSnapshot,
    DesktopEntityMutationReceipt, DesktopExecutionKind, DesktopManagedLogin,
    DesktopManagementSnapshot, DesktopMigrationReceipt, DesktopMigrationSnapshot,
    DesktopModelOption, DesktopPermissionDecision, DesktopPermissionEffect, DesktopPermissionMode,
    DesktopPermissionPreview, DesktopPermissionRuleDraft, DesktopPermissionRuleSummary,
    DesktopPermissionSnapshot, DesktopPrivateMigrationRecord, DesktopProfileDraft,
    DesktopProfileSummary, DesktopProjectDraft, DesktopProjectSummary, DesktopProviderKind,
    DesktopResetPlan, DesktopResetReceipt, DesktopResetScope, DesktopResetTarget,
    DesktopResourceLimit, DesktopResourcePolicyDraft, DesktopResourcePolicyPreview,
    DesktopResourcePolicySnapshot, DesktopSecret, DesktopSetupDraft, DesktopSetupMode,
    DesktopSetupReceipt, DesktopSetupSnapshot, DesktopWorkbenchPreferenceSnapshot,
};
pub use navigation::{
    DesktopConversationNode, DesktopLaunchCatalog, DesktopLaunchChoice, DesktopLaunchChoiceKind,
    DesktopNavigationConversationState, DesktopNavigationSnapshot, DesktopProjectNode,
    DesktopSidebarMode, DesktopWorkspaceStatus,
};
pub use settings::{
    DesktopLocalizedText, DesktopSettingChange, DesktopSettingEffect, DesktopSettingEntry,
    DesktopSettingKind, DesktopSettingSource, DesktopSettingTarget, DesktopSettingValue,
    DesktopSettingsBackup, DesktopSettingsDraftId, DesktopSettingsDraftSnapshot,
    DesktopSettingsOwner, DesktopSettingsReceipt, DesktopSettingsSection, DesktopSettingsSnapshot,
};

pub(crate) use managed::run_managed;

pub use crate::host_lifecycle::{
    AttentionKind, AttentionSignal, ClientFocus, GlobalNotice, GlobalNoticeKind, LastWindowChoice,
    LastWindowEffect, NotificationCandidate, NotificationDestination, NotificationPlanner,
    NotificationPolicy, last_window_effect,
};
use crate::{
    app::{ChatExit, ChatHeader},
    command_catalog::{
        self, AuthorityRequirement, CommandContext, CommandSurface, PresentationCapabilities,
    },
    controller::ControllerClientId,
    execution_host::{
        ConversationRegistration, ExecutionHost, HostChanges, HostEvent, HostedRun, RunAccess,
        WriteCollisionDecision,
    },
    frontend::{
        ClientCommand, ClientEvent, ClientObservation, ClientSnapshot, ClientSnapshotSeed,
        EmbeddedClient, FRONTEND_PROTOCOL_VERSION,
    },
    identity::{OperationId, RoundBudgetId, ToolInvocationId},
    message::{Message, Role},
    native_runtime::{
        AgentEvent, OperationOutcome, OperationState, RoundBudgetAction, RuntimeCommand,
        RuntimeHandle,
    },
    paths::XanaPaths,
    permission::{ControllerDecision, PermissionRequest, PermissionScope},
    vision::{ImageAttachment, ImageIngestor, ImageLimits},
    workspace_host::{ConversationRef, WorkspaceHost},
};
use std::{
    ffi::OsString,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc as std_mpsc},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

/// The repository-private Desktop protocol version.
pub const PROTOCOL_VERSION: u16 = FRONTEND_PROTOCOL_VERSION;

const COMMAND_CAPACITY: usize = 32;
const UPDATE_CAPACITY: usize = 256;
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CRITICAL_UPDATE_GRACE: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PUBLIC_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
struct DesktopController {
    conversation: ConversationRef,
    client_id: ControllerClientId,
}

struct NativeCommandContext<'a> {
    owner: &'a crate::frontend::EmbeddedOwner,
    execution_host: &'a ExecutionHost,
    controller: &'a DesktopController,
    snapshot: &'a ClientSnapshot,
    active_run: &'a mut Option<HostedRun>,
    navigation: &'a mut DesktopNavigationSnapshot,
    navigation_store: &'a navigation::DesktopNavigationStore,
    layout: &'a mut DesktopResolvedLayout,
    layout_store: &'a layout::DesktopLayoutStore,
    settings: &'a mut settings::DesktopSettingsState,
    attachments: &'a DesktopAttachmentService,
}

struct DesktopFrontendState {
    navigation: DesktopNavigationSnapshot,
    navigation_store: navigation::DesktopNavigationStore,
    layout: DesktopResolvedLayout,
    layout_store: layout::DesktopLayoutStore,
    settings: settings::DesktopSettingsState,
    attachments: DesktopAttachmentService,
}

#[derive(Clone)]
struct DesktopAttachmentService {
    workspace: PathBuf,
    store: crate::artifact::ArtifactStore,
    ingestor: ImageIngestor,
    resource_policy: crate::resource::ResourcePolicyV1,
    owner: crate::identity::PrincipalId,
}

impl DesktopAttachmentService {
    fn stage_path(
        &self,
        source_path: &str,
        external_approved: bool,
    ) -> Result<DesktopAttachment, DesktopError> {
        if is_provider_image_path(source_path) {
            let attachment = if external_approved {
                self.ingestor
                    .ingest_approved_dropped_path(&self.workspace, source_path, self.owner)
            } else {
                self.ingestor
                    .ingest_dropped_path(&self.workspace, source_path, self.owner)
            }
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::StateInvalid,
                    format!("could not stage image attachment: {error}"),
                )
            })?;
            return Ok(DesktopAttachment::from_image(attachment));
        }

        let ingestor = crate::resource::inspection::ResourceIngestor::new(
            self.store.clone(),
            self.resource_policy.clone(),
        )
        .map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("could not initialize resource validation: {error}"),
            )
        })?;
        let resource = if external_approved {
            ingestor.ingest_approved_path(&self.workspace, source_path, self.owner)
        } else {
            ingestor.ingest_path(&self.workspace, source_path, self.owner)
        }
        .map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("could not stage resource attachment: {error}"),
            )
        })?;
        Ok(DesktopAttachment::from_resource(resource))
    }

    fn stage_clipboard(&self) -> Result<DesktopAttachment, DesktopError> {
        crate::vision::ingest_clipboard_image(self.store.clone(), self.owner)
            .map(DesktopAttachment::from_image)
            .map_err(|error| DesktopError::new(DesktopErrorCode::StateInvalid, error))
    }

    fn save_artifact(
        &self,
        artifact: &crate::artifact::ArtifactRecord,
        destination: &std::path::Path,
    ) -> Result<(), DesktopError> {
        if !destination.is_absolute() || destination.file_name().is_none() {
            return Err(DesktopError::new(
                DesktopErrorCode::StateInvalid,
                "artifact export requires an absolute file destination",
            ));
        }
        self.store
            .copy_verified_create_new(
                artifact,
                destination,
                crate::resource::MAX_RESOURCE_SOURCE_BYTES,
            )
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::StateInvalid,
                    format!("could not save verified artifact copy: {error}"),
                )
            })
    }

    fn launch_artifact(
        &self,
        artifact: &crate::artifact::ArtifactRecord,
        action: crate::artifact_action::ExternalArtifactAction,
    ) -> Result<(), DesktopError> {
        crate::artifact_action::launch_verified(
            &self.store,
            artifact,
            crate::resource::MAX_RESOURCE_SOURCE_BYTES,
            action,
        )
        .map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("could not launch verified artifact action: {error}"),
            )
        })
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

fn artifact_record_for_id(
    snapshot: &ClientSnapshot,
    artifact_id: &str,
) -> Result<crate::artifact::ArtifactRecord, DesktopError> {
    snapshot
        .conversation
        .iter()
        .rev()
        .flat_map(|message| message.content.iter().rev())
        .find_map(|block| match block {
            crate::message::ContentBlock::Image(image)
                if image.artifact.reference.id.to_string() == artifact_id =>
            {
                Some(image.artifact.clone())
            }
            _ => None,
        })
        .ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("artifact {artifact_id} is not present in the bounded Conversation"),
            )
        })
}

enum DesktopArtifactCommand {
    Save(PathBuf),
    Reveal,
    Open,
}

async fn perform_artifact_command(
    service: DesktopAttachmentService,
    snapshot: &ClientSnapshot,
    artifact_id: &str,
    command: DesktopArtifactCommand,
) -> Result<(), DesktopError> {
    let artifact = artifact_record_for_id(snapshot, artifact_id)?;
    tokio::task::spawn_blocking(move || match command {
        DesktopArtifactCommand::Save(destination) => service.save_artifact(&artifact, &destination),
        DesktopArtifactCommand::Reveal => service.launch_artifact(
            &artifact,
            crate::artifact_action::ExternalArtifactAction::Reveal,
        ),
        DesktopArtifactCommand::Open => service.launch_artifact(
            &artifact,
            crate::artifact_action::ExternalArtifactAction::Open,
        ),
    })
    .await
    .map_err(|error| {
        DesktopError::new(
            DesktopErrorCode::RuntimeCrashed,
            format!("Desktop artifact worker stopped: {error}"),
        )
    })?
}

/// Authority held by one Desktop frontend attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAuthority {
    Observer,
    Controller,
    Owner,
}

impl From<DesktopAuthority> for AuthorityRequirement {
    fn from(value: DesktopAuthority) -> Self {
        match value {
            DesktopAuthority::Observer => Self::Observer,
            DesktopAuthority::Controller => Self::Controller,
            DesktopAuthority::Owner => Self::Owner,
        }
    }
}

/// Presentation-safe projection of one shared semantic command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopCommandDescriptor {
    pub id: &'static str,
    pub family: &'static str,
    pub aliases: &'static [&'static str],
    pub mode: &'static str,
    pub summary: &'static str,
    pub available: bool,
    pub availability_code: &'static str,
    pub unavailable_reason: Option<&'static str>,
}

/// Projects the application command catalog without granting runtime authority.
pub fn desktop_commands(
    authority: DesktopAuthority,
    configured: bool,
) -> Vec<DesktopCommandDescriptor> {
    let context = CommandContext {
        surface: CommandSurface::Desktop,
        authority: authority.into(),
        interactive: true,
        configured,
    };
    command_catalog::commands_for(CommandSurface::Desktop)
        .map(|command| project_desktop_command(command, context))
        .collect()
}

/// Resolves one stable ID for menus, buttons, and compatibility handling.
pub fn desktop_command(
    stable_id: &str,
    authority: DesktopAuthority,
    configured: bool,
) -> Option<DesktopCommandDescriptor> {
    command_catalog::COMMANDS
        .iter()
        .copied()
        .find(|command| {
            command.stable_id == stable_id && command.surfaces.contains(CommandSurface::Desktop)
        })
        .map(|command| {
            project_desktop_command(
                command,
                CommandContext {
                    surface: CommandSurface::Desktop,
                    authority: authority.into(),
                    interactive: true,
                    configured,
                },
            )
        })
}

/// Desktop's presentation contract. It is not provider or tool authority.
pub fn desktop_presentation_capabilities() -> DesktopPresentationCapabilities {
    PresentationCapabilities::desktop().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopPresentationCapabilities {
    pub color: bool,
    pub unicode: bool,
    pub dimensions: bool,
    pub pointer: bool,
    pub clipboard: bool,
    pub inline_images: bool,
    pub inline_audio_video: bool,
    pub safe_link_open: bool,
    pub notifications: bool,
    pub native_accessibility: bool,
    pub rich_markdown: bool,
    pub math: bool,
    pub composable_layout: bool,
}

impl From<PresentationCapabilities> for DesktopPresentationCapabilities {
    fn from(value: PresentationCapabilities) -> Self {
        Self {
            color: value.color != command_catalog::ColorCapability::None,
            unicode: value.unicode,
            dimensions: value.dimensions,
            pointer: value.pointer,
            clipboard: value.clipboard,
            inline_images: value.inline_images,
            inline_audio_video: value.inline_audio_video,
            safe_link_open: value.safe_link_open,
            notifications: value.notifications,
            native_accessibility: value.accessibility
                == command_catalog::AccessibilityCapability::NativeSemanticTree,
            rich_markdown: value.rich_markdown,
            math: value.math,
            composable_layout: value.composable_layout,
        }
    }
}

fn project_desktop_command(
    command: command_catalog::CommandSpec,
    context: CommandContext,
) -> DesktopCommandDescriptor {
    let availability = command.availability(context);
    DesktopCommandDescriptor {
        id: command.stable_id,
        family: command.name,
        aliases: command.aliases,
        mode: command.mode,
        summary: command.summary,
        available: availability.enabled,
        availability_code: availability_label(availability.code),
        unavailable_reason: availability.reason,
    }
}

fn availability_label(code: command_catalog::AvailabilityCode) -> &'static str {
    use command_catalog::AvailabilityCode;
    match code {
        AvailabilityCode::Available => "available",
        AvailabilityCode::UnsupportedSurface => "unsupported_surface",
        AvailabilityCode::AuthorityRequired => "authority_required",
        AvailabilityCode::InteractiveInputRequired => "interactive_input_required",
        AvailabilityCode::NoninteractiveOnly => "noninteractive_only",
        AvailabilityCode::SetupRequired => "setup_required",
        AvailabilityCode::NotImplemented => "not_implemented",
        AvailabilityCode::UnknownCommand => "unknown_command",
    }
}

type StartupSender = std_mpsc::SyncSender<Result<DesktopSnapshot, DesktopError>>;

/// Owned process-edge inputs used to start the bundled Desktop runtime.
#[derive(Debug, Clone)]
pub struct DesktopLaunch {
    workspace: Option<PathBuf>,
    conversation: Option<ConversationRef>,
    force_new: bool,
    xana_home: Option<OsString>,
}

impl DesktopLaunch {
    /// Captures the current workspace and optional `XANA_HOME` value once.
    pub fn from_process() -> Result<Self, DesktopError> {
        let workspace = std::env::current_dir().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!("could not resolve the Desktop workspace: {error}"),
            )
        })?;
        Ok(Self {
            workspace: Some(workspace),
            conversation: None,
            force_new: false,
            xana_home: std::env::var_os("XANA_HOME"),
        })
    }

    /// Creates a cold icon-style launch that owns no workspace authority yet.
    pub fn for_launcher_from_process() -> Self {
        Self {
            workspace: None,
            conversation: None,
            force_new: false,
            xana_home: std::env::var_os("XANA_HOME"),
        }
    }

    /// Creates an explicit launch without reading process-global workspace state.
    pub fn new(workspace: impl Into<PathBuf>, xana_home: Option<OsString>) -> Self {
        Self {
            workspace: Some(workspace.into()),
            conversation: None,
            force_new: false,
            xana_home,
        }
    }

    /// Reads the bounded launch catalog without starting a workspace runtime.
    pub fn launch_catalog(&self) -> Result<DesktopLaunchCatalog, DesktopError> {
        let paths = XanaPaths::resolve(self.xana_home.clone()).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::ConfigurationUnavailable,
                format!("could not resolve Xana paths: {error}"),
            )
        })?;
        navigation::DesktopNavigationStore::launch_catalog(&paths)
    }

    /// Opens the typed setup/settings control plane without granting workspace authority.
    pub fn control_plane(&self) -> Result<DesktopControlPlane, DesktopError> {
        DesktopControlPlane::resolve(self.xana_home.clone())
    }

    /// Selects one catalog target without exposing its stored filesystem path.
    pub fn with_choice(&self, choice: &DesktopLaunchChoice) -> Self {
        Self {
            workspace: Some(choice.target.workspace.clone()),
            conversation: choice.target.conversation.clone(),
            force_new: choice.target.force_new,
            xana_home: self.xana_home.clone(),
        }
    }

    /// Selects an explicit folder after a native folder-picker result.
    pub fn with_workspace(&self, workspace: impl Into<PathBuf>, force_new: bool) -> Self {
        Self {
            workspace: Some(workspace.into()),
            conversation: None,
            force_new,
            xana_home: self.xana_home.clone(),
        }
    }
}

/// Stable semantic failure categories crossing the Desktop boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopErrorCode {
    WorkspaceUnavailable,
    ConfigurationUnavailable,
    InstanceUnavailable,
    StateInvalid,
    HostBusy,
    AuthorityRequired,
    ProtocolMismatch,
    CommandRejected,
    RuntimeUnavailable,
    RuntimeCrashed,
    ObserverStalled,
    UnsupportedExecutionOwner,
}

impl DesktopErrorCode {
    /// Stable machine-readable code for logging and future transport adapters.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceUnavailable => "workspace_unavailable",
            Self::ConfigurationUnavailable => "configuration_unavailable",
            Self::InstanceUnavailable => "instance_unavailable",
            Self::StateInvalid => "state_invalid",
            Self::HostBusy => "host_busy",
            Self::AuthorityRequired => "authority_required",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::CommandRejected => "command_rejected",
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::RuntimeCrashed => "runtime_crashed",
            Self::ObserverStalled => "observer_stalled",
            Self::UnsupportedExecutionOwner => "unsupported_execution_owner",
        }
    }
}

/// Redacted, user-presentable Desktop boundary error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopError {
    pub code: DesktopErrorCode,
    pub message: String,
}

impl DesktopError {
    fn new(code: DesktopErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded_text(message.into(), MAX_PUBLIC_TEXT_BYTES),
        }
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for DesktopError {}

/// Opaque runtime operation identity retained by the Desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopOperationId(OperationId);

impl DesktopOperationId {
    /// Creates an application-owned correlation id.
    pub fn new() -> Self {
        Self(OperationId::new())
    }
}

impl Default for DesktopOperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DesktopOperationId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(output)
    }
}

/// Opaque permission correlation retained by the Desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopPermissionId(DesktopPermissionTarget);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DesktopPermissionTarget {
    Native {
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    },
    Managed {
        operation_id: OperationId,
        request_id: u64,
    },
    AttachedManaged {
        operation_id: OperationId,
        approval_id: uuid::Uuid,
    },
}

impl DesktopPermissionId {
    fn native(operation_id: OperationId, invocation_id: ToolInvocationId) -> Self {
        Self(DesktopPermissionTarget::Native {
            operation_id,
            invocation_id,
        })
    }

    fn managed(operation_id: OperationId, request_id: u64) -> Self {
        Self(DesktopPermissionTarget::Managed {
            operation_id,
            request_id,
        })
    }

    fn attached_managed(operation_id: OperationId, approval_id: uuid::Uuid) -> Self {
        Self(DesktopPermissionTarget::AttachedManaged {
            operation_id,
            approval_id,
        })
    }

    fn operation_id(self) -> OperationId {
        match self.0 {
            DesktopPermissionTarget::Native { operation_id, .. }
            | DesktopPermissionTarget::Managed { operation_id, .. }
            | DesktopPermissionTarget::AttachedManaged { operation_id, .. } => operation_id,
        }
    }
}

impl fmt::Display for DesktopPermissionId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            DesktopPermissionTarget::Native {
                operation_id,
                invocation_id,
            } => write!(output, "native:{operation_id}:{invocation_id}"),
            DesktopPermissionTarget::Managed {
                operation_id,
                request_id,
            } => write!(output, "managed:{operation_id}:{request_id}"),
            DesktopPermissionTarget::AttachedManaged {
                operation_id,
                approval_id,
            } => write!(output, "attached-managed:{operation_id}:{approval_id}"),
        }
    }
}

/// Opaque identity for one exact round-budget suspension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DesktopRoundBudgetId(RoundBudgetId);

/// Bounded committed facts shown when a native turn needs an explicit choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRoundBudgetSuspension {
    pub id: DesktopRoundBudgetId,
    pub operation_id: DesktopOperationId,
    pub rounds_consumed: u32,
    pub hard_round_limit: u32,
    pub remaining_rounds: u32,
    pub committed_steps: u32,
    pub committed_invocations: u32,
    pub committed_results: u32,
    pub repeated_tool_patterns: u32,
    pub can_continue: bool,
}

/// An immutable resource staged by Xana for one Desktop draft.
///
/// The filesystem path and artifact integrity record remain private to the
/// runtime package. Desktop presentation code can retain and return this
/// capability token, but cannot use it to read arbitrary files.
#[derive(Clone, PartialEq, Eq)]
pub struct DesktopAttachment {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub byte_len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kind: String,
    pub provider_input_available: bool,
    payload: DesktopAttachmentPayload,
}

#[derive(Clone, PartialEq, Eq)]
enum DesktopAttachmentPayload {
    Image(ImageAttachment),
    Resource(Box<crate::resource::ResourceRefV1>),
}

impl fmt::Debug for DesktopAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopAttachment")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("media_type", &self.media_type)
            .field("byte_len", &self.byte_len)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("kind", &self.kind)
            .field("provider_input_available", &self.provider_input_available)
            .finish_non_exhaustive()
    }
}

impl DesktopAttachment {
    fn from_image(image: ImageAttachment) -> Self {
        let name = std::path::Path::new(&image.source_path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| !name.is_empty())
            .unwrap_or("image")
            .to_owned();
        Self {
            id: image.image.artifact.reference.id.to_string(),
            name,
            media_type: image.image.media_type.clone(),
            byte_len: image.image.byte_len,
            width: image.image.width,
            height: image.image.height,
            kind: "static_raster".to_owned(),
            provider_input_available: true,
            payload: DesktopAttachmentPayload::Image(image),
        }
    }

    fn from_resource(resource: crate::resource::inspection::IngestedResource) -> Self {
        let media_type = resource
            .resource
            .media_type
            .detected
            .clone()
            .or_else(|| resource.resource.media_type.declared.clone())
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        Self {
            id: resource.resource.artifact.reference.id.to_string(),
            name: resource.source_label,
            media_type,
            byte_len: resource.resource.artifact.byte_len,
            width: resource.resource.metadata.width,
            height: resource.resource.metadata.height,
            kind: resource.resource.kind.code().to_owned(),
            provider_input_available: false,
            payload: DesktopAttachmentPayload::Resource(Box::new(resource.resource)),
        }
    }

    fn into_image(self) -> Result<crate::vision::ImageRef, DesktopError> {
        match self.payload {
            DesktopAttachmentPayload::Image(image) => Ok(image.image),
            DesktopAttachmentPayload::Resource(resource) => Err(DesktopError::new(
                DesktopErrorCode::CommandRejected,
                format!(
                    "{} resource {} is retained, but the selected route does not support it as turn input",
                    resource.kind.code(),
                    resource.artifact.reference.id
                ),
            )),
        }
    }

    fn into_managed_image(self) -> Result<ImageAttachment, DesktopError> {
        match self.payload {
            DesktopAttachmentPayload::Image(image) => Ok(image),
            DesktopAttachmentPayload::Resource(resource) => Err(DesktopError::new(
                DesktopErrorCode::CommandRejected,
                format!(
                    "{} resource {} is retained, but Codex input support was not established for it",
                    resource.kind.code(),
                    resource.artifact.reference.id
                ),
            )),
        }
    }
}

/// One accepted application-side command enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopCommandReceipt {
    pub command_id: u64,
    pub operation_id: Option<DesktopOperationId>,
}

/// A bounded atomic projection of current runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSnapshot {
    pub version: u16,
    pub sequence: u64,
    pub authority: DesktopAuthority,
    pub attached_to_foreground_host: bool,
    pub session_id: String,
    pub connection: String,
    pub execution_owner: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub notification_policy: NotificationPolicy,
    pub conversation: Vec<DesktopMessage>,
    pub conversation_truncated: bool,
    pub active_operation: Option<DesktopOperationId>,
    pub pending_approval_count: usize,
    pub pending_approvals: Vec<DesktopPendingApproval>,
    pub activity_count: usize,
    pub artifact_count: usize,
    pub host_sequence: u64,
    pub hosted_workspace_count: usize,
    pub hosted_conversation_count: usize,
    pub hosted_conversations: Vec<DesktopHostedConversation>,
    pub attached_conversation: Option<String>,
    pub controllers: Vec<DesktopControllerLease>,
    pub host_lifecycle: String,
    pub global_notices: Vec<DesktopGlobalNotice>,
    pub navigation: DesktopNavigationSnapshot,
    pub layout: DesktopResolvedLayout,
    pub settings: DesktopSettingsSnapshot,
    pub conversation_facts: DesktopConversationFacts,
}

/// One presentation-safe approval gate that survives projection resync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPendingApproval {
    pub id: DesktopPermissionId,
    pub tool: String,
    pub effect: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopGlobalNotice {
    pub kind: String,
    pub code: String,
    pub conversation: Option<String>,
    pub operation_id: Option<DesktopOperationId>,
}

/// Presentation-safe authority state for one hosted Conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopControllerLease {
    pub conversation: String,
    pub controller_id: String,
    pub generation: u64,
    pub state: String,
    pub takeover_confirmed: bool,
    pub reconnect_grace_remaining_ms: Option<u64>,
    pub disconnect_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOperationState {
    Running,
    Suspended,
    Completed,
    Failed,
    Declined,
    Interrupted,
}

/// One ordered, bounded runtime observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopObservation {
    pub version: u16,
    pub sequence: u64,
    pub event: DesktopEvent,
}

/// One ordered host-coordination observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHostObservation {
    pub sequence: u64,
    pub event: DesktopHostEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopConversationState {
    Idle,
    Running,
    Suspended,
    Completed,
    Failed,
    Declined,
    Interrupted,
}

impl DesktopConversationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Suspended => "suspended",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Declined => "declined",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Bounded host-owned work state for one Conversation in global projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHostedConversation {
    pub conversation: String,
    pub workspace_id: String,
    pub connection: String,
    pub model: String,
    pub profile: Option<String>,
    pub permission_mode: String,
    pub state: DesktopConversationState,
    pub active_operation: Option<DesktopOperationId>,
    pub pending_approvals: usize,
    pub activity_count: usize,
    pub last_outcome: Option<String>,
    pub controller: Option<DesktopControllerLease>,
}

/// Stable host-routing facts, separate from one Conversation's runtime events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopHostEvent {
    ConversationRegistered {
        conversation: String,
        workspace_id: String,
    },
    RunStarted {
        conversation: String,
        operation_id: DesktopOperationId,
        workspace_write: bool,
        collision_acknowledged: bool,
    },
    RuntimeObservation {
        conversation: String,
    },
    RunFinished {
        conversation: String,
        operation_id: DesktopOperationId,
        state: DesktopConversationState,
        error: Option<String>,
    },
    ConversationAttached {
        previous: Option<String>,
        conversation: String,
        restoration: String,
    },
    ControllerChanged {
        conversation: String,
        controller: Option<DesktopControllerLease>,
        change: String,
    },
    GlobalNotice(DesktopGlobalNotice),
    LifecycleChanged {
        state: String,
    },
    ShutdownCompleted {
        interrupted_runs: usize,
        cleanup: String,
    },
}

/// Minimal semantic vocabulary used by the M4 walking skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    OperationState {
        operation_id: DesktopOperationId,
        state: DesktopOperationState,
    },
    AssistantDelta {
        operation_id: DesktopOperationId,
        text: String,
    },
    ReasoningDelta {
        operation_id: DesktopOperationId,
        text: String,
    },
    /// A managed owner accepted a later-turn model or reasoning selection.
    ExecutionSelectionChanged {
        model: String,
        reasoning_effort: Option<String>,
        receipt: String,
    },
    MessageFinal {
        operation_id: DesktopOperationId,
        message: DesktopMessage,
    },
    PermissionRequired {
        permission_id: DesktopPermissionId,
        tool: String,
        effect: String,
        scope: String,
    },
    PermissionResolved {
        permission_id: DesktopPermissionId,
    },
    RoundBudgetReached(DesktopRoundBudgetSuspension),
    RoundBudgetDecision {
        suspension_id: DesktopRoundBudgetId,
        continued: bool,
    },
    Usage {
        operation_id: DesktopOperationId,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        requests: u64,
    },
    ConversationCleared,
    Activity {
        label: String,
    },
    ActivityUpserted(DesktopActivityItem),
    Error(DesktopError),
}

impl DesktopEvent {
    fn replaceable(&self) -> bool {
        matches!(
            self,
            Self::AssistantDelta { .. } | Self::ReasoningDelta { .. }
        )
    }
}

/// Updates delivered to one Desktop projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopUpdate {
    Snapshot(Box<DesktopSnapshot>),
    Navigation(DesktopNavigationSnapshot),
    Layout(Box<DesktopResolvedLayout>),
    Settings(DesktopSettingsSnapshot),
    SettingsDraft(Option<DesktopSettingsDraftSnapshot>),
    SettingsReceipt(DesktopSettingsReceipt),
    AttachmentStaged {
        command_id: u64,
        attachment: DesktopAttachment,
    },
    Observation(DesktopObservation),
    HostObservation(DesktopHostObservation),
    CommandResult {
        command_id: u64,
        accepted: bool,
        error: Option<DesktopError>,
    },
    ResyncRequired {
        expected_sequence: u64,
        received_sequence: u64,
    },
    BackendStopped {
        expected: bool,
        error: Option<DesktopError>,
    },
}

/// The Desktop-side controller and bounded observation receiver.
pub struct DesktopClient {
    commands: mpsc::Sender<BridgeCommand>,
    updates: mpsc::Receiver<DesktopUpdate>,
    update_signal: DesktopWakeSignal,
    next_command_id: std::sync::atomic::AtomicU64,
    backend: Option<thread::JoinHandle<()>>,
    backend_done: std_mpsc::Receiver<()>,
    initial_snapshot: DesktopSnapshot,
    artifacts: DesktopArtifactReader,
}

/// A coalescing, executor-independent wake signal for newly queued Desktop work.
///
/// The signal carries no data or authority. Consumers must still drain the
/// bounded typed channel and validate every update.
#[derive(Clone, Default)]
pub struct DesktopWakeSignal(Arc<tokio::sync::Notify>);

impl DesktopWakeSignal {
    pub(crate) fn wake(&self) {
        self.0.notify_one();
    }

    /// Waits until a producer reports that typed Desktop work may be available.
    pub async fn wait(&self) {
        self.0.notified().await;
    }
}

impl DesktopClient {
    /// Starts Xana's matching runtime inside this process.
    pub fn launch(launch: DesktopLaunch) -> Result<Self, DesktopError> {
        let workspace = launch.workspace.ok_or_else(|| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                "Desktop launch requires an explicit workspace selection",
            )
        })?;
        let workspace = workspace.canonicalize().map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::WorkspaceUnavailable,
                format!(
                    "could not canonicalize Desktop workspace {}: {error}",
                    workspace.display()
                ),
            )
        })?;
        let conversation = launch.conversation;
        let force_new = launch.force_new;
        let paths = XanaPaths::resolve(launch.xana_home).map_err(|error| {
            DesktopError::new(
                DesktopErrorCode::ConfigurationUnavailable,
                format!("could not resolve Xana paths: {error}"),
            )
        })?;
        let artifacts = DesktopArtifactReader::new(crate::artifact::ArtifactStore::new(
            paths.data_dir().join("artifacts"),
        ));
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (updates, update_receiver) = mpsc::channel(UPDATE_CAPACITY);
        let update_signal = DesktopWakeSignal::default();
        let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
        let startup = StartupSignal::new(startup_sender);
        let (done_sender, done_receiver) = std_mpsc::sync_channel(1);
        let bridge = Bridge {
            commands: Arc::new(tokio::sync::Mutex::new(command_receiver)),
            updates: updates.clone(),
            update_signal: update_signal.clone(),
            startup: startup.clone(),
            notification_policy: NotificationPolicy::default(),
        };
        let failure_updates = updates.clone();
        let failure_signal = update_signal.clone();
        let backend = thread::Builder::new()
            .name("xana-desktop-runtime".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_backend(paths, workspace, conversation, force_new, bridge)
                }));
                match result {
                    Ok(Ok(())) => {
                        startup.fail_if_pending(DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "Desktop runtime stopped before publishing its initial snapshot",
                        ));
                    }
                    Ok(Err(error)) => {
                        let error = classify_backend_error(&error);
                        startup.fail_if_pending(error.clone());
                        if failure_updates
                            .blocking_send(DesktopUpdate::BackendStopped {
                                expected: false,
                                error: Some(error),
                            })
                            .is_ok()
                        {
                            failure_signal.wake();
                        }
                    }
                    Err(_) => {
                        let error = DesktopError::new(
                            DesktopErrorCode::RuntimeCrashed,
                            "Desktop runtime thread panicked; durable state remains recoverable",
                        );
                        startup.fail_if_pending(error.clone());
                        if failure_updates
                            .blocking_send(DesktopUpdate::BackendStopped {
                                expected: false,
                                error: Some(error),
                            })
                            .is_ok()
                        {
                            failure_signal.wake();
                        }
                    }
                }
                let _ = done_sender.send(());
            })
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("could not create the Desktop runtime thread: {error}"),
                )
            })?;
        let initial_snapshot =
            startup_receiver
                .recv_timeout(START_TIMEOUT)
                .map_err(|error| {
                    DesktopError::new(
                        DesktopErrorCode::RuntimeUnavailable,
                        format!("Desktop runtime did not become ready within 30 seconds: {error}"),
                    )
                })??;
        if initial_snapshot.version != PROTOCOL_VERSION {
            return Err(DesktopError::new(
                DesktopErrorCode::ProtocolMismatch,
                format!(
                    "Desktop protocol {} does not match runtime protocol {}",
                    PROTOCOL_VERSION, initial_snapshot.version
                ),
            ));
        }
        Ok(Self {
            commands,
            updates: update_receiver,
            update_signal,
            next_command_id: std::sync::atomic::AtomicU64::new(1),
            backend: Some(backend),
            backend_done: done_receiver,
            initial_snapshot,
            artifacts,
        })
    }

    pub fn initial_snapshot(&self) -> &DesktopSnapshot {
        &self.initial_snapshot
    }

    /// Returns a coalescing signal used to wake a presentation only when the
    /// runtime may have queued updates.
    pub fn update_signal(&self) -> DesktopWakeSignal {
        self.update_signal.clone()
    }

    /// Returns a bounded reader that accepts only runtime-projected resources.
    pub fn artifact_reader(&self) -> DesktopArtifactReader {
        self.artifacts.clone()
    }

    /// Saves a verified copy to an explicit path selected by the native UI.
    pub fn save_artifact(
        &self,
        artifact_id: impl Into<String>,
        destination: impl Into<PathBuf>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SaveArtifact {
            artifact_id: artifact_id.into(),
            destination: destination.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Reveals one verified artifact in the platform file manager.
    pub fn reveal_artifact(
        &self,
        artifact_id: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::RevealArtifact {
            artifact_id: artifact_id.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Opens one verified artifact with the platform default application.
    pub fn open_artifact(
        &self,
        artifact_id: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::OpenArtifact {
            artifact_id: artifact_id.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    pub fn submit(&self, input: impl Into<String>) -> Result<DesktopCommandReceipt, DesktopError> {
        self.submit_with_attachments_and_workspace_collision_acknowledgement(
            input,
            Vec::new(),
            false,
        )
    }

    /// Submits one turn with immutable attachments previously staged by Xana.
    pub fn submit_with_attachments(
        &self,
        input: impl Into<String>,
        attachments: Vec<DesktopAttachment>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.submit_with_attachments_and_workspace_collision_acknowledgement(
            input,
            attachments,
            false,
        )
    }

    /// Submits a turn while explicitly acknowledging concurrent writes in the
    /// same canonical workspace when `acknowledge` is true.
    pub fn submit_with_workspace_collision_acknowledgement(
        &self,
        input: impl Into<String>,
        acknowledge: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.submit_with_attachments_and_workspace_collision_acknowledgement(
            input,
            Vec::new(),
            acknowledge,
        )
    }

    /// Submits a turn with staged resources and an explicit write-collision decision.
    pub fn submit_with_attachments_and_workspace_collision_acknowledgement(
        &self,
        input: impl Into<String>,
        attachments: Vec<DesktopAttachment>,
        acknowledge: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        let operation_id = DesktopOperationId(OperationId::new());
        let command_id = self.enqueue(BridgeCommandValue::Submit {
            operation_id,
            input: input.into(),
            attachments,
            acknowledge_workspace_write_collision: acknowledge,
        })?;
        Ok(DesktopCommandReceipt {
            command_id,
            operation_id: Some(operation_id),
        })
    }

    /// Asks Xana to validate and ingest one user-selected resource path.
    pub fn stage_resource(
        &self,
        path: impl Into<PathBuf>,
        external_approved: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::StageResource {
            path: path.into(),
            external_approved,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Asks Xana to ingest the current native clipboard image.
    pub fn stage_clipboard_image(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::StageClipboardImage)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    pub fn clear(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::Clear)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    pub fn interrupt(
        &self,
        operation_id: DesktopOperationId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::Interrupt { operation_id })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: Some(operation_id),
            })
    }

    /// Changes the model for later turns of the current managed Conversation.
    /// The provider-owned thread and its existing context remain unchanged.
    pub fn select_managed_model(
        &self,
        model: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SelectManagedModel {
            model: model.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Changes reasoning effort for later turns of the current managed Conversation.
    pub fn set_managed_reasoning(
        &self,
        effort: Option<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SetManagedReasoning { effort })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    pub fn decide_permission(
        &self,
        permission_id: DesktopPermissionId,
        allow_once: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::DecidePermission {
            permission_id,
            allow_once,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: Some(DesktopOperationId(permission_id.operation_id())),
        })
    }

    /// Continues the same native operation for its next configured tranche.
    pub fn continue_round_budget(
        &self,
        suspension: &DesktopRoundBudgetSuspension,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.decide_round_budget(suspension, RoundBudgetAction::Continue)
    }

    /// Stops the suspended native operation without rolling back committed effects.
    pub fn stop_round_budget(
        &self,
        suspension: &DesktopRoundBudgetSuspension,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.decide_round_budget(suspension, RoundBudgetAction::Stop)
    }

    fn decide_round_budget(
        &self,
        suspension: &DesktopRoundBudgetSuspension,
        action: RoundBudgetAction,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::DecideRoundBudget {
            operation_id: suspension.operation_id,
            suspension_id: suspension.id,
            action,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: Some(suspension.operation_id),
        })
    }

    pub fn request_snapshot(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::RequestSnapshot)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Persists the local full/mini sidebar preference through the runtime.
    pub fn set_sidebar_mode(
        &self,
        mode: DesktopSidebarMode,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SetSidebarMode { mode })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Requests that the persistent Desktop window attach to a retained Conversation.
    pub fn switch_conversation(
        &self,
        conversation_id: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SwitchConversation {
            conversation_id: conversation_id.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Requests a new ungrouped or Project-workspace Conversation.
    pub fn new_conversation(
        &self,
        project_id: Option<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::NewConversation { project_id })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Archives one inactive managed Conversation handle locally.
    ///
    /// Provider-owned history is never deleted, and native Conversations are
    /// immutable retained history rather than archiveable handles.
    pub fn archive_managed_conversation(
        &self,
        conversation_id: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::ArchiveManagedConversation {
            conversation_id: conversation_id.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Renames one Xana Project through the shared Project store.
    pub fn rename_project(
        &self,
        project_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::RenameProject {
            project_id: project_id.into(),
            name: name.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Archives or restores one Project without touching its workspace or Conversations.
    pub fn set_project_archived(
        &self,
        project_id: impl Into<String>,
        archived: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SetProjectArchived {
            project_id: project_id.into(),
            archived,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Removes the selected Conversation's optional Project membership.
    pub fn ungroup_conversation(
        &self,
        conversation_id: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::UngroupConversation {
            conversation_id: conversation_id.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Moves within a workspace or creates an explicitly confirmed continuation across workspaces.
    pub fn move_conversation(
        &self,
        conversation_id: impl Into<String>,
        project_id: impl Into<String>,
        allow_fresh_continuation: bool,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::MoveConversation {
            conversation_id: conversation_id.into(),
            project_id: project_id.into(),
            allow_fresh_continuation,
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Branches one retained Conversation at an exact, projected source point.
    pub fn branch_conversation(
        &self,
        conversation_id: impl Into<String>,
        source_point: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::BranchConversation {
            conversation_id: conversation_id.into(),
            source_point: source_point.into(),
        })
        .map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }

    /// Validates and atomically persists one Conversation's Workbench layout.
    pub fn save_layout(
        &self,
        layout: DesktopWorkbenchLayout,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SaveLayout { layout })
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Removes one Conversation override and resolves default/recovery layout again.
    pub fn reset_layout(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::ResetLayout)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Persists the current validated Workbench layout as the one user default.
    pub fn save_layout_as_default(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::SaveLayoutAsDefault)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Removes the user default without changing a valid Conversation override.
    pub fn clear_default_layout(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::ClearDefaultLayout)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Opens or returns the one process-local settings draft.
    pub fn begin_settings(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::BeginSettings)
    }

    /// Stages one scalar setting through Xana's shared validation contract.
    pub fn set_setting(
        &self,
        draft_id: DesktopSettingsDraftId,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::SetSetting {
            draft_id,
            key: key.into(),
            value: value.into(),
        })
    }

    /// Stages one setting's declared default.
    pub fn reset_setting(
        &self,
        draft_id: DesktopSettingsDraftId,
        key: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::ResetSetting {
            draft_id,
            key: key.into(),
        })
    }

    /// Removes one staged setting intent.
    pub fn revert_setting(
        &self,
        draft_id: DesktopSettingsDraftId,
        key: impl Into<String>,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::RevertSetting {
            draft_id,
            key: key.into(),
        })
    }

    /// Validates the complete draft without mutating durable owners.
    pub fn validate_settings(
        &self,
        draft_id: DesktopSettingsDraftId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::ValidateSettings { draft_id })
    }

    /// Atomically commits the complete draft after optimistic-concurrency checks.
    pub fn commit_settings(
        &self,
        draft_id: DesktopSettingsDraftId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::CommitSettings { draft_id })
    }

    /// Discards one process-local draft without writing either durable owner.
    pub fn discard_settings(
        &self,
        draft_id: DesktopSettingsDraftId,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::DiscardSettings { draft_id })
    }

    /// Reloads settings from their authoritative owners and discards any draft.
    pub fn reload_settings(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue_control(BridgeCommandValue::ReloadSettings)
    }

    /// Requests shutdown without blocking the GPUI application thread.
    pub fn request_shutdown(&self) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(BridgeCommandValue::Shutdown)
            .map(|command_id| DesktopCommandReceipt {
                command_id,
                operation_id: None,
            })
    }

    /// Drains one already-delivered update without blocking GPUI's render thread.
    pub fn try_next(&mut self) -> Result<Option<DesktopUpdate>, DesktopError> {
        match self.updates.try_recv() {
            Ok(update) => Ok(Some(update)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop runtime update stream closed",
            )),
        }
    }

    /// Requests clean shutdown and verifies the runtime thread terminates.
    pub fn shutdown(mut self) -> Result<(), DesktopError> {
        let _ = self.enqueue(BridgeCommandValue::Shutdown);
        self.backend_done
            .recv_timeout(SHUTDOWN_TIMEOUT)
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("Desktop runtime did not stop within 10 seconds: {error}"),
                )
            })?;
        if let Some(backend) = self.backend.take() {
            backend.join().map_err(|_| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeCrashed,
                    "Desktop runtime thread panicked during shutdown",
                )
            })?;
        }
        Ok(())
    }

    fn enqueue(&self, value: BridgeCommandValue) -> Result<u64, DesktopError> {
        use std::sync::atomic::Ordering;
        let command_id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        self.commands
            .try_send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id,
                value,
            })
            .map_err(|error| {
                DesktopError::new(
                    DesktopErrorCode::RuntimeUnavailable,
                    format!("Desktop command queue is unavailable: {error}"),
                )
            })?;
        Ok(command_id)
    }

    fn enqueue_control(
        &self,
        value: BridgeCommandValue,
    ) -> Result<DesktopCommandReceipt, DesktopError> {
        self.enqueue(value).map(|command_id| DesktopCommandReceipt {
            command_id,
            operation_id: None,
        })
    }
}

impl Drop for DesktopClient {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let command_id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.try_send(BridgeCommand {
            version: PROTOCOL_VERSION,
            command_id,
            value: BridgeCommandValue::Shutdown,
        });
    }
}

#[derive(Clone)]
struct StartupSignal(Arc<Mutex<Option<StartupSender>>>);

impl StartupSignal {
    fn new(sender: StartupSender) -> Self {
        Self(Arc::new(Mutex::new(Some(sender))))
    }

    fn ready(&self, snapshot: DesktopSnapshot) -> bool {
        if let Ok(mut sender) = self.0.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(Ok(snapshot));
            return true;
        }
        false
    }

    fn fail_if_pending(&self, error: DesktopError) {
        if let Ok(mut sender) = self.0.lock()
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(Err(error));
        }
    }
}

#[derive(Clone)]
pub(crate) struct Bridge {
    commands: Arc<tokio::sync::Mutex<mpsc::Receiver<BridgeCommand>>>,
    updates: mpsc::Sender<DesktopUpdate>,
    update_signal: DesktopWakeSignal,
    startup: StartupSignal,
    notification_policy: NotificationPolicy,
}

#[derive(Debug)]
struct BridgeCommand {
    version: u16,
    command_id: u64,
    value: BridgeCommandValue,
}

#[derive(Debug)]
enum BridgeCommandValue {
    Submit {
        operation_id: DesktopOperationId,
        input: String,
        attachments: Vec<DesktopAttachment>,
        acknowledge_workspace_write_collision: bool,
    },
    StageResource {
        path: PathBuf,
        external_approved: bool,
    },
    StageClipboardImage,
    SaveArtifact {
        artifact_id: String,
        destination: PathBuf,
    },
    RevealArtifact {
        artifact_id: String,
    },
    OpenArtifact {
        artifact_id: String,
    },
    Clear,
    Interrupt {
        operation_id: DesktopOperationId,
    },
    SelectManagedModel {
        model: String,
    },
    SetManagedReasoning {
        effort: Option<String>,
    },
    DecidePermission {
        permission_id: DesktopPermissionId,
        allow_once: bool,
    },
    DecideRoundBudget {
        operation_id: DesktopOperationId,
        suspension_id: DesktopRoundBudgetId,
        action: RoundBudgetAction,
    },
    RequestSnapshot,
    SetSidebarMode {
        mode: DesktopSidebarMode,
    },
    SwitchConversation {
        conversation_id: String,
    },
    NewConversation {
        project_id: Option<String>,
    },
    ArchiveManagedConversation {
        conversation_id: String,
    },
    RenameProject {
        project_id: String,
        name: String,
    },
    SetProjectArchived {
        project_id: String,
        archived: bool,
    },
    UngroupConversation {
        conversation_id: String,
    },
    MoveConversation {
        conversation_id: String,
        project_id: String,
        allow_fresh_continuation: bool,
    },
    BranchConversation {
        conversation_id: String,
        source_point: String,
    },
    SaveLayout {
        layout: DesktopWorkbenchLayout,
    },
    ResetLayout,
    SaveLayoutAsDefault,
    ClearDefaultLayout,
    BeginSettings,
    SetSetting {
        draft_id: DesktopSettingsDraftId,
        key: String,
        value: String,
    },
    ResetSetting {
        draft_id: DesktopSettingsDraftId,
        key: String,
    },
    RevertSetting {
        draft_id: DesktopSettingsDraftId,
        key: String,
    },
    ValidateSettings {
        draft_id: DesktopSettingsDraftId,
    },
    CommitSettings {
        draft_id: DesktopSettingsDraftId,
    },
    DiscardSettings {
        draft_id: DesktopSettingsDraftId,
    },
    ReloadSettings,
    Shutdown,
}

fn run_backend(
    paths: XanaPaths,
    workspace: PathBuf,
    conversation: Option<ConversationRef>,
    force_new: bool,
    bridge: Bridge,
) -> anyhow::Result<()> {
    let _diagnostics = crate::diagnostics::DiagnosticRuntime::start(&paths)
        .ok()
        .flatten();
    let stale_markers = _diagnostics
        .as_ref()
        .map_or(0, crate::diagnostics::DiagnosticRuntime::stale_markers);
    if let Err(error) = crate::host_lifecycle::recover_startup(&paths, stale_markers) {
        eprintln!("warning: Xana could not reconcile abandoned artifact staging files: {error}");
        crate::diagnostics::emit(
            crate::diagnostics::DiagnosticFact::new(
                crate::config::DiagnosticLevel::Warn,
                crate::config::DiagnosticTarget::Storage,
                crate::diagnostics::EventKind::RecoveryAction,
                crate::diagnostics::EventOutcome::Failed,
            )
            .subject("artifact_partial_reconciliation"),
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow::anyhow!("could not create Desktop async runtime: {error}"))?;
    runtime.block_on(async {
        if let Some(observer) = attached::connect_if_active(&paths, &workspace)
            .await
            .map_err(anyhow::Error::new)?
        {
            attached::serve(bridge, &paths, &workspace, observer)
                .await
                .map(|_| ())
                .map_err(anyhow::Error::new)
        } else {
            let launch = crate::app::run_desktop(
                paths.clone(),
                workspace.clone(),
                conversation,
                force_new,
                bridge.clone(),
            )
            .await;
            match launch {
                Ok(()) => Ok(()),
                Err(original) => {
                    // Close the discovery/start race without ever replacing an
                    // active descriptor or starting a second workspace owner.
                    match attached::connect_if_active(&paths, &workspace).await {
                        Ok(Some(observer)) => attached::serve(bridge, &paths, &workspace, observer)
                            .await
                            .map(|_| ())
                            .map_err(anyhow::Error::new),
                        Ok(None) => Err(original),
                        Err(error) => Err(anyhow::Error::new(error)),
                    }
                }
            }
        }
    })
}

pub(crate) async fn run_native(
    runtime: RuntimeHandle,
    header: &ChatHeader,
    workspace_host: WorkspaceHost,
    conversation: ConversationRef,
    bridge: Bridge,
    paths: &XanaPaths,
) -> anyhow::Result<ChatExit> {
    let reasoning_effort = header
        .models
        .selected()
        .ok()
        .and_then(|selection| selection.reasoning_effort);
    let seed = ClientSnapshotSeed {
        session_id: header.session_id,
        connection: header.provider_name.clone(),
        execution_owner: "native".to_owned(),
        model: header.model.clone(),
        reasoning_effort,
        host_location: crate::frontend::semantic::HostLocationV1::Embedded,
        approval_policy: header.permission_mode.as_str().to_owned(),
        children: header.children.clone(),
        resource_policy: header.resource_policy.clone(),
    };
    let execution_host = ExecutionHost::new();
    let navigation_store =
        navigation::DesktopNavigationStore::open(paths, workspace_host.workspace())?;
    let navigation = navigation_store.snapshot(Some(&conversation.to_string()))?;
    // A launch history entry is presentation state only. Failure to persist it
    // must not prevent the selected Conversation from starting.
    let _ = navigation_store.record_recent(Some(&conversation.to_string()));
    let layout_store = layout::DesktopLayoutStore::open(paths);
    let layout = layout_store.resolve(&conversation.to_string());
    let settings =
        settings::DesktopSettingsState::open(crate::settings::SettingsManager::new(paths))?;
    let attachment_store = header.artifact_store.clone();
    let attachments = DesktopAttachmentService {
        workspace: header.workspace_root.clone(),
        store: attachment_store.clone(),
        ingestor: ImageIngestor::new(attachment_store, ImageLimits::default()),
        resource_policy: header.resource_policy.clone(),
        owner: header.owner,
    };
    execution_host.register(
        workspace_host,
        ConversationRegistration::new(
            conversation.clone(),
            header.provider_name.clone(),
            header.model.clone(),
            Some(header.profile_name.clone()),
            header.permission_mode.as_str(),
        ),
    )?;
    execution_host.attach(&conversation)?;
    let exit = bridge
        .serve_native(
            EmbeddedClient::from_runtime(runtime, seed),
            execution_host,
            conversation,
            header.notification_policy.clone(),
            DesktopFrontendState {
                navigation,
                navigation_store,
                layout,
                layout_store,
                settings,
                attachments,
            },
        )
        .await?;
    Ok(exit)
}

impl Bridge {
    async fn serve_native(
        mut self,
        client: EmbeddedClient,
        execution_host: ExecutionHost,
        conversation: ConversationRef,
        notification_policy: NotificationPolicy,
        frontend: DesktopFrontendState,
    ) -> Result<ChatExit, DesktopError> {
        self.notification_policy = notification_policy;
        let DesktopFrontendState {
            mut navigation,
            navigation_store,
            mut layout,
            layout_store,
            mut settings,
            attachments,
        } = frontend;
        let mut commands = self.commands.lock().await;
        let (owner, mut observer) = client.into_parts();
        let mut snapshot = observer.snapshot().clone();
        let controller = DesktopController {
            conversation,
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
        let initial = project_snapshot(
            &snapshot,
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
        let mut shutdown_cleanup = crate::host_lifecycle::OwnedExecutionCleanup::Unresolved;
        let mut exit = ChatExit::Quit;

        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else {
                        execution_host.request_shutdown().map_err(host_error)?;
                        if owner.send(ClientCommand::new(RuntimeCommand::Shutdown)).await
                            .is_ok_and(|result| result.accepted)
                        {
                            shutdown_cleanup = crate::host_lifecycle::OwnedExecutionCleanup::Clean;
                        }
                        break;
                    };
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
                    let stop = self
                        .handle_command(
                            command,
                            NativeCommandContext {
                                owner: &owner,
                                execution_host: &execution_host,
                                controller: &controller,
                                snapshot: &snapshot,
                                active_run: &mut active_run,
                                navigation: &mut navigation,
                                navigation_store: &navigation_store,
                                layout: &mut layout,
                                layout_store: &layout_store,
                                settings: &mut settings,
                                attachments: &attachments,
                            },
                        )
                        .await?;
                    self.publish_host_changes(
                        &execution_host,
                        &snapshot,
                        &mut host_cursor,
                        &navigation,
                        &layout,
                        settings.snapshot(),
                    ).await?;
                    if let Some((cleanup, requested_exit)) = stop {
                        shutdown_cleanup = cleanup;
                        exit = requested_exit;
                        break;
                    }
                }
                observation = observer.next() => {
                    let observation = observation.map_err(|error| {
                        DesktopError::new(DesktopErrorCode::RuntimeUnavailable, error.to_string())
                    })?;
                    if observation.version != FRONTEND_PROTOCOL_VERSION {
                        return Err(DesktopError::new(
                            DesktopErrorCode::ProtocolMismatch,
                            format!(
                                "runtime observation protocol {} does not match Desktop protocol {}",
                                observation.version, FRONTEND_PROTOCOL_VERSION
                            ),
                        ));
                    }
                    let expected = snapshot.sequence.saturating_add(1);
                    if observation.sequence != expected {
                        self.publish_critical(DesktopUpdate::ResyncRequired {
                            expected_sequence: expected,
                            received_sequence: observation.sequence,
                        }).await?;
                    }
                    if let Err(error) = execution_host.record_runtime_observation(
                        &controller.conversation,
                        &observation,
                    ) {
                        self.publish_critical(DesktopUpdate::ResyncRequired {
                            expected_sequence: snapshot.sequence.saturating_add(1),
                            received_sequence: observation.sequence,
                        }).await?;
                        if !matches!(error, crate::execution_host::ExecutionHostError::RuntimeGap { .. }) {
                            return Err(host_error(error));
                        }
                    }
                    if let Some(outcome) = observation_outcome(&observation)
                        && let Some(run) = active_run.take()
                    {
                        execution_host.finish_run(run, outcome).map_err(host_error)?;
                    }
                    self.publish_host_changes(
                        &execution_host,
                        &snapshot,
                        &mut host_cursor,
                        &navigation,
                        &layout,
                        settings.snapshot(),
                    ).await?;
                    snapshot.apply(&observation.event, observation.sequence);
                    let projected = DesktopObservation {
                        version: observation.version,
                        sequence: observation.sequence,
                        event: project_event(
                            &observation.event,
                            &snapshot.session_id,
                            &snapshot.semantic.attachment_policy.configured,
                        ),
                    };
                    let replaceable = projected.event.replaceable();
                    self.publish(DesktopUpdate::Observation(projected), replaceable).await?;
                }
            }
        }

        execution_host.request_shutdown().map_err(host_error)?;
        drop(active_run);
        execution_host
            .complete_shutdown(crate::host_lifecycle::ShutdownProof {
                durable_state_flushed: true,
                owned_execution: shutdown_cleanup,
            })
            .map_err(host_error)?;
        self.publish_host_changes(
            &execution_host,
            &snapshot,
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

    async fn handle_command(
        &self,
        command: BridgeCommand,
        context: NativeCommandContext<'_>,
    ) -> Result<Option<(crate::host_lifecycle::OwnedExecutionCleanup, ChatExit)>, DesktopError>
    {
        let NativeCommandContext {
            owner,
            execution_host,
            controller,
            snapshot,
            active_run,
            navigation,
            navigation_store,
            layout,
            layout_store,
            settings,
            attachments,
        } = context;
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
            BridgeCommandValue::RequestSnapshot => {
                let host_snapshot = execution_host.snapshot().map_err(host_error)?;
                self.publish_critical(DesktopUpdate::Snapshot(Box::new(project_snapshot(
                    snapshot,
                    &host_snapshot,
                    &self.notification_policy,
                    navigation,
                    layout,
                    settings.snapshot(),
                ))))
                .await?;
                self.publish_command_result(command_id, Ok(())).await?;
                Ok(None)
            }
            BridgeCommandValue::SetSidebarMode { mode } => {
                let result = navigation_store.set_sidebar_mode(mode);
                if result.is_ok() {
                    navigation.sidebar_mode = mode;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::SwitchConversation { conversation_id } => {
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "wait for or interrupt the active Run before switching Conversations",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                let Some(destination) = navigation_store.resolve_conversation(&conversation_id)?
                else {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::StateInvalid,
                            format!("Conversation {conversation_id} is no longer available"),
                        )),
                    )
                    .await?;
                    return Ok(None);
                };
                let cleanup = shutdown_owner(owner).await;
                self.publish_command_result(command_id, Ok(())).await?;
                Ok(Some((
                    cleanup,
                    ChatExit::DesktopSwitchConversation {
                        workspace: destination.workspace,
                        conversation: destination.conversation,
                    },
                )))
            }
            BridgeCommandValue::NewConversation { project_id } => {
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "wait for or interrupt the active Run before creating a Conversation",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                let workspace = navigation_store.resolve_new_workspace(project_id.as_deref())?;
                let cleanup = shutdown_owner(owner).await;
                self.publish_command_result(command_id, Ok(())).await?;
                Ok(Some((
                    cleanup,
                    ChatExit::DesktopNewConversation { workspace },
                )))
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
                        navigation_store.snapshot(Some(&controller.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::RenameProject { project_id, name } => {
                let result = navigation_store.rename_project(&project_id, &name);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&controller.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::SetProjectArchived {
                project_id,
                archived,
            } => {
                let result = navigation_store.set_project_archived(&project_id, archived);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&controller.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::UngroupConversation { conversation_id } => {
                let result = navigation_store.ungroup_conversation(&conversation_id);
                if result.is_ok() {
                    *navigation =
                        navigation_store.snapshot(Some(&controller.conversation.to_string()))?;
                    self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::MoveConversation {
                conversation_id,
                project_id,
                allow_fresh_continuation,
            } => {
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "wait for or interrupt the active Run before moving a Conversation",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                match navigation_store.move_conversation(
                    &conversation_id,
                    &project_id,
                    allow_fresh_continuation,
                ) {
                    Ok(Some(destination)) => {
                        let cleanup = shutdown_owner(owner).await;
                        self.publish_command_result(command_id, Ok(())).await?;
                        Ok(Some((
                            cleanup,
                            ChatExit::DesktopSwitchConversation {
                                workspace: destination.workspace,
                                conversation: destination.conversation,
                            },
                        )))
                    }
                    Ok(None) => {
                        *navigation = navigation_store
                            .snapshot(Some(&controller.conversation.to_string()))?;
                        self.publish_critical(DesktopUpdate::Navigation(navigation.clone()))
                            .await?;
                        self.publish_command_result(command_id, Ok(())).await?;
                        Ok(None)
                    }
                    Err(error) => {
                        self.publish_command_result(command_id, Err(error)).await?;
                        Ok(None)
                    }
                }
            }
            BridgeCommandValue::BranchConversation {
                conversation_id,
                source_point,
            } => {
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "wait for or interrupt the active Run before branching a Conversation",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                match navigation_store.branch_conversation(&conversation_id, &source_point) {
                    Ok(destination) => {
                        let cleanup = shutdown_owner(owner).await;
                        self.publish_command_result(command_id, Ok(())).await?;
                        Ok(Some((
                            cleanup,
                            ChatExit::DesktopSwitchConversation {
                                workspace: destination.workspace,
                                conversation: destination.conversation,
                            },
                        )))
                    }
                    Err(error) => {
                        self.publish_command_result(command_id, Err(error)).await?;
                        Ok(None)
                    }
                }
            }
            BridgeCommandValue::SaveLayout { layout: candidate } => {
                let result = candidate.validate().and_then(|()| {
                    layout_store.save_conversation(&controller.conversation.to_string(), &candidate)
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
                Ok(None)
            }
            BridgeCommandValue::ResetLayout => {
                let result = layout_store.clear_conversation(&controller.conversation.to_string());
                if result.is_ok() {
                    *layout = layout_store.resolve(&controller.conversation.to_string());
                    self.publish_critical(DesktopUpdate::Layout(Box::new(layout.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::SaveLayoutAsDefault => {
                let result = layout_store.save_default(&layout.layout);
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::ClearDefaultLayout => {
                let result = layout_store.clear_default();
                if result.is_ok() {
                    *layout = layout_store.resolve(&controller.conversation.to_string());
                    self.publish_critical(DesktopUpdate::Layout(Box::new(layout.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::BeginSettings => {
                let result = settings.begin();
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
                Ok(None)
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
                Ok(None)
            }
            BridgeCommandValue::ResetSetting { draft_id, key } => {
                let result = settings.reset(draft_id, &key);
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
                Ok(None)
            }
            BridgeCommandValue::RevertSetting { draft_id, key } => {
                let result = settings.revert(draft_id, &key);
                if let Ok(draft) = &result {
                    self.publish_critical(DesktopUpdate::SettingsDraft(Some(draft.clone())))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
                Ok(None)
            }
            BridgeCommandValue::ValidateSettings { draft_id } => {
                let result = settings.validate(draft_id);
                if let Ok(receipt) = &result {
                    self.publish_critical(DesktopUpdate::SettingsReceipt(receipt.clone()))
                        .await?;
                }
                self.publish_command_result(command_id, result.map(|_| ()))
                    .await?;
                Ok(None)
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
                Ok(None)
            }
            BridgeCommandValue::DiscardSettings { draft_id } => {
                let result = settings.discard(draft_id);
                if result.is_ok() {
                    self.publish_critical(DesktopUpdate::SettingsDraft(None))
                        .await?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
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
                Ok(None)
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
                Ok(None)
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
                Ok(None)
            }
            BridgeCommandValue::SaveArtifact {
                artifact_id,
                destination,
            } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Save(destination),
                )
                .await;
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::RevealArtifact { artifact_id } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Reveal,
                )
                .await;
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::OpenArtifact { artifact_id } => {
                let result = perform_artifact_command(
                    attachments.clone(),
                    snapshot,
                    &artifact_id,
                    DesktopArtifactCommand::Open,
                )
                .await;
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::Shutdown => {
                execution_host.request_shutdown().map_err(host_error)?;
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::Shutdown))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime rejected Desktop shutdown because it is unavailable",
                        )
                    })
                    .and_then(command_result);
                let cleanup = if result.is_ok() {
                    crate::host_lifecycle::OwnedExecutionCleanup::Clean
                } else {
                    crate::host_lifecycle::OwnedExecutionCleanup::Unresolved
                };
                self.publish_command_result(command_id, result).await?;
                Ok(Some((cleanup, ChatExit::Quit)))
            }
            BridgeCommandValue::Submit {
                operation_id,
                input,
                attachments,
                acknowledge_workspace_write_collision,
            } => {
                if active_run.is_some() {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::HostBusy,
                            "a root operation is already active",
                        )),
                    )
                    .await?;
                    return Ok(None);
                }
                let run = execution_host.begin_run(
                    &controller.conversation,
                    operation_id.0,
                    RunAccess::WorkspaceWrite,
                    if acknowledge_workspace_write_collision {
                        WriteCollisionDecision::Acknowledge
                    } else {
                        WriteCollisionDecision::Reject
                    },
                );
                let run = match run {
                    Ok(run) => run,
                    Err(error) => {
                        self.publish_command_result(command_id, Err(host_error(error)))
                            .await?;
                        return Ok(None);
                    }
                };
                let images = match validate_desktop_attachments(attachments) {
                    Ok(images) => images,
                    Err(error) => {
                        execution_host
                            .finish_run(run, Err(error.message.clone()))
                            .map_err(host_error)?;
                        self.publish_command_result(command_id, Err(error)).await?;
                        return Ok(None);
                    }
                };
                let runtime_command = if images.is_empty() {
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
                };
                let result = owner
                    .send(ClientCommand::new(runtime_command))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                if result.is_ok() {
                    *active_run = Some(run);
                } else {
                    let reason = result.as_ref().err().map_or_else(
                        || "runtime rejected the Run".to_owned(),
                        ToString::to_string,
                    );
                    execution_host
                        .finish_run(run, Err(reason))
                        .map_err(host_error)?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::Clear => {
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::ClearConversation))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::Interrupt { operation_id } => {
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::InterruptOperation {
                        operation_id: operation_id.0,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::SelectManagedModel { .. }
            | BridgeCommandValue::SetManagedReasoning { .. } => {
                self.publish_command_result(
                    command_id,
                    Err(DesktopError::new(
                        DesktopErrorCode::UnsupportedExecutionOwner,
                        "in-place model and reasoning changes belong to managed runtimes; use Settings and start a new native Conversation",
                    )),
                )
                .await?;
                Ok(None)
            }
            BridgeCommandValue::DecidePermission {
                permission_id,
                allow_once,
            } => {
                let DesktopPermissionTarget::Native {
                    operation_id,
                    invocation_id,
                } = permission_id.0
                else {
                    self.publish_command_result(
                        command_id,
                        Err(DesktopError::new(
                            DesktopErrorCode::CommandRejected,
                            "managed approval was sent to the native runtime",
                        )),
                    )
                    .await?;
                    return Ok(None);
                };
                let decision = if allow_once {
                    ControllerDecision::AllowOnce
                } else {
                    ControllerDecision::Deny
                };
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::DecidePermission {
                        operation_id,
                        invocation_id,
                        decision,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
            BridgeCommandValue::DecideRoundBudget {
                operation_id,
                suspension_id,
                action,
            } => {
                let mut acquired = None;
                if action == RoundBudgetAction::Continue && active_run.is_none() {
                    match execution_host.begin_run(
                        &controller.conversation,
                        operation_id.0,
                        RunAccess::WorkspaceWrite,
                        WriteCollisionDecision::Reject,
                    ) {
                        Ok(run) => acquired = Some(run),
                        Err(error) => {
                            self.publish_command_result(command_id, Err(host_error(error)))
                                .await?;
                            return Ok(None);
                        }
                    }
                }
                let result = owner
                    .send(ClientCommand::new(RuntimeCommand::DecideRoundBudget {
                        operation_id: operation_id.0,
                        suspension_id: suspension_id.0,
                        action,
                    }))
                    .await
                    .map_err(|_| {
                        DesktopError::new(
                            DesktopErrorCode::RuntimeUnavailable,
                            "runtime is unavailable",
                        )
                    })
                    .and_then(command_result);
                if result.is_ok() {
                    if let Some(run) = acquired {
                        *active_run = Some(run);
                    }
                } else if let Some(run) = acquired {
                    let reason = result.as_ref().err().map_or_else(
                        || "runtime rejected the Run".to_owned(),
                        ToString::to_string,
                    );
                    execution_host
                        .finish_run(run, Err(reason))
                        .map_err(host_error)?;
                }
                self.publish_command_result(command_id, result).await?;
                Ok(None)
            }
        }
    }

    async fn publish_command_result(
        &self,
        command_id: u64,
        result: Result<(), DesktopError>,
    ) -> Result<(), DesktopError> {
        self.publish_critical(DesktopUpdate::CommandResult {
            command_id,
            accepted: result.is_ok(),
            error: result.err(),
        })
        .await
    }

    async fn publish_host_changes(
        &self,
        host: &ExecutionHost,
        frontend: &ClientSnapshot,
        cursor: &mut u64,
        navigation: &DesktopNavigationSnapshot,
        layout: &DesktopResolvedLayout,
        settings: &DesktopSettingsSnapshot,
    ) -> Result<(), DesktopError> {
        match host.changes_after(*cursor).map_err(host_error)? {
            HostChanges::Events(events) => {
                for event in events {
                    *cursor = event.sequence;
                    self.publish(
                        DesktopUpdate::HostObservation(project_host_observation(event)),
                        false,
                    )
                    .await?;
                }
            }
            HostChanges::SnapshotRequired(snapshot) => {
                *cursor = snapshot.sequence;
                self.publish_critical(DesktopUpdate::Snapshot(Box::new(project_snapshot(
                    frontend,
                    &snapshot,
                    &self.notification_policy,
                    navigation,
                    layout,
                    settings,
                ))))
                .await?;
            }
        }
        Ok(())
    }

    async fn publish(&self, update: DesktopUpdate, replaceable: bool) -> Result<(), DesktopError> {
        match self.updates.try_send(update) {
            Ok(()) => {
                self.update_signal.wake();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) if replaceable => Ok(()),
            Err(mpsc::error::TrySendError::Full(update)) => self.publish_critical(update).await,
            Err(mpsc::error::TrySendError::Closed(_)) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop projection detached",
            )),
        }
    }

    async fn publish_critical(&self, update: DesktopUpdate) -> Result<(), DesktopError> {
        match tokio::time::timeout(CRITICAL_UPDATE_GRACE, self.updates.send(update)).await {
            Ok(Ok(())) => {
                self.update_signal.wake();
                Ok(())
            }
            Ok(Err(_)) => Err(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                "Desktop projection detached",
            )),
            Err(_) => Err(DesktopError::new(
                DesktopErrorCode::ObserverStalled,
                "Desktop did not receive a critical update within 5 seconds",
            )),
        }
    }
}

async fn shutdown_owner(
    owner: &crate::frontend::EmbeddedOwner,
) -> crate::host_lifecycle::OwnedExecutionCleanup {
    if owner
        .send(ClientCommand::new(RuntimeCommand::Shutdown))
        .await
        .is_ok_and(|result| result.accepted)
    {
        crate::host_lifecycle::OwnedExecutionCleanup::Clean
    } else {
        crate::host_lifecycle::OwnedExecutionCleanup::Unresolved
    }
}

fn command_result(result: crate::frontend::ClientCommandResult) -> Result<(), DesktopError> {
    if result.accepted {
        Ok(())
    } else {
        Err(DesktopError::new(
            DesktopErrorCode::CommandRejected,
            result
                .reason
                .unwrap_or_else(|| "runtime rejected the Desktop command".to_owned()),
        ))
    }
}

fn host_error(error: crate::execution_host::ExecutionHostError) -> DesktopError {
    let code = match error {
        crate::execution_host::ExecutionHostError::Limit { .. }
        | crate::execution_host::ExecutionHostError::ConversationBusy(_)
        | crate::execution_host::ExecutionHostError::WriteCollision { .. }
        | crate::execution_host::ExecutionHostError::Workspace(
            crate::workspace_host::WorkspaceHostError::Busy(_),
        ) => DesktopErrorCode::HostBusy,
        crate::execution_host::ExecutionHostError::RuntimeGap { .. } => {
            DesktopErrorCode::ProtocolMismatch
        }
        crate::execution_host::ExecutionHostError::Controller(
            crate::controller::ControllerLeaseError::NotController(_),
        ) => DesktopErrorCode::AuthorityRequired,
        _ => DesktopErrorCode::StateInvalid,
    };
    DesktopError::new(code, error.to_string())
}

fn observation_outcome(
    observation: &ClientObservation,
) -> Option<Result<OperationOutcome, String>> {
    match &observation.event {
        ClientEvent::Runtime(event) => match event.as_ref() {
            AgentEvent::OperationStateChanged {
                state: OperationState::Finished(outcome),
                ..
            } => Some(Ok(*outcome)),
            AgentEvent::OperationFailed { reason, .. } => Some(Err(reason.clone())),
            _ => None,
        },
        ClientEvent::Managed(_) | ClientEvent::Semantic(_) | ClientEvent::PayloadOmitted { .. } => {
            None
        }
    }
}

fn project_snapshot(
    snapshot: &ClientSnapshot,
    host: &crate::execution_host::ExecutionHostSnapshot,
    notification_policy: &NotificationPolicy,
    navigation: &DesktopNavigationSnapshot,
    layout: &DesktopResolvedLayout,
    settings: &DesktopSettingsSnapshot,
) -> DesktopSnapshot {
    let profile = host.attached.as_ref().and_then(|selected| {
        host.conversations
            .iter()
            .find(|candidate| &candidate.conversation == selected)
            .and_then(|candidate| candidate.profile.clone())
    });
    let mut projected = project_frontend_snapshot(
        snapshot,
        DesktopAuthority::Owner,
        false,
        notification_policy,
        navigation,
        layout,
        settings,
        profile,
    );
    projected.host_sequence = host.sequence;
    projected.hosted_workspace_count = host.workspaces.len();
    projected.hosted_conversation_count = host.conversations.len();
    projected.hosted_conversations = host
        .conversations
        .iter()
        .map(|conversation| DesktopHostedConversation {
            conversation: conversation.conversation.to_string(),
            workspace_id: conversation.workspace_id.clone(),
            connection: conversation.connection.clone(),
            model: conversation.model.clone(),
            profile: conversation.profile.clone(),
            permission_mode: conversation.permission_mode.clone(),
            state: project_conversation_state(conversation.state),
            active_operation: conversation.active_operation.map(DesktopOperationId),
            pending_approvals: conversation.pending_approvals,
            activity_count: conversation.activity_count,
            last_outcome: conversation
                .last_outcome
                .map(|outcome| format!("{outcome:?}").to_ascii_lowercase()),
            controller: conversation.controller.as_ref().map(project_controller),
        })
        .collect();
    projected.attached_conversation = host.attached.as_ref().map(ToString::to_string);
    projected.controllers = host
        .conversations
        .iter()
        .filter_map(|conversation| conversation.controller.as_ref().map(project_controller))
        .collect();
    projected.host_lifecycle = format!("{:?}", host.lifecycle).to_ascii_lowercase();
    projected.global_notices = host
        .global_notices
        .iter()
        .map(project_global_notice)
        .collect();
    projected
}

#[allow(clippy::too_many_arguments)]
fn project_frontend_snapshot(
    snapshot: &ClientSnapshot,
    authority: DesktopAuthority,
    attached_to_foreground_host: bool,
    notification_policy: &NotificationPolicy,
    navigation: &DesktopNavigationSnapshot,
    layout: &DesktopResolvedLayout,
    settings: &DesktopSettingsSnapshot,
    profile: Option<String>,
) -> DesktopSnapshot {
    DesktopSnapshot {
        version: snapshot.version,
        sequence: snapshot.sequence,
        authority,
        attached_to_foreground_host,
        session_id: snapshot.session_id.to_string(),
        connection: snapshot.connection.clone(),
        execution_owner: snapshot.execution_owner.clone(),
        model: snapshot.model.clone(),
        reasoning_effort: snapshot.reasoning_effort.clone(),
        notification_policy: notification_policy.clone(),
        conversation: content::project_messages(
            snapshot.session_id.to_string(),
            &snapshot.conversation,
            &snapshot.semantic.attachment_policy.configured,
        ),
        conversation_truncated: snapshot.conversation_truncated,
        active_operation: snapshot.active_operation.map(DesktopOperationId),
        pending_approval_count: snapshot.pending_approval_count,
        pending_approvals: snapshot
            .pending_approvals
            .iter()
            .map(|approval| DesktopPendingApproval {
                id: DesktopPermissionId::native(approval.operation_id, approval.invocation_id),
                tool: approval.tool_name.clone(),
                effect: format!("{:?}", approval.effect_class).to_ascii_lowercase(),
                scope: permission_scope_label(&approval.scope),
            })
            .collect(),
        activity_count: snapshot.activity_count,
        artifact_count: snapshot.artifact_count,
        host_sequence: 0,
        hosted_workspace_count: 0,
        hosted_conversation_count: 0,
        hosted_conversations: Vec::new(),
        attached_conversation: None,
        controllers: Vec::new(),
        host_lifecycle: "running".to_owned(),
        global_notices: Vec::new(),
        navigation: navigation.clone(),
        layout: layout.clone(),
        settings: settings.clone(),
        conversation_facts: conversation::project_conversation_facts(snapshot, profile),
    }
}

fn project_host_observation(
    observation: crate::execution_host::HostObservation,
) -> DesktopHostObservation {
    let event = match observation.event {
        HostEvent::ConversationRegistered {
            conversation,
            workspace_id,
        } => DesktopHostEvent::ConversationRegistered {
            conversation: conversation.to_string(),
            workspace_id,
        },
        HostEvent::RunStarted {
            conversation,
            operation_id,
            access,
            collision_acknowledged,
        } => DesktopHostEvent::RunStarted {
            conversation: conversation.to_string(),
            operation_id: DesktopOperationId(operation_id),
            workspace_write: access == RunAccess::WorkspaceWrite,
            collision_acknowledged,
        },
        HostEvent::RuntimeObservation { conversation, .. } => {
            DesktopHostEvent::RuntimeObservation {
                conversation: conversation.to_string(),
            }
        }
        HostEvent::RunFinished {
            conversation,
            operation_id,
            state,
            error,
        } => DesktopHostEvent::RunFinished {
            conversation: conversation.to_string(),
            operation_id: DesktopOperationId(operation_id),
            state: project_conversation_state(state),
            error,
        },
        HostEvent::ConversationAttached {
            previous,
            conversation,
            restoration,
        } => DesktopHostEvent::ConversationAttached {
            previous: previous.map(|value| value.to_string()),
            conversation: conversation.to_string(),
            restoration: match restoration {
                crate::execution_host::OwnerRestoration::NativeDurableHistory => {
                    "native_durable_history"
                }
                crate::execution_host::OwnerRestoration::ManagedOpaqueThread => {
                    "managed_opaque_thread"
                }
            }
            .to_owned(),
        },
        HostEvent::ControllerChanged {
            conversation,
            controller,
            change,
        } => DesktopHostEvent::ControllerChanged {
            conversation: conversation.to_string(),
            controller: controller.as_ref().map(project_controller),
            change: match change {
                crate::controller::ControllerChangeKind::Acquired => "acquired".to_owned(),
                crate::controller::ControllerChangeKind::Renewed => "renewed".to_owned(),
                crate::controller::ControllerChangeKind::Reconnected => "reconnected".to_owned(),
                crate::controller::ControllerChangeKind::TakenOver { .. } => {
                    "taken_over".to_owned()
                }
                crate::controller::ControllerChangeKind::Disconnected { reason } => {
                    format!("disconnected:{reason:?}").to_ascii_lowercase()
                }
                crate::controller::ControllerChangeKind::Released => "released".to_owned(),
                crate::controller::ControllerChangeKind::Expired => "expired".to_owned(),
            },
        },
        HostEvent::GlobalNotice { notice } => {
            DesktopHostEvent::GlobalNotice(project_global_notice(&notice))
        }
        HostEvent::LifecycleChanged { state } => DesktopHostEvent::LifecycleChanged {
            state: format!("{state:?}").to_ascii_lowercase(),
        },
        HostEvent::ShutdownCompleted { receipt } => DesktopHostEvent::ShutdownCompleted {
            interrupted_runs: receipt.interrupted_runs.len(),
            cleanup: format!("{:?}", receipt.owned_execution).to_ascii_lowercase(),
        },
    };
    DesktopHostObservation {
        sequence: observation.sequence,
        event,
    }
}

fn project_conversation_state(
    state: crate::execution_host::HostedConversationState,
) -> DesktopConversationState {
    match state {
        crate::execution_host::HostedConversationState::Idle => DesktopConversationState::Idle,
        crate::execution_host::HostedConversationState::Running => {
            DesktopConversationState::Running
        }
        crate::execution_host::HostedConversationState::Suspended => {
            DesktopConversationState::Suspended
        }
        crate::execution_host::HostedConversationState::Completed => {
            DesktopConversationState::Completed
        }
        crate::execution_host::HostedConversationState::Failed => DesktopConversationState::Failed,
        crate::execution_host::HostedConversationState::Declined => {
            DesktopConversationState::Declined
        }
        crate::execution_host::HostedConversationState::Interrupted => {
            DesktopConversationState::Interrupted
        }
    }
}

fn project_global_notice(notice: &crate::host_lifecycle::GlobalNotice) -> DesktopGlobalNotice {
    DesktopGlobalNotice {
        kind: format!("{:?}", notice.kind).to_ascii_lowercase(),
        code: notice.code.clone(),
        conversation: notice.conversation.as_ref().map(ToString::to_string),
        operation_id: notice.operation_id.map(DesktopOperationId),
    }
}

fn project_controller<K: ToString>(
    controller: &crate::controller::ControllerLeaseSnapshot<K>,
) -> DesktopControllerLease {
    DesktopControllerLease {
        conversation: controller.conversation.to_string(),
        controller_id: controller.controller_id.to_string(),
        generation: controller.generation,
        state: format!("{:?}", controller.state).to_ascii_lowercase(),
        takeover_confirmed: matches!(
            controller.takeover,
            crate::controller::ControllerTakeoverState::Confirmed
        ),
        reconnect_grace_remaining_ms: controller.reconnect_grace_remaining_ms,
        disconnect_reason: controller
            .disconnect_reason
            .map(|reason| format!("{reason:?}").to_ascii_lowercase()),
    }
}

fn project_event(
    event: &ClientEvent,
    session_id: &crate::identity::SessionId,
    resource_policy: &crate::resource::ResourcePolicyV1,
) -> DesktopEvent {
    if let Some(activity) = conversation::project_live_activity(event) {
        return DesktopEvent::ActivityUpserted(activity);
    }
    match event {
        ClientEvent::Runtime(event) => match event.as_ref() {
            AgentEvent::OperationStateChanged {
                operation_id,
                state,
            } => DesktopEvent::OperationState {
                operation_id: DesktopOperationId(*operation_id),
                state: project_operation_state(*state),
            },
            AgentEvent::AssistantTextDelta {
                operation_id, text, ..
            } => DesktopEvent::AssistantDelta {
                operation_id: DesktopOperationId(*operation_id),
                text: bounded_text(text.clone(), MAX_PUBLIC_TEXT_BYTES),
            },
            AgentEvent::ProviderReasoningDelta {
                operation_id, text, ..
            } => DesktopEvent::ReasoningDelta {
                operation_id: DesktopOperationId(*operation_id),
                text: bounded_text(text.clone(), MAX_PUBLIC_TEXT_BYTES),
            },
            AgentEvent::PermissionRequested { request } => project_permission(request),
            AgentEvent::PermissionAudited { fact } => DesktopEvent::PermissionResolved {
                permission_id: DesktopPermissionId::native(
                    fact.request.operation_id,
                    fact.request.invocation_id,
                ),
            },
            AgentEvent::AssistantMessage {
                operation_id,
                message,
            } => DesktopEvent::MessageFinal {
                operation_id: DesktopOperationId(*operation_id),
                message: content::project_message(
                    format!("{session_id}:{operation_id}:final"),
                    message,
                    resource_policy,
                ),
            },
            AgentEvent::UsageObserved {
                operation_id,
                usage,
            } => DesktopEvent::Usage {
                operation_id: DesktopOperationId(*operation_id),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                requests: usage.requests,
            },
            AgentEvent::RoundBudgetReached { suspension } => {
                DesktopEvent::RoundBudgetReached(DesktopRoundBudgetSuspension {
                    id: DesktopRoundBudgetId(suspension.id),
                    operation_id: DesktopOperationId(suspension.operation_id),
                    rounds_consumed: suspension.rounds_consumed,
                    hard_round_limit: suspension.hard_round_limit,
                    remaining_rounds: suspension.remaining_rounds,
                    committed_steps: suspension.committed.steps,
                    committed_invocations: suspension.committed.invocations,
                    committed_results: suspension.committed.results,
                    repeated_tool_patterns: suspension.repeated_tool_patterns,
                    can_continue: suspension
                        .allowed_actions
                        .contains(&RoundBudgetAction::Continue),
                })
            }
            AgentEvent::RoundBudgetDecisionCommitted { decision } => {
                DesktopEvent::RoundBudgetDecision {
                    suspension_id: DesktopRoundBudgetId(decision.suspension_id),
                    continued: decision.action == RoundBudgetAction::Continue,
                }
            }
            AgentEvent::OperationFailed {
                operation_id,
                reason,
            } => DesktopEvent::Error(DesktopError::new(
                DesktopErrorCode::RuntimeUnavailable,
                format!("operation {operation_id} failed: {reason}"),
            )),
            AgentEvent::ConversationCleared => DesktopEvent::ConversationCleared,
            AgentEvent::PromptPlanUpdated { ledger, .. } => DesktopEvent::Activity {
                label: format!(
                    "Prompt plan: {} / {} estimated input tokens",
                    ledger.estimated_input_tokens, ledger.budget.input_budget_tokens
                ),
            },
            AgentEvent::CompactionStarted { .. } => DesktopEvent::Activity {
                label: "Compacting older conversation context".to_owned(),
            },
            AgentEvent::ConversationCompacted { checkpoint } => DesktopEvent::Activity {
                label: format!(
                    "Compacted {} canonical entries; raw history retained",
                    checkpoint.source_entry_count
                ),
            },
            AgentEvent::CompactionUnavailable { reason, .. } => DesktopEvent::Activity {
                label: format!("Compaction unavailable: {reason}"),
            },
            AgentEvent::CommandRejected { reason } => DesktopEvent::Error(DesktopError::new(
                DesktopErrorCode::CommandRejected,
                reason.clone(),
            )),
            AgentEvent::ToolFinished { .. } => DesktopEvent::Activity {
                label: "Tool finished".to_owned(),
            },
            AgentEvent::InvocationIntentCommitted { .. } => DesktopEvent::Activity {
                label: "Tool invocation committed".to_owned(),
            },
            AgentEvent::InvocationResultCommitted { .. } => DesktopEvent::Activity {
                label: "Tool result committed".to_owned(),
            },
            AgentEvent::ChildLifecycleChanged { .. } => DesktopEvent::Activity {
                label: "Child lifecycle changed".to_owned(),
            },
            AgentEvent::ChildActivity { .. } => DesktopEvent::Activity {
                label: "Child activity updated".to_owned(),
            },
            AgentEvent::ChildReportCommitted { .. } => DesktopEvent::Activity {
                label: "Child report committed".to_owned(),
            },
            AgentEvent::ChildListSnapshot { .. } => DesktopEvent::Activity {
                label: "Child list refreshed".to_owned(),
            },
            AgentEvent::ChildInspectionSnapshot { .. } => DesktopEvent::Activity {
                label: "Child inspection refreshed".to_owned(),
            },
            AgentEvent::ChildCancellationRequested { .. } => DesktopEvent::Activity {
                label: "Child cancellation requested".to_owned(),
            },
            AgentEvent::ExternalAgentActivity { .. } => DesktopEvent::Activity {
                label: "External agent activity updated".to_owned(),
            },
        },
        ClientEvent::Managed(_) => DesktopEvent::Error(DesktopError::new(
            DesktopErrorCode::UnsupportedExecutionOwner,
            "managed event reached the native Desktop adapter",
        )),
        ClientEvent::Semantic(_) => DesktopEvent::Activity {
            label: "Semantic frontend state updated".to_owned(),
        },
        ClientEvent::PayloadOmitted {
            kind,
            encoded_bytes,
            limit,
        } => DesktopEvent::Activity {
            label: format!(
                "Oversized {kind} payload omitted ({encoded_bytes} bytes; limit {limit})"
            ),
        },
    }
}

fn project_operation_state(state: OperationState) -> DesktopOperationState {
    match state {
        OperationState::Running => DesktopOperationState::Running,
        OperationState::Suspended => DesktopOperationState::Suspended,
        OperationState::Finished(OperationOutcome::Completed) => DesktopOperationState::Completed,
        OperationState::Finished(OperationOutcome::Failed) => DesktopOperationState::Failed,
        OperationState::Finished(OperationOutcome::Declined) => DesktopOperationState::Declined,
        OperationState::Finished(OperationOutcome::Interrupted) => {
            DesktopOperationState::Interrupted
        }
    }
}

fn project_permission(request: &PermissionRequest) -> DesktopEvent {
    DesktopEvent::PermissionRequired {
        permission_id: DesktopPermissionId::native(request.operation_id, request.invocation_id),
        tool: request.tool_name.clone(),
        effect: format!("{:?}", request.effect_class).to_ascii_lowercase(),
        scope: permission_scope_label(&request.scope),
    }
}

fn permission_scope_label(scope: &PermissionScope) -> String {
    match scope {
        PermissionScope::WorkspacePath { canonical_path } => {
            format!("workspace path {}", canonical_path.display())
        }
        PermissionScope::ExternalPath { canonical_path } => {
            format!("external path {}", canonical_path.display())
        }
        PermissionScope::Command {
            shell,
            canonical_cwd,
            command,
        } => format!("{shell} in {}: {command}", canonical_cwd.display()),
        PermissionScope::External { operation, .. } => {
            format!("external recipient for {operation}")
        }
        PermissionScope::BuiltInResource { id } => format!("built-in resource {id}"),
        PermissionScope::Unscoped => "unscoped".to_owned(),
    }
}

fn validate_desktop_attachments(
    attachments: Vec<DesktopAttachment>,
) -> Result<Vec<crate::vision::ImageRef>, DesktopError> {
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
    let mut byte_len = 0_u64;
    let mut images = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        if !ids.insert(attachment.id.clone()) {
            return Err(DesktopError::new(
                DesktopErrorCode::StateInvalid,
                format!("attachment {} appears more than once", attachment.id),
            ));
        }
        byte_len = byte_len.saturating_add(attachment.byte_len);
        images.push(attachment.into_image()?);
    }
    if byte_len > crate::vision::MAX_IMAGE_BYTES_PER_TURN {
        return Err(DesktopError::new(
            DesktopErrorCode::StateInvalid,
            format!(
                "turn images total {byte_len} bytes; limit is {}",
                crate::vision::MAX_IMAGE_BYTES_PER_TURN
            ),
        ));
    }
    Ok(images)
}

fn classify_backend_error(error: &anyhow::Error) -> DesktopError {
    if let Some(error) = error.downcast_ref::<DesktopError>() {
        return error.clone();
    }
    let message = format!("{error:#}");
    let code = if message.contains("not initialized") || message.contains("config") {
        DesktopErrorCode::ConfigurationUnavailable
    } else if message.contains("already has an active Xana root") {
        DesktopErrorCode::HostBusy
    } else if message.contains("invalid") || message.contains("corrupt") {
        DesktopErrorCode::StateInvalid
    } else {
        DesktopErrorCode::RuntimeUnavailable
    };
    DesktopError::new(code, message)
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::Agent,
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        context::ContextBudget,
        identity::StepId,
        permission::{PermissionPolicy, PolicyDecision},
        prompt::{PromptEnvironment, PromptInputs, PromptSurface, assemble_snapshot},
        provider::{ConversationalProvider, DeltaSink, ProviderError},
        shell::ShellConfig,
        tool::{ToolDefinition, ToolRegistry},
    };
    use anyhow::Result;
    use futures::future::BoxFuture;

    type BridgeChannels = (
        Bridge,
        mpsc::Sender<BridgeCommand>,
        mpsc::Receiver<DesktopUpdate>,
        std_mpsc::Receiver<Result<DesktopSnapshot, DesktopError>>,
    );

    struct ScriptedProvider;

    impl ConversationalProvider for ScriptedProvider {
        fn stream_message<'a>(
            &'a self,
            _messages: &'a [Message],
            _tools: &'a [&'a ToolDefinition],
            step_id: StepId,
            deltas: &'a dyn DeltaSink,
        ) -> BoxFuture<'a, Result<Message, ProviderError>> {
            Box::pin(async move {
                deltas.text_delta(step_id, "hello ");
                deltas.text_delta(step_id, "from Desktop");
                Ok(Message::text(Role::Assistant, "hello from Desktop"))
            })
        }
    }

    fn scripted_client(workspace: &std::path::Path) -> (EmbeddedClient, ConversationRef) {
        let tools = ToolRegistry::new();
        let definitions = tools.definitions();
        let environment = PromptEnvironment {
            connection: "test-connection".to_owned(),
            model: "test-model".to_owned(),
            operating_system: "test".to_owned(),
            working_directory: workspace.to_owned(),
            configured_shell: "test shell".to_owned(),
            surface: PromptSurface::Cli,
        };
        let prompt = assemble_snapshot(PromptInputs {
            tool_definitions: &definitions,
            environment: &environment,
            product_documentation: None,
            project_sources: &[],
            budget: ContextBudget {
                total_tokens: 16_384,
                conversation_reserve_tokens: 4_096,
            },
        })
        .unwrap();
        let agent = Agent::new(
            Box::new(ScriptedProvider),
            tools,
            workspace.to_owned(),
            prompt,
            2,
        );
        let policy = PermissionPolicy::new(PolicyDecision::Allow, Vec::new(), workspace).unwrap();
        let session_id = crate::identity::SessionId::new();
        let conversation = ConversationRef::Native { session_id };
        let client = EmbeddedClient::from_runtime(
            RuntimeHandle::spawn(agent, policy, true),
            ClientSnapshotSeed {
                session_id,
                connection: "scripted".to_owned(),
                execution_owner: "native".to_owned(),
                model: "test-model".to_owned(),
                reasoning_effort: None,
                host_location: crate::frontend::semantic::HostLocationV1::Embedded,
                approval_policy: "ask".to_owned(),
                children: Vec::new(),
                resource_policy: crate::resource::ResourcePolicyV1::default(),
            },
        );
        (client, conversation)
    }

    fn execution_host(
        data_root: &std::path::Path,
        workspace: &std::path::Path,
        conversation: &ConversationRef,
    ) -> ExecutionHost {
        let host = ExecutionHost::new();
        host.register(
            WorkspaceHost::open(data_root, workspace).unwrap(),
            ConversationRegistration::new(
                conversation.clone(),
                "scripted",
                "test-model",
                Some("test-profile".to_owned()),
                "allow",
            ),
        )
        .unwrap();
        host
    }

    fn settings_state(paths: &XanaPaths) -> settings::DesktopSettingsState {
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "ollama".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen3:1.7b".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .expect("render settings config");
        std::fs::write(paths.config_file(), rendered).expect("write settings config");
        settings::DesktopSettingsState::open(crate::settings::SettingsManager::new(paths))
            .expect("open Desktop settings")
    }

    fn attachment_service(
        paths: &XanaPaths,
        workspace: &std::path::Path,
    ) -> DesktopAttachmentService {
        let store = crate::artifact::ArtifactStore::new(paths.data_dir().join("artifacts"));
        DesktopAttachmentService {
            workspace: workspace.to_owned(),
            store: store.clone(),
            ingestor: ImageIngestor::new(store, ImageLimits::default()),
            resource_policy: crate::resource::ResourcePolicyV1::default(),
            owner: crate::identity::PrincipalId::new(),
        }
    }

    fn bridge_channels() -> BridgeChannels {
        let (command_sender, commands) = mpsc::channel(COMMAND_CAPACITY);
        let (updates, update_receiver) = mpsc::channel(UPDATE_CAPACITY);
        let (startup_sender, startup_receiver) = std_mpsc::sync_channel(1);
        (
            Bridge {
                commands: Arc::new(tokio::sync::Mutex::new(commands)),
                updates,
                update_signal: DesktopWakeSignal::default(),
                startup: StartupSignal::new(startup_sender),
                notification_policy: NotificationPolicy::default(),
            },
            command_sender,
            update_receiver,
            startup_receiver,
        )
    }

    #[tokio::test]
    async fn bridge_publication_wakes_the_observer_without_a_polling_clock() {
        let (bridge, _, mut updates, _) = bridge_channels();
        let signal = bridge.update_signal.clone();

        bridge
            .publish_critical(DesktopUpdate::CommandResult {
                command_id: 7,
                accepted: true,
                error: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), signal.wait())
            .await
            .expect("published update must wake the observer");

        assert!(matches!(
            updates.try_recv(),
            Ok(DesktopUpdate::CommandResult {
                command_id: 7,
                accepted: true,
                error: None,
            })
        ));
    }

    #[test]
    fn public_message_projection_never_exposes_image_paths_or_bytes() {
        let message = Message::text(Role::Assistant, "bounded answer");
        let projected = content::project_message(
            "stable".to_owned(),
            &message,
            &crate::resource::ResourcePolicyV1::default(),
        );

        assert_eq!(projected.id, "stable");
        assert!(matches!(
            projected.content.as_slice(),
            [DesktopContent {
                value: DesktopContentValue::Text(text),
                ..
            }] if text == "bounded answer"
        ));
    }

    #[test]
    fn desktop_attachment_staging_returns_only_bounded_presentation_metadata() {
        use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let image_path = workspace.join("private-name.png");
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[0; 8], 2, 1, ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&image_path, png).unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();

        let attachment = attachment_service(&paths, &workspace)
            .stage_path("private-name.png", false)
            .unwrap();

        assert_eq!(attachment.name, "private-name.png");
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!((attachment.width, attachment.height), (Some(2), Some(1)));
        let debug = format!("{attachment:?}");
        assert!(!debug.contains(&workspace.display().to_string()));
        assert!(!debug.contains("content_hash"));
    }

    #[test]
    fn desktop_artifact_export_requires_a_visible_resource_and_preserves_bytes() {
        use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let image_path = workspace.join("image.png");
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[0; 4], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&image_path, &png).unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();
        let service = attachment_service(&paths, &workspace);
        let image = service
            .stage_path("image.png", false)
            .unwrap()
            .into_image()
            .unwrap();
        let artifact_id = image.artifact.reference.id.to_string();
        let snapshot = ClientSnapshot::initial(
            ClientSnapshotSeed {
                session_id: crate::identity::SessionId::new(),
                connection: "fixture".to_owned(),
                execution_owner: "native".to_owned(),
                model: "fixture".to_owned(),
                reasoning_effort: None,
                host_location: crate::frontend::semantic::HostLocationV1::Embedded,
                approval_policy: "ask".to_owned(),
                children: Vec::new(),
                resource_policy: crate::resource::ResourcePolicyV1::default(),
            },
            vec![Message {
                role: Role::User,
                content: vec![crate::message::ContentBlock::Image(image)],
            }],
        );

        let artifact = artifact_record_for_id(&snapshot, &artifact_id).unwrap();
        let destination = directory.path().join("saved.png");
        service.save_artifact(&artifact, &destination).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), png);
        assert!(artifact_record_for_id(&snapshot, "not-visible").is_err());
    }

    #[test]
    fn desktop_submission_rejects_duplicate_attachment_capabilities() {
        use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let image_path = workspace.join("image.png");
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[0; 4], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&image_path, png).unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();
        let attachment = attachment_service(&paths, &workspace)
            .stage_path("image.png", false)
            .unwrap();

        let error = validate_desktop_attachments(vec![attachment.clone(), attachment]).unwrap_err();
        assert_eq!(error.code, DesktopErrorCode::StateInvalid);
        assert!(error.message.contains("more than once"));
    }

    #[test]
    fn desktop_retains_non_image_resources_without_claiming_provider_input() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            workspace.join("clip.webm"),
            [0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0],
        )
        .unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();

        let attachment = attachment_service(&paths, &workspace)
            .stage_path("clip.webm", false)
            .unwrap();

        assert_eq!(attachment.name, "clip.webm");
        assert_eq!(attachment.kind, "video");
        assert_eq!(attachment.media_type, "video/webm");
        assert!(!attachment.provider_input_available);
        let debug = format!("{attachment:?}");
        assert!(!debug.contains(&workspace.display().to_string()));
        assert!(!debug.contains("content_hash"));

        let error = validate_desktop_attachments(vec![attachment]).unwrap_err();
        assert_eq!(error.code, DesktopErrorCode::CommandRejected);
        assert!(error.message.contains("retained"));
        assert!(error.message.contains("does not support it as turn input"));
    }

    #[test]
    fn desktop_command_projection_preserves_authority_and_stable_ids() {
        let observer = desktop_commands(DesktopAuthority::Observer, true);
        let project = observer
            .iter()
            .find(|command| command.id == "project.manage.v1")
            .unwrap();
        assert!(!project.available);
        assert_eq!(project.availability_code, "authority_required");

        let owner = desktop_command("project.manage.v1", DesktopAuthority::Owner, true).unwrap();
        assert!(owner.available);
        assert_eq!(owner.family, "project");
        assert!(desktop_command("future.synthetic.v9", DesktopAuthority::Owner, true).is_none());
    }

    #[test]
    fn desktop_presentation_capabilities_are_not_runtime_authority() {
        let presentation = desktop_presentation_capabilities();
        assert!(presentation.pointer);
        assert!(presentation.inline_images);
        assert!(presentation.native_accessibility);
        let observer_clear =
            desktop_command("conversation.clear.v1", DesktopAuthority::Observer, true).unwrap();
        assert!(!observer_clear.available);
    }

    #[test]
    fn invalid_command_version_is_a_semantic_protocol_failure() {
        let error = DesktopError::new(
            DesktopErrorCode::ProtocolMismatch,
            "Desktop command protocol 0 does not match runtime protocol 2",
        );

        assert_eq!(error.code.as_str(), "protocol_mismatch");
        assert!(error.message.contains("does not match"));
    }

    #[test]
    fn round_budget_projection_preserves_exact_decision_identity() {
        let operation_id = OperationId::new();
        let suspension_id = RoundBudgetId::new();
        let event = ClientEvent::Runtime(Box::new(AgentEvent::RoundBudgetReached {
            suspension: crate::native_runtime::RoundBudgetSuspension {
                id: suspension_id,
                operation_id,
                soft_round_limit: 8,
                last_tranche_rounds: 8,
                rounds_consumed: 8,
                hard_round_limit: 256,
                remaining_rounds: 248,
                continuations_used: 0,
                committed: crate::native_runtime::RoundBudgetCommitFacts {
                    steps: 8,
                    invocations: 8,
                    results: 8,
                },
                repeated_tool_patterns: 2,
                usage: crate::agent::AgentTurnUsage::empty(),
                allowed_actions: vec![RoundBudgetAction::Continue, RoundBudgetAction::Stop],
            },
        }));

        let DesktopEvent::RoundBudgetReached(projected) = project_event(
            &event,
            &crate::identity::SessionId::new(),
            &crate::resource::ResourcePolicyV1::default(),
        ) else {
            panic!("round-budget projection")
        };
        assert_eq!(projected.operation_id, DesktopOperationId(operation_id));
        assert_eq!(projected.id, DesktopRoundBudgetId(suspension_id));
        assert!(projected.can_continue);
        assert_eq!(projected.committed_results, 8);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_embedded_turn_streams_then_publishes_authoritative_final() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let (client, conversation) = scripted_client(&workspace);
        let host = execution_host(directory.path(), &workspace, &conversation);
        let inspection = host.clone();
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();
        let navigation_store =
            navigation::DesktopNavigationStore::open(&paths, &workspace).unwrap();
        let layout_store = layout::DesktopLayoutStore::open(&paths);
        let layout = layout_store.resolve(&conversation.to_string());
        let settings = settings_state(&paths);
        let runtime = tokio::spawn(bridge.serve_native(
            client,
            host,
            conversation.clone(),
            NotificationPolicy::default(),
            DesktopFrontendState {
                navigation: DesktopNavigationSnapshot::empty(DesktopSidebarMode::Full),
                navigation_store,
                layout,
                layout_store,
                settings,
                attachments: attachment_service(&paths, &workspace),
            },
        ));

        let initial = startup
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(initial.connection, "scripted");
        assert_eq!(initial.controllers.len(), 1);
        assert_eq!(
            initial.controllers[0].conversation,
            conversation.to_string()
        );

        let operation_id = DesktopOperationId::new();
        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 1,
                value: BridgeCommandValue::Submit {
                    operation_id,
                    input: "hello".to_owned(),
                    attachments: Vec::new(),
                    acknowledge_workspace_write_collision: false,
                },
            })
            .await
            .unwrap();

        let mut accepted = false;
        let mut delta = String::new();
        let mut final_text = None;
        let mut completed = false;
        while !(accepted && !delta.is_empty() && final_text.is_some() && completed) {
            let update = tokio::time::timeout(Duration::from_secs(2), updates.recv())
                .await
                .unwrap()
                .unwrap();
            match update {
                DesktopUpdate::CommandResult {
                    command_id: 1,
                    accepted: true,
                    ..
                } => accepted = true,
                DesktopUpdate::Observation(DesktopObservation {
                    event: DesktopEvent::AssistantDelta { text, .. },
                    ..
                }) => delta.push_str(&text),
                DesktopUpdate::Observation(DesktopObservation {
                    event: DesktopEvent::MessageFinal { message, .. },
                    ..
                }) => {
                    final_text = message
                        .content
                        .into_iter()
                        .find_map(|content| match content {
                            DesktopContent {
                                value: DesktopContentValue::Text(text),
                                ..
                            } => Some(text),
                            _ => None,
                        });
                }
                DesktopUpdate::Observation(DesktopObservation {
                    event:
                        DesktopEvent::OperationState {
                            state: DesktopOperationState::Completed,
                            ..
                        },
                    ..
                }) => completed = true,
                _ => {}
            }
        }
        assert_eq!(delta, "hello from Desktop");
        assert_eq!(final_text.as_deref(), Some("hello from Desktop"));

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 2,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        runtime.await.unwrap().unwrap();
        assert!(
            inspection
                .snapshot()
                .unwrap()
                .conversations
                .iter()
                .all(|conversation| conversation.controller.is_none())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn desktop_settings_draft_validates_and_commits_through_the_bridge() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let (client, conversation) = scripted_client(&workspace);
        let host = execution_host(directory.path(), &workspace, &conversation);
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();
        let navigation_store =
            navigation::DesktopNavigationStore::open(&paths, &workspace).unwrap();
        let layout_store = layout::DesktopLayoutStore::open(&paths);
        let layout = layout_store.resolve(&conversation.to_string());
        let settings = settings_state(&paths);
        let runtime = tokio::spawn(bridge.serve_native(
            client,
            host,
            conversation,
            NotificationPolicy::default(),
            DesktopFrontendState {
                navigation: DesktopNavigationSnapshot::empty(DesktopSidebarMode::Full),
                navigation_store,
                layout,
                layout_store,
                settings,
                attachments: attachment_service(&paths, &workspace),
            },
        ));
        let initial = startup
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(
            initial
                .settings
                .entries
                .iter()
                .any(|entry| entry.key == "appearance.theme")
        );

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 10,
                value: BridgeCommandValue::BeginSettings,
            })
            .await
            .unwrap();
        let draft_id = loop {
            match tokio::time::timeout(Duration::from_secs(1), updates.recv())
                .await
                .unwrap()
                .unwrap()
            {
                DesktopUpdate::SettingsDraft(Some(draft)) => break draft.id,
                DesktopUpdate::CommandResult {
                    command_id: 10,
                    accepted: false,
                    error,
                } => panic!("begin rejected: {error:?}"),
                _ => {}
            }
        };

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 11,
                value: BridgeCommandValue::SetSetting {
                    draft_id,
                    key: "appearance.theme".to_owned(),
                    value: "dark".to_owned(),
                },
            })
            .await
            .unwrap();
        loop {
            if let DesktopUpdate::SettingsDraft(Some(draft)) =
                tokio::time::timeout(Duration::from_secs(1), updates.recv())
                    .await
                    .unwrap()
                    .unwrap()
                && draft.pending_count == 1
            {
                break;
            }
        }

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 12,
                value: BridgeCommandValue::ValidateSettings { draft_id },
            })
            .await
            .unwrap();
        loop {
            if let DesktopUpdate::SettingsReceipt(receipt) =
                tokio::time::timeout(Duration::from_secs(1), updates.recv())
                    .await
                    .unwrap()
                    .unwrap()
            {
                assert!(receipt.dry_run);
                break;
            }
        }

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 13,
                value: BridgeCommandValue::CommitSettings { draft_id },
            })
            .await
            .unwrap();
        let committed = loop {
            if let DesktopUpdate::Settings(snapshot) =
                tokio::time::timeout(Duration::from_secs(1), updates.recv())
                    .await
                    .unwrap()
                    .unwrap()
            {
                break snapshot;
            }
        };
        assert_eq!(
            committed
                .entries
                .iter()
                .find(|entry| entry.key == "appearance.theme")
                .and_then(|entry| entry.value.raw.as_deref()),
            Some("dark")
        );

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 14,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        runtime.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mismatched_command_is_rejected_without_stopping_the_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let (client, conversation) = scripted_client(&workspace);
        let host = execution_host(directory.path(), &workspace, &conversation);
        let (bridge, commands, mut updates, startup) = bridge_channels();
        let paths = XanaPaths::resolve(Some(directory.path().into())).unwrap();
        let navigation_store =
            navigation::DesktopNavigationStore::open(&paths, &workspace).unwrap();
        let layout_store = layout::DesktopLayoutStore::open(&paths);
        let layout = layout_store.resolve(&conversation.to_string());
        let settings = settings_state(&paths);
        let runtime = tokio::spawn(bridge.serve_native(
            client,
            host,
            conversation,
            NotificationPolicy::default(),
            DesktopFrontendState {
                navigation: DesktopNavigationSnapshot::empty(DesktopSidebarMode::Full),
                navigation_store,
                layout,
                layout_store,
                settings,
                attachments: attachment_service(&paths, &workspace),
            },
        ));
        startup
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION.saturating_sub(1),
                command_id: 7,
                value: BridgeCommandValue::Clear,
            })
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(1), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            update,
            DesktopUpdate::CommandResult {
                command_id: 7,
                accepted: false,
                error: Some(DesktopError {
                    code: DesktopErrorCode::ProtocolMismatch,
                    ..
                }),
                ..
            }
        ));

        commands
            .send(BridgeCommand {
                version: PROTOCOL_VERSION,
                command_id: 8,
                value: BridgeCommandValue::Shutdown,
            })
            .await
            .unwrap();
        runtime.await.unwrap().unwrap();
    }
}
