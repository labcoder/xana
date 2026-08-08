//! Deterministic capability discovery and immutable agent composition.
//!
//! Discovery is pure metadata. Authorization still happens at invocation
//! time in the existing permission broker; resolving a capability never grants
//! access to a path, process, or network.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use xana_core::{AgentCapabilitySnapshot, CapabilityId, LogicalToolId, Tool, ToolRegistry};

const BUILTIN_CAPABILITIES: &[(&str, &str)] = &[
    ("fs.read", "read_file"),
    ("fs.list", "list_files"),
    ("fs.write", "edit_file"),
    ("process.execute", "run_command"),
    ("document.extract", "read_document"),
    ("xana.docs.read", "xana_docs"),
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CapabilityIdError> {
        let value = value.into();
        validate_id(&value).map(|()| Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityIdError(String);

impl fmt::Display for CapabilityIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid stable identifier {:?}", self.0)
    }
}

impl std::error::Error for CapabilityIdError {}

fn validate_id(value: &str) -> Result<(), CapabilityIdError> {
    if value.is_empty()
        || !value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'
                        || byte == b'-'
                })
        })
    {
        Err(CapabilityIdError(value.to_owned()))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub required: Vec<CapabilityId>,
    pub optional: Vec<CapabilityId>,
}

#[derive(Clone)]
pub struct ToolContribution {
    pub id: LogicalToolId,
    pub capability: CapabilityId,
    pub implementation: Arc<dyn Tool>,
}

#[derive(Clone)]
pub struct ProviderDescriptor {
    pub provider_id: ProviderId,
    pub capabilities: Vec<CapabilityDescriptor>,
    pub tools: Vec<ToolContribution>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    Disabled,
    NotSelected,
    MissingRequired { dependency: CapabilityId },
    DependencyUnavailable { dependency: CapabilityId },
    Cycle { path: Vec<CapabilityId> },
    DuplicateCapability { providers: Vec<ProviderId> },
    DuplicateTool { providers: Vec<ProviderId> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    InvalidProvider(String),
    DuplicateCapability {
        id: CapabilityId,
        providers: Vec<ProviderId>,
    },
    DuplicateTool {
        id: LogicalToolId,
        providers: Vec<ProviderId>,
    },
    ToolCapabilityMissing {
        tool: LogicalToolId,
        capability: CapabilityId,
    },
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProvider(message) => f.write_str(message),
            Self::DuplicateCapability { id, providers } => {
                write!(f, "capability {id} is provided by {}", join_ids(providers))
            }
            Self::DuplicateTool { id, providers } => {
                write!(f, "tool {id} is provided by {}", join_ids(providers))
            }
            Self::ToolCapabilityMissing { tool, capability } => write!(
                f,
                "tool {tool} requires unavailable capability {capability}"
            ),
        }
    }
}

impl std::error::Error for ResolutionError {}

fn join_ids<T: fmt::Display>(ids: &[T]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub struct ResolutionInput {
    pub providers: Vec<ProviderDescriptor>,
    pub enabled: BTreeSet<CapabilityId>,
    pub selected: BTreeSet<CapabilityId>,
}

pub struct ResolvedCapabilities {
    pub snapshot: AgentCapabilitySnapshot,
    pub unavailable: BTreeMap<CapabilityId, UnavailableReason>,
}

pub fn resolve(input: ResolutionInput) -> Result<ResolvedCapabilities, ResolutionError> {
    let mut capabilities = BTreeMap::<CapabilityId, (ProviderId, CapabilityDescriptor)>::new();
    let mut capability_providers = BTreeMap::<CapabilityId, Vec<ProviderId>>::new();

    let mut providers = input.providers;
    providers.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    for provider in &providers {
        if provider.capabilities.is_empty() && provider.tools.is_empty() {
            return Err(ResolutionError::InvalidProvider(format!(
                "provider {} contributes nothing",
                provider.provider_id
            )));
        }
        for descriptor in &provider.capabilities {
            capability_providers
                .entry(descriptor.id.clone())
                .or_default()
                .push(provider.provider_id.clone());
            capabilities
                .entry(descriptor.id.clone())
                .or_insert_with(|| (provider.provider_id.clone(), descriptor.clone()));
        }
    }

    for (id, providers) in &capability_providers {
        if providers.len() > 1 {
            return Err(ResolutionError::DuplicateCapability {
                id: id.clone(),
                providers: providers.clone(),
            });
        }
    }

    let enabled_all = input.enabled.is_empty();
    let selected_all = input.selected.is_empty();
    let mut unavailable = BTreeMap::new();
    let mut resolved = BTreeSet::new();
    let mut visiting = Vec::<CapabilityId>::new();

    fn visit(
        id: &CapabilityId,
        capabilities: &BTreeMap<CapabilityId, (ProviderId, CapabilityDescriptor)>,
        enabled: &BTreeSet<CapabilityId>,
        enabled_all: bool,
        unavailable: &mut BTreeMap<CapabilityId, UnavailableReason>,
        resolved: &mut BTreeSet<CapabilityId>,
        visiting: &mut Vec<CapabilityId>,
    ) -> bool {
        if resolved.contains(id) {
            return true;
        }
        if let Some(reason) = unavailable.get(id) {
            return !matches!(
                reason,
                UnavailableReason::Disabled | UnavailableReason::NotSelected
            );
        }
        if let Some(position) = visiting.iter().position(|item| item == id) {
            let mut path = visiting[position..].to_vec();
            path.push(id.clone());
            unavailable.insert(id.clone(), UnavailableReason::Cycle { path });
            return false;
        }
        let Some((_, descriptor)) = capabilities.get(id) else {
            unavailable.insert(
                id.clone(),
                UnavailableReason::MissingRequired {
                    dependency: id.clone(),
                },
            );
            return false;
        };
        if !enabled_all && !enabled.contains(id) {
            unavailable.insert(id.clone(), UnavailableReason::Disabled);
            return false;
        }
        visiting.push(id.clone());
        let mut ok = true;
        for dependency in &descriptor.required {
            if !visit(
                dependency,
                capabilities,
                enabled,
                enabled_all,
                unavailable,
                resolved,
                visiting,
            ) {
                unavailable.insert(
                    id.clone(),
                    UnavailableReason::DependencyUnavailable {
                        dependency: dependency.clone(),
                    },
                );
                ok = false;
                break;
            }
        }
        if ok {
            for dependency in &descriptor.optional {
                let _ = visit(
                    dependency,
                    capabilities,
                    enabled,
                    enabled_all,
                    unavailable,
                    resolved,
                    visiting,
                );
            }
            resolved.insert(id.clone());
        }
        visiting.pop();
        ok
    }

    let candidates = capabilities.keys().cloned().collect::<Vec<_>>();
    for id in candidates {
        if !selected_all && !input.selected.contains(&id) {
            unavailable.insert(id, UnavailableReason::NotSelected);
            continue;
        }
        let _ = visit(
            &id,
            &capabilities,
            &input.enabled,
            enabled_all,
            &mut unavailable,
            &mut resolved,
            &mut visiting,
        );
    }

    let mut tool_providers = BTreeMap::<LogicalToolId, Vec<ProviderId>>::new();
    let mut tool_impls = BTreeMap::<LogicalToolId, Arc<dyn Tool>>::new();
    for provider in &providers {
        for tool in &provider.tools {
            if !resolved.contains(&tool.capability) {
                continue;
            }
            tool_providers
                .entry(tool.id.clone())
                .or_default()
                .push(provider.provider_id.clone());
            tool_impls
                .entry(tool.id.clone())
                .or_insert_with(|| Arc::clone(&tool.implementation));
        }
    }
    for (id, providers) in &tool_providers {
        if providers.len() > 1 {
            return Err(ResolutionError::DuplicateTool {
                id: id.clone(),
                providers: providers.clone(),
            });
        }
    }

    let tools = ToolRegistry::new(tool_impls.into_values().collect())
        .map_err(ResolutionError::InvalidProvider)?;
    Ok(ResolvedCapabilities {
        snapshot: AgentCapabilitySnapshot::new(resolved, tools),
        unavailable,
    })
}

/// Resolve the capabilities shipped in the Xana executable before concrete
/// runtime tools are constructed. The returned names are the only built-ins
/// exposed in the production tool registry.
pub(crate) fn resolve_builtin_tool_names() -> Result<BTreeSet<String>, ResolutionError> {
    let provider_id = ProviderId::parse("xana.builtins")
        .map_err(|error| ResolutionError::InvalidProvider(error.to_string()))?;
    let mut capabilities = Vec::new();
    let mut tools = Vec::new();
    for (capability, tool) in BUILTIN_CAPABILITIES {
        let capability = CapabilityId::parse(*capability)
            .map_err(|error| ResolutionError::InvalidProvider(error.to_string()))?;
        let tool_id = LogicalToolId::parse(*tool)
            .map_err(|error| ResolutionError::InvalidProvider(error.to_string()))?;
        capabilities.push(CapabilityDescriptor {
            id: capability.clone(),
            required: Vec::new(),
            optional: Vec::new(),
        });
        tools.push(ToolContribution {
            id: tool_id.clone(),
            capability,
            implementation: Arc::new(BuiltinMarkerTool { id: tool_id }),
        });
    }
    let resolved = resolve(ResolutionInput {
        providers: vec![ProviderDescriptor {
            provider_id,
            capabilities,
            tools,
        }],
        enabled: BTreeSet::new(),
        selected: BTreeSet::new(),
    })?;
    Ok(resolved
        .snapshot
        .tool_definitions()
        .into_iter()
        .map(|definition| definition.name.to_string())
        .collect())
}

struct BuiltinMarkerTool {
    id: LogicalToolId,
}

impl Tool for BuiltinMarkerTool {
    fn definition(&self) -> xana_core::ToolDefinition {
        xana_core::ToolDefinition {
            name: self.id.clone(),
            contract_version: 1,
            description: "runtime-owned built-in capability marker".into(),
            parameters: serde_json::json!({"type":"object"}),
            effect_class: xana_core::EffectClass::Read,
            replay_safety: xana_core::ReplaySafety::Safe,
        }
    }

    fn invoke<'a>(
        &'a self,
        _: &'a serde_json::Value,
    ) -> futures::future::BoxFuture<'a, Result<String, String>> {
        Box::pin(async { Err("capability markers are not executable".into()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use serde_json::json;

    struct FakeTool {
        id: LogicalToolId,
    }
    impl Tool for FakeTool {
        fn definition(&self) -> xana_core::ToolDefinition {
            xana_core::ToolDefinition {
                name: self.id.clone(),
                contract_version: 1,
                description: "fake".into(),
                parameters: json!({"type":"object"}),
                effect_class: xana_core::EffectClass::Read,
                replay_safety: xana_core::ReplaySafety::Safe,
            }
        }
        fn invoke<'a>(&'a self, _: &'a serde_json::Value) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async { Ok("ok".into()) })
        }
    }
    fn id(value: &str) -> CapabilityId {
        CapabilityId::parse(value).unwrap()
    }
    fn provider(name: &str, descriptors: Vec<CapabilityDescriptor>) -> ProviderDescriptor {
        ProviderDescriptor {
            provider_id: ProviderId::parse(name).unwrap(),
            capabilities: descriptors,
            tools: vec![],
        }
    }

    #[test]
    fn dependencies_resolve_independent_of_input_order() {
        let a = CapabilityDescriptor {
            id: id("a"),
            required: vec![id("b")],
            optional: vec![],
        };
        let b = CapabilityDescriptor {
            id: id("b"),
            required: vec![],
            optional: vec![],
        };
        let one = resolve(ResolutionInput {
            providers: vec![provider("p", vec![a.clone(), b.clone()])],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        })
        .unwrap();
        let two = resolve(ResolutionInput {
            providers: vec![provider("p", vec![b, a])],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        })
        .unwrap();
        assert_eq!(one.snapshot.capabilities(), two.snapshot.capabilities());
    }

    #[test]
    fn missing_required_dependency_is_typed() {
        let result = resolve(ResolutionInput {
            providers: vec![provider(
                "p",
                vec![CapabilityDescriptor {
                    id: id("a"),
                    required: vec![id("missing")],
                    optional: vec![],
                }],
            )],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        })
        .unwrap();
        assert!(matches!(
            result.unavailable.get(&id("a")),
            Some(UnavailableReason::DependencyUnavailable { .. })
        ));
    }

    #[test]
    fn optional_dependency_does_not_hide_consumer() {
        let result = resolve(ResolutionInput {
            providers: vec![provider(
                "p",
                vec![CapabilityDescriptor {
                    id: id("a"),
                    required: vec![],
                    optional: vec![id("missing")],
                }],
            )],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        })
        .unwrap();
        assert!(result.snapshot.contains(&id("a")));
    }

    #[test]
    fn duplicate_capability_is_rejected() {
        let result = resolve(ResolutionInput {
            providers: vec![
                provider(
                    "a",
                    vec![CapabilityDescriptor {
                        id: id("x"),
                        required: vec![],
                        optional: vec![],
                    }],
                ),
                provider(
                    "b",
                    vec![CapabilityDescriptor {
                        id: id("x"),
                        required: vec![],
                        optional: vec![],
                    }],
                ),
            ],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        });
        assert!(matches!(
            result,
            Err(ResolutionError::DuplicateCapability { .. })
        ));
    }

    #[test]
    fn duplicate_tool_is_rejected() {
        let tool_id = LogicalToolId::parse("tool.read").unwrap();
        let tool = |provider: &str, capability_name: &str| ProviderDescriptor {
            provider_id: ProviderId::parse(provider).unwrap(),
            capabilities: vec![CapabilityDescriptor {
                id: id(capability_name),
                required: vec![],
                optional: vec![],
            }],
            tools: vec![ToolContribution {
                id: tool_id.clone(),
                capability: id(capability_name),
                implementation: Arc::new(FakeTool {
                    id: tool_id.clone(),
                }),
            }],
        };
        let result = resolve(ResolutionInput {
            providers: vec![tool("a", "x"), tool("b", "y")],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        });
        assert!(matches!(result, Err(ResolutionError::DuplicateTool { .. })));
    }

    #[test]
    fn production_builtin_names_are_resolved_from_capability_descriptors() {
        assert_eq!(
            resolve_builtin_tool_names().unwrap(),
            crate::tool::BUILTIN_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        );
    }
}
