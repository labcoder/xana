//! Typed focused-service operations and deterministic named-route resolution.
//!
//! M3-18 establishes this boundary before M3-19 through M3-21 construct the
//! production adapters and application surface.

#![allow(dead_code)]

use crate::{
    artifact::{ArtifactRecord, ArtifactStore},
    config::{ConnectionRegistry, CredentialReference, OutboundDataClass, ServiceRouteDeclaration},
    identity::{OperationId, PrincipalId},
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_FOCUSED_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_FOCUSED_INPUT_ARTIFACTS: usize = 16;
pub(crate) const MAX_FOCUSED_OUTPUT_ARTIFACTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ServiceOperation {
    ImageGenerate,
    VisionAnalyze,
}

impl ServiceOperation {
    pub(crate) fn parse(value: &str) -> Result<Self, FocusedServiceError> {
        match value {
            "image.generate" => Ok(Self::ImageGenerate),
            "vision.analyze" => Ok(Self::VisionAnalyze),
            _ => Err(FocusedServiceError::UnsupportedOperation(value.to_owned())),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ImageGenerate => "image.generate",
            Self::VisionAnalyze => "vision.analyze",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaModality {
    Text,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FocusedServiceDescriptor {
    pub(crate) adapter: String,
    pub(crate) operation: ServiceOperation,
    pub(crate) inputs: BTreeSet<MediaModality>,
    pub(crate) outputs: BTreeSet<MediaModality>,
    pub(crate) supports_editing: bool,
    pub(crate) output_formats: BTreeSet<String>,
    pub(crate) max_prompt_bytes: usize,
    pub(crate) max_input_artifacts: usize,
    pub(crate) max_output_artifacts: usize,
    pub(crate) max_artifact_bytes: usize,
    pub(crate) requires_credential: bool,
    pub(crate) supports_cancellation: bool,
    pub(crate) reports_usage: bool,
    pub(crate) reports_cost: bool,
}

impl FocusedServiceDescriptor {
    pub(crate) fn validate(&self) -> Result<(), FocusedServiceError> {
        if !valid_id(&self.adapter)
            || self.inputs.is_empty()
            || self.outputs.is_empty()
            || self.max_prompt_bytes == 0
            || self.max_prompt_bytes > MAX_FOCUSED_PROMPT_BYTES
            || self.max_input_artifacts > MAX_FOCUSED_INPUT_ARTIFACTS
            || self.max_output_artifacts == 0
            || self.max_output_artifacts > MAX_FOCUSED_OUTPUT_ARTIFACTS
            || self.max_artifact_bytes == 0
            || self.max_artifact_bytes > crate::artifact::MAX_ARTIFACT_BYTES
            || self.output_formats.len() > 16
            || self
                .output_formats
                .iter()
                .any(|format| !valid_format(format))
        {
            return Err(FocusedServiceError::InvalidDescriptor(self.adapter.clone()));
        }
        match self.operation {
            ServiceOperation::ImageGenerate
                if !self.inputs.contains(&MediaModality::Text)
                    || !self.outputs.contains(&MediaModality::Image) =>
            {
                Err(FocusedServiceError::InvalidDescriptor(self.adapter.clone()))
            }
            ServiceOperation::VisionAnalyze
                if !self.inputs.contains(&MediaModality::Image)
                    || !self.outputs.contains(&MediaModality::Text) =>
            {
                Err(FocusedServiceError::InvalidDescriptor(self.adapter.clone()))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedServiceRoute {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) connection: String,
    pub(crate) adapter: String,
    pub(crate) base_url: Option<String>,
    pub(crate) credential: Option<CredentialReference>,
    pub(crate) model: String,
    pub(crate) operation: ServiceOperation,
    pub(crate) options: BTreeMap<String, String>,
    pub(crate) descriptor: FocusedServiceDescriptor,
    pub(crate) allowed_outbound: BTreeSet<OutboundDataClass>,
    pub(crate) is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ServiceRouteStatus {
    pub(crate) route: String,
    pub(crate) operation: Option<ServiceOperation>,
    pub(crate) configured: bool,
    pub(crate) exposed: bool,
    pub(crate) compatible: bool,
    pub(crate) credential_declared: bool,
    pub(crate) ready: bool,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FocusedServiceProvenance {
    pub(crate) operation_id: OperationId,
    pub(crate) route: String,
    pub(crate) connection: String,
    pub(crate) adapter: String,
    pub(crate) model: String,
    pub(crate) operation: ServiceOperation,
    pub(crate) options: BTreeMap<String, String>,
    pub(crate) created_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct FocusedServiceUsage {
    pub(crate) input_units: Option<u64>,
    pub(crate) output_units: Option<u64>,
    pub(crate) cost_microusd: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct FocusedServiceRequest {
    pub(crate) operation_id: OperationId,
    pub(crate) route: ResolvedServiceRoute,
    pub(crate) prompt: String,
    pub(crate) input_artifacts: Vec<ArtifactRecord>,
}

impl FocusedServiceRequest {
    pub(crate) fn validate(&self) -> Result<(), FocusedServiceError> {
        if self.prompt.trim().is_empty()
            || self.prompt.len() > self.route.descriptor.max_prompt_bytes
            || self
                .prompt
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(FocusedServiceError::InvalidRequest(
                "prompt is blank, unsafe, or exceeds the route limit",
            ));
        }
        if self.input_artifacts.len() > self.route.descriptor.max_input_artifacts {
            return Err(FocusedServiceError::InvalidRequest(
                "too many input artifacts for the selected route",
            ));
        }
        if !self.input_artifacts.is_empty()
            && !self.route.descriptor.inputs.contains(&MediaModality::Image)
        {
            return Err(FocusedServiceError::InvalidRequest(
                "the selected route does not accept image input",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FocusedServiceContext {
    pub(crate) artifacts: ArtifactStore,
    pub(crate) owner: PrincipalId,
    pub(crate) cancellation: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FocusedServiceResult {
    pub(crate) provenance: FocusedServiceProvenance,
    pub(crate) artifacts: Vec<ArtifactRecord>,
    pub(crate) derived_text: Option<String>,
    pub(crate) usage: FocusedServiceUsage,
    pub(crate) usage_available: bool,
    pub(crate) cost_available: bool,
    pub(crate) untrusted: bool,
}

pub(crate) trait FocusedServiceAdapter: Send + Sync {
    fn descriptors(&self) -> &[FocusedServiceDescriptor];

    fn execute<'a>(
        &'a self,
        request: FocusedServiceRequest,
        context: FocusedServiceContext,
    ) -> BoxFuture<'a, Result<FocusedServiceResult, FocusedServiceError>>;
}

#[derive(Default)]
pub(crate) struct FocusedServiceRegistry {
    adapters: BTreeMap<String, Arc<dyn FocusedServiceAdapter>>,
    descriptors: BTreeMap<(String, ServiceOperation), FocusedServiceDescriptor>,
}

impl FocusedServiceRegistry {
    pub(crate) fn register(
        &mut self,
        adapter: Arc<dyn FocusedServiceAdapter>,
    ) -> Result<(), FocusedServiceError> {
        let descriptors = adapter.descriptors();
        if descriptors.is_empty() {
            return Err(FocusedServiceError::InvalidDescriptor("unknown".into()));
        }
        let adapter_id = descriptors[0].adapter.clone();
        if descriptors
            .iter()
            .any(|descriptor| descriptor.adapter != adapter_id)
            || self.adapters.contains_key(&adapter_id)
        {
            return Err(FocusedServiceError::DuplicateAdapter(adapter_id));
        }
        for descriptor in descriptors {
            descriptor.validate()?;
            let key = (adapter_id.clone(), descriptor.operation);
            if self.descriptors.insert(key, descriptor.clone()).is_some() {
                return Err(FocusedServiceError::DuplicateDescriptor(
                    adapter_id,
                    descriptor.operation,
                ));
            }
        }
        self.adapters.insert(adapter_id, adapter);
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        registry: &ConnectionRegistry,
        exposed_routes: &[String],
        profile_egress: &[OutboundDataClass],
        operation: ServiceOperation,
        selected_route: Option<&str>,
    ) -> Result<ResolvedServiceRoute, FocusedServiceError> {
        let name = match selected_route {
            Some(name) => name.to_owned(),
            None => self.resolve_default(registry, exposed_routes, operation)?,
        };
        if !exposed_routes.iter().any(|candidate| candidate == &name) {
            return Err(FocusedServiceError::RouteNotExposed(name));
        }
        let route = registry
            .service_routes
            .get(&name)
            .ok_or_else(|| FocusedServiceError::UnknownRoute(name.clone()))?;
        let route_operation = ServiceOperation::parse(&route.operation)?;
        if route_operation != operation {
            return Err(FocusedServiceError::OperationMismatch {
                route: name,
                requested: operation,
                configured: route_operation,
            });
        }
        self.resolve_declaration(registry, profile_egress, name, route, operation)
    }

    pub(crate) fn inspect(
        &self,
        registry: &ConnectionRegistry,
        exposed_routes: &[String],
        route_name: &str,
    ) -> ServiceRouteStatus {
        let Some(route) = registry.service_routes.get(route_name) else {
            return route_status(
                route_name,
                None,
                false,
                false,
                false,
                false,
                "not configured",
            );
        };
        let operation = ServiceOperation::parse(&route.operation).ok();
        let exposed = exposed_routes
            .iter()
            .any(|candidate| candidate == route_name);
        let Some(connection) = registry.service_connections.get(&route.connection) else {
            return route_status(
                route_name,
                operation,
                true,
                exposed,
                false,
                false,
                "connection is missing",
            );
        };
        let compatible = operation.is_some_and(|operation| {
            self.descriptors
                .contains_key(&(connection.adapter.clone(), operation))
        });
        let credential_declared = connection.credential.is_some();
        let requires_credential = operation
            .and_then(|operation| {
                self.descriptors
                    .get(&(connection.adapter.clone(), operation))
            })
            .is_some_and(|descriptor| descriptor.requires_credential);
        let reason = if !exposed {
            "not exposed by the resolved profile"
        } else if !compatible {
            "adapter does not implement the configured operation"
        } else if requires_credential && !credential_declared {
            "adapter requires a connection-owned credential reference"
        } else {
            return ServiceRouteStatus {
                route: route_name.to_owned(),
                operation,
                configured: true,
                exposed: true,
                compatible: true,
                credential_declared,
                ready: true,
                reason: None,
            };
        };
        route_status(
            route_name,
            operation,
            true,
            exposed,
            compatible,
            credential_declared,
            reason,
        )
    }

    pub(crate) async fn execute(
        &self,
        request: FocusedServiceRequest,
        context: FocusedServiceContext,
    ) -> Result<FocusedServiceResult, FocusedServiceError> {
        request.validate()?;
        let adapter = self.adapters.get(&request.route.adapter).ok_or_else(|| {
            FocusedServiceError::AdapterUnavailable(request.route.adapter.clone())
        })?;
        adapter.execute(request, context).await
    }

    fn resolve_default(
        &self,
        registry: &ConnectionRegistry,
        exposed_routes: &[String],
        operation: ServiceOperation,
    ) -> Result<String, FocusedServiceError> {
        let defaults = exposed_routes
            .iter()
            .filter_map(|name| {
                registry.service_routes.get(name).and_then(|route| {
                    (route.default && route.operation == operation.as_str()).then(|| name.clone())
                })
            })
            .collect::<Vec<_>>();
        match defaults.as_slice() {
            [name] => Ok(name.clone()),
            [] => Err(FocusedServiceError::NoDefault(operation)),
            _ => Err(FocusedServiceError::AmbiguousDefault(operation)),
        }
    }

    fn resolve_declaration(
        &self,
        registry: &ConnectionRegistry,
        profile_egress: &[OutboundDataClass],
        name: String,
        route: &ServiceRouteDeclaration,
        operation: ServiceOperation,
    ) -> Result<ResolvedServiceRoute, FocusedServiceError> {
        let connection = registry
            .service_connections
            .get(&route.connection)
            .ok_or_else(|| FocusedServiceError::MissingConnection(route.connection.clone()))?;
        let descriptor = self
            .descriptors
            .get(&(connection.adapter.clone(), operation))
            .cloned()
            .ok_or_else(|| FocusedServiceError::IncompatibleAdapter {
                adapter: connection.adapter.clone(),
                operation,
            })?;
        if descriptor.requires_credential && connection.credential.is_none() {
            return Err(FocusedServiceError::MissingCredentialReference(
                route.connection.clone(),
            ));
        }
        let route_egress = route
            .egress_policy
            .as_deref()
            .and_then(|policy| registry.egress_policies.get(policy))
            .map(|policy| policy.allowed.iter().copied().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let profile_egress = profile_egress.iter().copied().collect::<BTreeSet<_>>();
        Ok(ResolvedServiceRoute {
            name,
            description: route.description.clone(),
            connection: route.connection.clone(),
            adapter: connection.adapter.clone(),
            base_url: connection.base_url.clone(),
            credential: connection.credential.clone(),
            model: route.model.clone(),
            operation,
            options: route.options.clone(),
            descriptor,
            allowed_outbound: route_egress
                .intersection(&profile_egress)
                .copied()
                .collect(),
            is_default: route.default,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FocusedServiceError {
    UnknownRoute(String),
    RouteNotExposed(String),
    NoDefault(ServiceOperation),
    AmbiguousDefault(ServiceOperation),
    UnsupportedOperation(String),
    OperationMismatch {
        route: String,
        requested: ServiceOperation,
        configured: ServiceOperation,
    },
    MissingConnection(String),
    MissingCredentialReference(String),
    AdapterUnavailable(String),
    IncompatibleAdapter {
        adapter: String,
        operation: ServiceOperation,
    },
    DuplicateAdapter(String),
    DuplicateDescriptor(String, ServiceOperation),
    InvalidDescriptor(String),
    InvalidRequest(&'static str),
    Unauthorized(&'static str),
    Authentication,
    RateLimited,
    Quota,
    ContentPolicy,
    Cancelled,
    Timeout,
    ResponseTooLarge,
    MalformedResponse,
    Artifact(String),
    Transport,
}

impl fmt::Display for FocusedServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRoute(route) => {
                write!(formatter, "unknown focused-service route {route:?}")
            }
            Self::RouteNotExposed(route) => {
                write!(
                    formatter,
                    "focused-service route {route:?} is not exposed by this profile"
                )
            }
            Self::NoDefault(operation) => write!(
                formatter,
                "no default route is exposed for {}; select a route explicitly",
                operation.as_str()
            ),
            Self::AmbiguousDefault(operation) => write!(
                formatter,
                "multiple default routes are exposed for {}; select one explicitly",
                operation.as_str()
            ),
            Self::UnsupportedOperation(operation) => {
                write!(
                    formatter,
                    "unsupported focused-service operation {operation:?}"
                )
            }
            Self::OperationMismatch {
                route,
                requested,
                configured,
            } => write!(
                formatter,
                "route {route:?} is {} rather than requested {}",
                configured.as_str(),
                requested.as_str()
            ),
            Self::MissingConnection(connection) => {
                write!(
                    formatter,
                    "focused-service connection {connection:?} is missing"
                )
            }
            Self::MissingCredentialReference(connection) => write!(
                formatter,
                "focused-service connection {connection:?} requires a credential reference"
            ),
            Self::AdapterUnavailable(adapter) => {
                write!(
                    formatter,
                    "focused-service adapter {adapter:?} is unavailable"
                )
            }
            Self::IncompatibleAdapter { adapter, operation } => write!(
                formatter,
                "adapter {adapter:?} does not implement {}",
                operation.as_str()
            ),
            Self::DuplicateAdapter(adapter) => {
                write!(
                    formatter,
                    "focused-service adapter {adapter:?} is already registered"
                )
            }
            Self::DuplicateDescriptor(adapter, operation) => write!(
                formatter,
                "adapter {adapter:?} declares {} more than once",
                operation.as_str()
            ),
            Self::InvalidDescriptor(adapter) => {
                write!(formatter, "adapter {adapter:?} has an invalid descriptor")
            }
            Self::InvalidRequest(reason) | Self::Unauthorized(reason) => {
                formatter.write_str(reason)
            }
            Self::Authentication => formatter.write_str("focused-service authentication failed"),
            Self::RateLimited => formatter.write_str("focused-service request was rate limited"),
            Self::Quota => formatter.write_str("focused-service quota is exhausted"),
            Self::ContentPolicy => {
                formatter.write_str("focused-service content policy rejected the request")
            }
            Self::Cancelled => formatter.write_str("focused-service request was cancelled"),
            Self::Timeout => formatter.write_str("focused-service request timed out"),
            Self::ResponseTooLarge => {
                formatter.write_str("focused-service response exceeded its bound")
            }
            Self::MalformedResponse => {
                formatter.write_str("focused-service response was malformed")
            }
            Self::Artifact(reason) => {
                write!(formatter, "focused-service artifact failed: {reason}")
            }
            Self::Transport => formatter.write_str("focused-service transport failed"),
        }
    }
}

impl Error for FocusedServiceError {}

fn route_status(
    route: &str,
    operation: Option<ServiceOperation>,
    configured: bool,
    exposed: bool,
    compatible: bool,
    credential_declared: bool,
    reason: &str,
) -> ServiceRouteStatus {
    ServiceRouteStatus {
        route: route.to_owned(),
        operation,
        configured,
        exposed,
        compatible,
        credential_declared,
        ready: false,
        reason: Some(reason.to_owned()),
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_format(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'.' | b'-'))
}

#[cfg(test)]
mod tests;
