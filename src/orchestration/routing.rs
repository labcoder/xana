use super::types::ChildRestrictions;
use crate::capability::AgentCapabilitySnapshot;
use crate::{
    capability::resolve_builtin_capability_snapshot,
    config::{ConnectionRegistry, OrchestrationLimits, PermissionMode, ProviderKind},
    credential::CredentialAvailability,
    model::{ModelDescriptor, ModelManager, ReasoningSummary},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub(crate) hard_token_limit: Option<u64>,
    pub(crate) hard_spend_microusd: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnforcementCapabilities {
    pub(crate) hard_tokens: bool,
    pub(crate) hard_spend: bool,
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
    PermissionModeUnavailable {
        route: String,
        owner: ExecutionOwner,
        permission_mode: PermissionMode,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestrictionError {
    AuthorityWidening,
    BoundWidening {
        field: &'static str,
        requested: u64,
        maximum: u64,
    },
    UnsupportedHardLimit {
        field: &'static str,
    },
    PermissionModeUnavailable {
        owner: ExecutionOwner,
        permission_mode: PermissionMode,
        reason: &'static str,
    },
}

impl fmt::Display for RestrictionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityWidening => formatter.write_str(
                "child permission restriction would widen its resolved parent/profile ceiling",
            ),
            Self::BoundWidening {
                field,
                requested,
                maximum,
            } => write!(
                formatter,
                "child restriction {field}={requested} exceeds resolved maximum {maximum}"
            ),
            Self::UnsupportedHardLimit { field } => write!(
                formatter,
                "child requests hard {field}, but this execution owner exposes no enforceable pre-request control or interruptible live meter"
            ),
            Self::PermissionModeUnavailable {
                owner,
                permission_mode,
                reason,
            } => write!(
                formatter,
                "child execution owner {} cannot enforce permission mode {permission_mode:?}: {reason}",
                owner.as_str()
            ),
        }
    }
}

impl Error for RestrictionError {}

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
            Self::PermissionModeUnavailable {
                route,
                owner,
                permission_mode,
                reason,
            } => write!(
                f,
                "route {route:?} cannot use execution owner {} with permission mode {permission_mode:?}: {reason}",
                owner.as_str()
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
        let parent_profile = self
            .registry
            .profiles
            .get(&self.registry.default_profile)
            .ok_or_else(|| {
                RouteResolutionError::InconsistentConfig(format!(
                    "default profile {:?} is missing",
                    self.registry.default_profile
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
        let capabilities = if connection.kind == ProviderKind::Codex {
            if let Some(capability) = profile
                .capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.first())
            {
                return Err(RouteResolutionError::CapabilityUnavailable {
                    route: route.to_owned(),
                    capability: capability.clone(),
                    reason: "managed Codex owns its tool catalog; a child route cannot claim Xana-native capabilities".to_owned(),
                });
            }
            AgentCapabilitySnapshot::new(BTreeSet::new(), BTreeSet::new())
        } else {
            resolve_builtin_capability_snapshot(profile.capabilities.as_deref()).map_err(
                |error| RouteResolutionError::CapabilityUnavailable {
                    route: route.to_owned(),
                    capability: error.capability().unwrap_or("unknown").to_owned(),
                    reason: error.to_string(),
                },
            )?
        };
        let parent_connection = self
            .registry
            .connections
            .get(&parent_profile.connection)
            .ok_or_else(|| {
                RouteResolutionError::InconsistentConfig(format!(
                    "default profile {:?} references missing connection {:?}",
                    parent_profile.id, parent_profile.connection
                ))
            })?;
        let parent_capabilities = if parent_connection.kind == ProviderKind::Codex {
            AgentCapabilitySnapshot::new(BTreeSet::new(), BTreeSet::new())
        } else {
            resolve_builtin_capability_snapshot(parent_profile.capabilities.as_deref()).map_err(
                |error| RouteResolutionError::CapabilityUnavailable {
                    route: route.to_owned(),
                    capability: error.capability().unwrap_or("unknown").to_owned(),
                    reason: format!("default profile capability ceiling is invalid: {error}"),
                },
            )?
        };
        if let Some(capability) = capabilities
            .capabilities()
            .difference(parent_capabilities.capabilities())
            .next()
        {
            return Err(RouteResolutionError::CapabilityUnavailable {
                route: route.to_owned(),
                capability: capability.to_string(),
                reason: format!(
                    "child route capability exceeds the default profile {:?} authority ceiling",
                    parent_profile.id
                ),
            });
        }
        if connection.kind != ProviderKind::Codex
            && !capabilities.tool_ids().is_empty()
            && model.tools == Some(false)
        {
            return Err(RouteResolutionError::ModelToolsUnavailable {
                route: route.to_owned(),
                connection: connection.id.clone(),
                model: model.id.clone(),
            });
        }

        let owner = if connection.kind == ProviderKind::Codex {
            ExecutionOwner::Codex
        } else {
            ExecutionOwner::Native
        };
        let permission_mode = narrow_permission(
            narrow_permission(
                self.registry.permission_mode,
                parent_profile.permission_mode,
            ),
            profile.permission_mode,
        );
        if owner == ExecutionOwner::Codex && permission_mode == PermissionMode::Deny {
            return Err(RouteResolutionError::PermissionModeUnavailable {
                route: route.to_owned(),
                owner,
                permission_mode,
                reason: "the current Codex app-server contract cannot prove that all inner tool effects are disabled",
            });
        }

        Ok(ResolvedAgentConfig {
            route: route.to_owned(),
            profile: profile.id.clone(),
            connection: connection.id.clone(),
            provider_kind: connection.kind,
            owner,
            model,
            reasoning_effort: selection.reasoning_effort,
            reasoning_summary: selection.reasoning_summary,
            capabilities,
            permission_mode,
            max_tool_rounds: profile.max_tool_rounds.min(parent_profile.max_tool_rounds),
            orchestration: intersect_limits(&parent_profile.orchestration, &profile.orchestration),
            hard_token_limit: None,
            hard_spend_microusd: None,
        })
    }
}

pub(crate) fn apply_spawn_restrictions(
    resolved: &mut ResolvedAgentConfig,
    restrictions: &ChildRestrictions,
    enforcement: EnforcementCapabilities,
) -> Result<(), RestrictionError> {
    let mut restricted = resolved.clone();
    if let Some(permission_mode) = restrictions.permission_mode {
        if permission_rank(permission_mode) > permission_rank(restricted.permission_mode) {
            return Err(RestrictionError::AuthorityWidening);
        }
        restricted.permission_mode = permission_mode;
    }
    if restricted.owner == ExecutionOwner::Codex
        && restricted.permission_mode == PermissionMode::Deny
    {
        return Err(RestrictionError::PermissionModeUnavailable {
            owner: restricted.owner,
            permission_mode: restricted.permission_mode,
            reason: "the current Codex app-server contract cannot prove that all inner tool effects are disabled",
        });
    }
    narrow_usize(
        "max_tool_rounds",
        &mut restricted.max_tool_rounds,
        restrictions.max_tool_rounds,
    )?;
    narrow_u64(
        "deadline_seconds",
        &mut restricted.orchestration.deadline_seconds,
        restrictions.deadline_seconds,
    )?;
    narrow_usize(
        "max_context_tokens",
        &mut restricted.orchestration.max_context_tokens,
        restrictions.max_context_tokens,
    )?;
    narrow_usize(
        "max_report_bytes",
        &mut restricted.orchestration.max_report_bytes,
        restrictions.max_report_bytes,
    )?;
    narrow_usize(
        "max_artifact_bytes",
        &mut restricted.orchestration.max_artifact_bytes,
        restrictions.max_artifact_bytes,
    )?;
    if restrictions.hard_token_limit.is_some() && !enforcement.hard_tokens {
        return Err(RestrictionError::UnsupportedHardLimit {
            field: "token limit",
        });
    }
    if restrictions.hard_spend_microusd.is_some() && !enforcement.hard_spend {
        return Err(RestrictionError::UnsupportedHardLimit {
            field: "spend limit",
        });
    }
    restricted.hard_token_limit = restrictions.hard_token_limit;
    restricted.hard_spend_microusd = restrictions.hard_spend_microusd;
    *resolved = restricted;
    Ok(())
}

fn permission_rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::Deny => 0,
        PermissionMode::Ask => 1,
        PermissionMode::Allow => 2,
    }
}

fn narrow_usize(
    field: &'static str,
    value: &mut usize,
    requested: Option<usize>,
) -> Result<(), RestrictionError> {
    if let Some(requested) = requested {
        if requested > *value {
            return Err(RestrictionError::BoundWidening {
                field,
                requested: requested as u64,
                maximum: *value as u64,
            });
        }
        *value = requested;
    }
    Ok(())
}

fn narrow_u64(
    field: &'static str,
    value: &mut u64,
    requested: Option<u64>,
) -> Result<(), RestrictionError> {
    if let Some(requested) = requested {
        if requested > *value {
            return Err(RestrictionError::BoundWidening {
                field,
                requested,
                maximum: *value,
            });
        }
        *value = requested;
    }
    Ok(())
}

fn narrow_permission(global: PermissionMode, profile: Option<PermissionMode>) -> PermissionMode {
    let profile = profile.unwrap_or(global);
    match (global, profile) {
        (PermissionMode::Deny, _) | (_, PermissionMode::Deny) => PermissionMode::Deny,
        (PermissionMode::Ask, _) | (_, PermissionMode::Ask) => PermissionMode::Ask,
        (PermissionMode::Allow, PermissionMode::Allow) => PermissionMode::Allow,
    }
}

fn intersect_limits(
    parent: &OrchestrationLimits,
    child: &OrchestrationLimits,
) -> OrchestrationLimits {
    OrchestrationLimits {
        max_fan_out: parent.max_fan_out.min(child.max_fan_out),
        max_descendants: parent.max_descendants.min(child.max_descendants),
        max_concurrency: parent.max_concurrency.min(child.max_concurrency),
        deadline_seconds: parent.deadline_seconds.min(child.deadline_seconds),
        max_context_tokens: parent.max_context_tokens.min(child.max_context_tokens),
        max_report_bytes: parent.max_report_bytes.min(child.max_report_bytes),
        max_artifact_bytes: parent.max_artifact_bytes.min(child.max_artifact_bytes),
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
    fn request_restrictions_narrow_atomically_and_never_widen_authority() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(
            &config_path,
            ROUTE_CONFIG.replace("permission_mode = \"deny\"", "permission_mode = \"ask\""),
        )
        .expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        let baseline = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve route");

        let mut narrowed = baseline.clone();
        apply_spawn_restrictions(
            &mut narrowed,
            &ChildRestrictions {
                permission_mode: Some(PermissionMode::Deny),
                max_tool_rounds: Some(2),
                deadline_seconds: Some(30),
                max_context_tokens: Some(1_024),
                max_report_bytes: Some(2_048),
                max_artifact_bytes: Some(4_096),
                hard_token_limit: None,
                hard_spend_microusd: None,
            },
            EnforcementCapabilities {
                hard_tokens: false,
                hard_spend: false,
            },
        )
        .expect("narrow restrictions");
        assert_eq!(narrowed.permission_mode, PermissionMode::Deny);
        assert_eq!(narrowed.max_tool_rounds, 2);
        assert_eq!(narrowed.orchestration.deadline_seconds, 30);

        let mut widening = baseline.clone();
        assert_eq!(
            apply_spawn_restrictions(
                &mut widening,
                &ChildRestrictions {
                    permission_mode: Some(PermissionMode::Allow),
                    ..Default::default()
                },
                EnforcementCapabilities {
                    hard_tokens: false,
                    hard_spend: false,
                },
            ),
            Err(RestrictionError::AuthorityWidening)
        );
        assert_eq!(widening, baseline, "a rejected restriction is atomic");
    }

    #[test]
    fn root_profile_is_the_parent_ceiling_for_child_routes() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        let config = ROUTE_CONFIG.replace(
            "[profiles.default]\nconnection = \"local\"\nmodel = \"root\"",
            r#"[profiles.default]
connection = "local"
model = "root"
permission_mode = "deny"
max_tool_rounds = 2

[profiles.default.orchestration]
deadline_seconds = 20
max_context_tokens = 1000"#,
        );
        fs::write(&config_path, config).expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );

        let resolved = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve route");
        assert_eq!(resolved.permission_mode, PermissionMode::Deny);
        assert_eq!(resolved.max_tool_rounds, 2);
        assert_eq!(resolved.orchestration.deadline_seconds, 20);
        assert_eq!(resolved.orchestration.max_context_tokens, 1_000);
    }

    #[test]
    fn child_route_cannot_widen_the_root_capability_ceiling() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        let config = ROUTE_CONFIG.replace(
            "[profiles.default]\nconnection = \"local\"\nmodel = \"root\"",
            "[profiles.default]\nconnection = \"local\"\nmodel = \"root\"\ncapabilities = [\"fs.read\"]",
        );
        fs::write(&config_path, config).expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );

        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(None),
            Err(RouteResolutionError::CapabilityUnavailable {
                capability,
                reason,
                ..
            }) if capability == "xana.docs.read" && reason.contains("authority ceiling")
        ));
    }

    #[test]
    fn hard_usage_limits_require_an_enforcement_capability() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(&config_path, ROUTE_CONFIG).expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        let baseline = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve route");
        let restrictions = ChildRestrictions {
            hard_token_limit: Some(1_000),
            hard_spend_microusd: Some(25_000),
            ..Default::default()
        };

        let mut unsupported = baseline.clone();
        assert!(matches!(
            apply_spawn_restrictions(
                &mut unsupported,
                &restrictions,
                EnforcementCapabilities {
                    hard_tokens: false,
                    hard_spend: false,
                },
            ),
            Err(RestrictionError::UnsupportedHardLimit { .. })
        ));
        assert_eq!(unsupported, baseline);

        let mut supported = baseline;
        apply_spawn_restrictions(
            &mut supported,
            &restrictions,
            EnforcementCapabilities {
                hard_tokens: true,
                hard_spend: true,
            },
        )
        .expect("enforceable limits");
        assert_eq!(supported.hard_token_limit, Some(1_000));
        assert_eq!(supported.hard_spend_microusd, Some(25_000));
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

    #[test]
    fn codex_route_rejects_an_effective_deny_policy() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
version = 3
default_profile = "default"
default_child_route = "worker"
permission_mode = "ask"

[providers.codex]
kind = "codex"
codex_program = "this-program-must-not-run"

[providers.codex.models."gpt-test"]
reasoning = true

[profiles.default]
connection = "codex"
model = "gpt-test"

[profiles.worker]
connection = "codex"
model = "gpt-test"
permission_mode = "deny"
capabilities = []

[routes.worker]
profile = "worker"
"#,
        )
        .expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );

        assert!(matches!(
            RouteResolver::new(&registry, &manager).resolve(None),
            Err(RouteResolutionError::PermissionModeUnavailable {
                owner: ExecutionOwner::Codex,
                permission_mode: PermissionMode::Deny,
                ..
            })
        ));
    }

    #[test]
    fn spawn_restriction_cannot_narrow_codex_to_an_unenforceable_deny_policy() {
        let directory = tempdir().expect("temporary directory");
        let config_path = directory.path().join("config.toml");
        fs::write(
            &config_path,
            ROUTE_CONFIG.replace("permission_mode = \"deny\"", "permission_mode = \"ask\""),
        )
        .expect("write config");
        let registry = XanaConfig::load_registry_from(&config_path).expect("registry");
        let manager = ModelManager::new(
            registry.clone(),
            directory.path().join("cache"),
            directory.path().join("selection.toml"),
        );
        let mut resolved = RouteResolver::new(&registry, &manager)
            .resolve(None)
            .expect("resolve route");
        resolved.owner = ExecutionOwner::Codex;
        let baseline = resolved.clone();

        assert!(matches!(
            apply_spawn_restrictions(
                &mut resolved,
                &ChildRestrictions {
                    permission_mode: Some(PermissionMode::Deny),
                    ..Default::default()
                },
                EnforcementCapabilities {
                    hard_tokens: false,
                    hard_spend: false,
                },
            ),
            Err(RestrictionError::PermissionModeUnavailable {
                owner: ExecutionOwner::Codex,
                permission_mode: PermissionMode::Deny,
                ..
            })
        ));
        assert_eq!(resolved, baseline, "a rejected restriction is atomic");
    }
}
