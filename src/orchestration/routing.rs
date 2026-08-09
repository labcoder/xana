use crate::{
    capability::resolve_builtin_capability_snapshot,
    config::{ConnectionRegistry, OrchestrationLimits, PermissionMode, ProviderKind},
    credential::CredentialAvailability,
    model::{ModelDescriptor, ModelManager, ReasoningSummary},
};
use std::{error::Error, fmt};
use xana_core::AgentCapabilitySnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionOwner {
    Native,
    Codex,
}

impl ExecutionOwner {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Codex => "managed_codex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedAgentConfig {
    pub(crate) route: String,
    pub(crate) profile: String,
    pub(crate) connection: String,
    pub(crate) provider_kind: ProviderKind,
    pub(crate) owner: ExecutionOwner,
    pub(crate) model: ModelDescriptor,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning_summary: Option<ReasoningSummary>,
    pub(crate) capabilities: AgentCapabilitySnapshot,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) max_tool_rounds: usize,
    pub(crate) orchestration: OrchestrationLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteResolutionError {
    MissingDefaultRoute,
    UnknownRoute(String),
    InconsistentConfig(String),
    CredentialUnavailable {
        route: String,
        connection: String,
    },
    ModelUnavailable {
        route: String,
        connection: String,
        model: String,
        reason: String,
    },
    CapabilityUnavailable {
        route: String,
        capability: String,
        reason: String,
    },
    ModelToolsUnavailable {
        route: String,
        connection: String,
        model: String,
    },
}

impl fmt::Display for RouteResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDefaultRoute => f.write_str("no default child route is configured"),
            Self::UnknownRoute(route) => write!(f, "unknown child route {route:?}"),
            Self::InconsistentConfig(reason) => {
                write!(f, "validated route registry is inconsistent: {reason}")
            }
            Self::CredentialUnavailable { route, connection } => write!(
                f,
                "route {route:?} requires an unavailable credential for connection {connection:?}"
            ),
            Self::ModelUnavailable {
                route,
                connection,
                model,
                reason,
            } => write!(
                f,
                "route {route:?} cannot use {connection}/{model}: {reason}"
            ),
            Self::CapabilityUnavailable {
                route,
                capability,
                reason,
            } => write!(
                f,
                "route {route:?} cannot expose capability {capability:?}: {reason}"
            ),
            Self::ModelToolsUnavailable {
                route,
                connection,
                model,
            } => write!(
                f,
                "route {route:?} selects tools but model {connection}/{model} declares tools unavailable"
            ),
        }
    }
}

impl Error for RouteResolutionError {}

pub(crate) struct RouteResolver<'a> {
    registry: &'a ConnectionRegistry,
    models: &'a ModelManager,
}

impl<'a> RouteResolver<'a> {
    pub(crate) fn new(registry: &'a ConnectionRegistry, models: &'a ModelManager) -> Self {
        Self { registry, models }
    }

    pub(crate) fn resolve(
        &self,
        requested: Option<&str>,
    ) -> Result<ResolvedAgentConfig, RouteResolutionError> {
        let route = requested
            .or(self.registry.default_child_route.as_deref())
            .ok_or(RouteResolutionError::MissingDefaultRoute)?;
        let route_config = self
            .registry
            .routes
            .get(route)
            .ok_or_else(|| RouteResolutionError::UnknownRoute(route.to_owned()))?;
        let profile = self
            .registry
            .profiles
            .get(&route_config.profile)
            .ok_or_else(|| {
                RouteResolutionError::InconsistentConfig(format!(
                    "route {route:?} references missing profile {:?}",
                    route_config.profile
                ))
            })?;
        let connection = self
            .registry
            .connections
            .get(&profile.connection)
            .ok_or_else(|| {
                RouteResolutionError::InconsistentConfig(format!(
                    "profile {:?} references missing connection {:?}",
                    profile.id, profile.connection
                ))
            })?;

        if self
            .models
            .credential_availability(connection)
            .map_err(|_error| RouteResolutionError::CredentialUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
            })?
            == CredentialAvailability::Missing
        {
            return Err(RouteResolutionError::CredentialUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
            });
        }

        let summary = profile
            .reasoning_summary
            .as_deref()
            .map(str::parse::<ReasoningSummary>)
            .transpose()
            .map_err(|error| RouteResolutionError::ModelUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
                model: profile.model.clone(),
                reason: error.to_string(),
            })?;
        let selection = self
            .models
            .validate_candidate(
                &connection.id,
                &profile.model,
                profile.reasoning_effort.clone(),
                summary,
            )
            .map_err(|error| RouteResolutionError::ModelUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
                model: profile.model.clone(),
                reason: error.to_string(),
            })?;
        let model = self
            .models
            .descriptor(&connection.id, &profile.model)
            .map_err(|error| RouteResolutionError::ModelUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
                model: profile.model.clone(),
                reason: error.to_string(),
            })?;
        let capabilities = resolve_builtin_capability_snapshot(profile.capabilities.as_deref())
            .map_err(|error| RouteResolutionError::CapabilityUnavailable {
                route: route.to_owned(),
                capability: error.capability().unwrap_or("unknown").to_owned(),
                reason: error.to_string(),
            })?;
        if !capabilities.tool_ids().is_empty() && model.tools == Some(false) {
            return Err(RouteResolutionError::ModelToolsUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
                model: model.id.clone(),
            });
        }

        Ok(ResolvedAgentConfig {
            route: route.to_owned(),
            profile: profile.id.clone(),
            connection: connection.id.clone(),
            provider_kind: connection.kind,
            owner: if connection.kind == ProviderKind::Codex {
                ExecutionOwner::Codex
            } else {
                ExecutionOwner::Native
            },
            model,
            reasoning_effort: selection.reasoning_effort,
            reasoning_summary: selection.reasoning_summary,
            capabilities,
            permission_mode: narrow_permission(
                self.registry.permission_mode,
                profile.permission_mode,
            ),
            max_tool_rounds: profile.max_tool_rounds,
            orchestration: profile.orchestration.clone(),
        })
    }
}

fn narrow_permission(global: PermissionMode, profile: Option<PermissionMode>) -> PermissionMode {
    let profile = profile.unwrap_or(global);
    match (global, profile) {
        (PermissionMode::Deny, _) | (_, PermissionMode::Deny) => PermissionMode::Deny,
        (PermissionMode::Ask, _) | (_, PermissionMode::Ask) => PermissionMode::Ask,
        (PermissionMode::Allow, PermissionMode::Allow) => PermissionMode::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XanaConfig;
    use std::fs;
    use tempfile::tempdir;

    const ROUTE_CONFIG: &str = r#"
version = 3
default_profile = "default"
default_child_route = "worker"
permission_mode = "ask"

[providers.local]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[providers.local.models.root]
tools = true

[providers.local.models.worker]
tools = true

[profiles.default]
connection = "local"
model = "root"

[profiles.worker]
connection = "local"
model = "worker"
capabilities = ["fs.read", "xana.docs.read"]
permission_mode = "deny"
max_tool_rounds = 4

[routes.worker]
profile = "worker"
"#;

    #[test]
    fn exact_route_resolves_one_immutable_native_configuration() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(&config_path, ROUTE_CONFIG).expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        manager
            .select("local", "root")
            .expect("persist unrelated root model selection");

        let resolved = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve default child route");

        assert_eq!(resolved.route, "worker");
        assert_eq!(resolved.profile, "worker");
        assert_eq!(resolved.connection, "local");
        assert_eq!(resolved.provider_kind, ProviderKind::OpenAiCompat);
        assert_eq!(resolved.owner, ExecutionOwner::Native);
        assert_eq!(resolved.model.id, "worker");
        assert_eq!(resolved.permission_mode, PermissionMode::Deny);
        assert_eq!(resolved.max_tool_rounds, 4);
        assert_eq!(
            resolved
                .capabilities
                .capabilities()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["fs.read", "xana.docs.read"]
        );
    }

    #[test]
    fn route_failures_name_unknown_routes_capabilities_tools_and_credentials() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(&config_path, ROUTE_CONFIG).expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(Some("missing")),
            Err(RouteResolutionError::UnknownRoute(route)) if route == "missing"
        ));

        let unavailable =
            ROUTE_CONFIG.replace("[\"fs.read\", \"xana.docs.read\"]", "[\"future.missing\"]");
        fs::write(&config_path, unavailable).expect("write unavailable capability");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache-two"),
            directory.path().join("selection-two.toml"),
        );
        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(None),
            Err(RouteResolutionError::CapabilityUnavailable { capability, .. })
                if capability == "future.missing"
        ));

        let no_tools = ROUTE_CONFIG.replacen(
            "[providers.local.models.worker]\ntools = true",
            "[providers.local.models.worker]\ntools = false",
            1,
        );
        fs::write(&config_path, no_tools).expect("write no-tools model");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache-three"),
            directory.path().join("selection-three.toml"),
        );
        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(None),
            Err(RouteResolutionError::ModelToolsUnavailable { model, .. }) if model == "worker"
        ));

        let missing_credential = ROUTE_CONFIG.replace(
            "base_url = \"http://localhost:11434/v1\"",
            "base_url = \"http://localhost:11434/v1\"\ncredential = { source = \"environment\", variable = \"XANA_ROUTE_TEST_GUARANTEED_MISSING_9B7E\" }",
        );
        fs::write(&config_path, missing_credential).expect("write missing credential");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache-four"),
            directory.path().join("selection-four.toml"),
        );
        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(None),
            Err(RouteResolutionError::CredentialUnavailable { connection, .. })
                if connection == "local"
        ));
    }

    #[test]
    fn profile_permission_can_only_narrow_the_global_ceiling() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(
            &config_path,
            ROUTE_CONFIG.replace("permission_mode = \"deny\"", "permission_mode = \"allow\""),
        )
        .expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );

        let resolved = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve route");

        assert_eq!(resolved.permission_mode, PermissionMode::Ask);
    }

    #[test]
    fn codex_route_resolves_from_cached_configuration_without_spawning_codex() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
version = 3
default_profile = "default"
default_child_route = "planner"
permission_mode = "ask"

[providers.codex]
kind = "codex"
codex_program = "this-program-must-not-run"

[providers.codex.models."gpt-5.6-sol"]
reasoning = true

[profiles.default]
connection = "codex"
model = "gpt-5.6-sol"

[profiles.planner]
connection = "codex"
model = "gpt-5.6-sol"
reasoning_effort = "xhigh"
reasoning_summary = "detailed"
capabilities = []

[routes.planner]
profile = "planner"
"#,
        )
        .expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );

        let resolved = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve managed route without process I/O");

        assert_eq!(resolved.owner, ExecutionOwner::Codex);
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(resolved.reasoning_summary, Some(ReasoningSummary::Detailed));
        assert!(resolved.capabilities.tool_ids().is_empty());
    }
}
