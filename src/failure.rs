//! Content-free failure provenance shared by execution owners and clients.
//!
//! These values carry no raw error text, URL, body, path, or free-form label.
//! Observation never authorizes retry or changes an operation's durable outcome.

use crate::identity::{ConversationId, OperationId};
use serde::{Deserialize, Serialize};

/// Keeps persistence provenance across a writer channel's legacy text reply.
/// This display is not part of the serialized diagnostic schema.
#[derive(Debug)]
pub(crate) struct PersistenceFailure(String);

impl PersistenceFailure {
    pub(crate) fn error(reason: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(Self(reason.into()))
    }
}

impl std::fmt::Display for PersistenceFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for PersistenceFailure {}

/// A one-way correlation label, including for syntactically plausible secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MetadataDigest([u8; 16]);

impl MetadataDigest {
    pub(crate) fn of(value: &str) -> Self {
        let mut digest = [0; 16];
        digest.copy_from_slice(&blake3::hash(value.as_bytes()).as_bytes()[..16]);
        Self(digest)
    }

    pub(crate) fn request_id(value: &str) -> Option<Self> {
        // Grammar validation is not proof that an opaque ID is not a secret.
        // Valid identifiers are still hashed; malformed/oversized ones omitted.
        (!value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte)))
        .then(|| Self::of(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    ProviderRejected,
    ProviderRateLimited,
    ProviderUnavailable,
    ConnectTimeout,
    ReadTimeout,
    Transport,
    BrokenStream,
    InvalidResponse,
    InvalidRequest,
    OutputLimit,
    PermissionDeclined,
    RoundBudget,
    ToolNoProgress,
    Cancelled,
    Interrupted,
    Storage,
    HostShutdown,
    HostPanic,
    ManagedRuntime,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    RequestPreparation,
    ProviderConnect,
    ProviderResponse,
    ProviderStream,
    Permission,
    Execution,
    Persistence,
    Shutdown,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryAdvice {
    NotRecommended,
    ExplicitNewRequest,
    ReconcileFirst,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKnowledge {
    RejectedBeforeDispatch,
    ResponseObserved,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDetails {
    pub category: FailureCategory,
    pub stage: FailureStage,
    pub retry: RetryAdvice,
    pub outcome_knowledge: OutcomeKnowledge,
    pub http_status: Option<u16>,
    pub request_id_digest: Option<MetadataDigest>,
}

impl FailureDetails {
    pub(crate) fn new(category: FailureCategory, stage: FailureStage) -> Self {
        Self {
            category,
            stage,
            retry: RetryAdvice::Unknown,
            outcome_knowledge: OutcomeKnowledge::Unknown,
            http_status: None,
            request_id_digest: None,
        }
    }

    pub(crate) fn response(mut self, status: u16, request_id: Option<MetadataDigest>) -> Self {
        self.http_status = (100..=599).contains(&status).then_some(status);
        self.request_id_digest = request_id;
        self
    }

    pub(crate) fn rejection(status: u16, request_id: Option<MetadataDigest>) -> Self {
        let category = match status {
            429 => FailureCategory::ProviderRateLimited,
            500..=599 => FailureCategory::ProviderUnavailable,
            _ => FailureCategory::ProviderRejected,
        };
        let mut detail =
            Self::new(category, FailureStage::ProviderResponse).response(status, request_id);
        detail.outcome_knowledge = OutcomeKnowledge::ResponseObserved;
        detail.retry = match status {
            429 | 500..=599 => RetryAdvice::ExplicitNewRequest,
            _ => RetryAdvice::NotRecommended,
        };
        detail
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Failed,
    Declined,
    Suspended,
    Cancelled,
    Interrupted,
    HostShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureOrigin {
    Native,
    Managed,
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDiagnostic {
    pub version: u16,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) conversation_id: Option<ConversationId>,
    /// No distinct run identity is invented when this owner does not expose one.
    pub run_id: Option<uuid::Uuid>,
    pub origin: FailureOrigin,
    pub outcome: TerminalOutcome,
    pub failure: FailureDetails,
    pub route_digest: Option<MetadataDigest>,
    pub model_digest: Option<MetadataDigest>,
    pub runtime_version: [u16; 3],
    pub runtime_revision: Option<MetadataDigest>,
}

impl TerminalDiagnostic {
    pub fn operation_id(&self) -> Option<uuid::Uuid> {
        self.operation_id.map(OperationId::as_uuid)
    }

    pub fn conversation_id(&self) -> Option<uuid::Uuid> {
        self.conversation_id.map(ConversationId::as_uuid)
    }

    pub(crate) fn new(
        operation_id: Option<OperationId>,
        conversation_id: Option<ConversationId>,
        origin: FailureOrigin,
        outcome: TerminalOutcome,
        failure: FailureDetails,
    ) -> Self {
        Self {
            version: 1,
            operation_id,
            conversation_id,
            run_id: None,
            origin,
            outcome,
            failure,
            route_digest: None,
            model_digest: None,
            runtime_version: [
                env!("CARGO_PKG_VERSION_MAJOR")
                    .parse()
                    .expect("package version"),
                env!("CARGO_PKG_VERSION_MINOR")
                    .parse()
                    .expect("package version"),
                env!("CARGO_PKG_VERSION_PATCH")
                    .parse()
                    .expect("package version"),
            ],
            runtime_revision: option_env!("XANA_BUILD_REVISION").map(MetadataDigest::of),
        }
    }

    pub(crate) fn route(mut self, route: Option<&str>, model: Option<&str>) -> Self {
        self.route_digest = route.map(MetadataDigest::of);
        self.model_digest = model.map(MetadataDigest::of);
        self
    }
}
