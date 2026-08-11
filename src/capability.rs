//! Deterministic capability discovery and immutable agent composition.
//!
//! Discovery is pure metadata. Authorization still happens at invocation
//! time in the existing permission broker; resolving a capability never grants
//! access to a path, process, or network.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CapabilityIdError> {
        let value = value.into();
        validate_id(&value).map(|()| Self(value))
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalToolId(String);

impl LogicalToolId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CapabilityIdError> {
        let value = value.into();
        validate_id(&value).map(|()| Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LogicalToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCapabilitySnapshot {
    capabilities: BTreeSet<CapabilityId>,
    tools: BTreeSet<LogicalToolId>,
}

impl AgentCapabilitySnapshot {
    pub fn new(capabilities: BTreeSet<CapabilityId>, tools: BTreeSet<LogicalToolId>) -> Self {
        Self {
            capabilities,
            tools,
        }
    }

    #[cfg(test)]
    fn contains(&self, id: &CapabilityId) -> bool {
        self.capabilities.contains(id)
    }

    pub fn capabilities(&self) -> &BTreeSet<CapabilityId> {
        &self.capabilities
    }

    pub fn tool_ids(&self) -> &BTreeSet<LogicalToolId> {
        &self.tools
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinCapabilityError {
    InvalidId(String),
    Unknown(String),
    Resolution(String),
}

impl fmt::Display for BuiltinCapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid capability id {id:?}"),
            Self::Unknown(id) => write!(f, "unknown built-in capability {id:?}"),
            Self::Resolution(reason) => write!(f, "could not resolve capabilities: {reason}"),
        }
    }
}

impl std::error::Error for BuiltinCapabilityError {}

impl BuiltinCapabilityError {
    pub(crate) fn capability(&self) -> Option<&str> {
        match self {
            Self::InvalidId(id) | Self::Unknown(id) => Some(id),
            Self::Resolution(_) => None,
        }
    }
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
    #[cfg(test)]
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
    let mut tool_ids = BTreeSet::<LogicalToolId>::new();
    for provider in &providers {
        for tool in &provider.tools {
            if !capabilities.contains_key(&tool.capability) {
                return Err(ResolutionError::ToolCapabilityMissing {
                    tool: tool.id.clone(),
                    capability: tool.capability.clone(),
                });
            }
            if !resolved.contains(&tool.capability) {
                continue;
            }
            tool_providers
                .entry(tool.id.clone())
                .or_default()
                .push(provider.provider_id.clone());
            tool_ids.insert(tool.id.clone());
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

    Ok(ResolvedCapabilities {
        snapshot: AgentCapabilitySnapshot::new(resolved, tool_ids),
        #[cfg(test)]
        unavailable,
    })
}

/// Resolve the capabilities shipped in the Xana executable before concrete
/// runtime tools are constructed. The returned names are the only built-ins
/// exposed in the production tool registry.
pub(crate) fn resolve_builtin_tool_names() -> Result<BTreeSet<String>, ResolutionError> {
    Ok(resolve_builtin_capability_snapshot(None)
        .map_err(|error| ResolutionError::InvalidProvider(error.to_string()))?
        .tool_ids()
        .iter()
        .map(ToString::to_string)
        .collect())
}

pub(crate) fn resolve_builtin_capability_snapshot(
    selection: Option<&[String]>,
) -> Result<AgentCapabilitySnapshot, BuiltinCapabilityError> {
    let provider_id = ProviderId::parse("xana.builtins")
        .map_err(|error| BuiltinCapabilityError::InvalidId(error.to_string()))?;
    let mut capabilities = Vec::new();
    let mut tools = Vec::new();
    let known = BUILTIN_CAPABILITIES
        .iter()
        .map(|(capability, _)| *capability)
        .collect::<BTreeSet<_>>();
    let selected = selection
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    if !known.contains(value.as_str()) {
                        return Err(BuiltinCapabilityError::Unknown(value.clone()));
                    }
                    CapabilityId::parse(value.clone())
                        .map_err(|_| BuiltinCapabilityError::InvalidId(value.clone()))
                })
                .collect::<Result<BTreeSet<_>, _>>()
        })
        .transpose()?;
    for (capability_name, tool_name) in BUILTIN_CAPABILITIES {
        let capability = CapabilityId::parse(*capability_name)
            .map_err(|_| BuiltinCapabilityError::InvalidId((*capability_name).to_owned()))?;
        let tool_id = LogicalToolId::parse(*tool_name)
            .map_err(|_| BuiltinCapabilityError::InvalidId((*tool_name).to_owned()))?;
        capabilities.push(CapabilityDescriptor {
            id: capability.clone(),
            required: Vec::new(),
            optional: Vec::new(),
        });
        tools.push(ToolContribution {
            id: tool_id,
            capability,
        });
    }
    if selected.as_ref().is_some_and(BTreeSet::is_empty) {
        return Ok(AgentCapabilitySnapshot::new(
            BTreeSet::new(),
            BTreeSet::new(),
        ));
    }
    let resolved = resolve(ResolutionInput {
        providers: vec![ProviderDescriptor {
            provider_id,
            capabilities,
            tools,
        }],
        enabled: BTreeSet::new(),
        selected: selected.unwrap_or_default(),
    })
    .map_err(|error| BuiltinCapabilityError::Resolution(error.to_string()))?;
    Ok(resolved.snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn tool_without_a_declared_capability_is_rejected() {
        let result = resolve(ResolutionInput {
            providers: vec![ProviderDescriptor {
                provider_id: ProviderId::parse("p").unwrap(),
                capabilities: vec![],
                tools: vec![ToolContribution {
                    id: LogicalToolId::parse("tool.read").unwrap(),
                    capability: id("missing"),
                }],
            }],
            enabled: BTreeSet::new(),
            selected: BTreeSet::new(),
        });
        assert!(matches!(
            result,
            Err(ResolutionError::ToolCapabilityMissing { .. })
        ));
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

    #[test]
    fn builtin_profile_selection_distinguishes_all_none_and_unknown() {
        let all = resolve_builtin_capability_snapshot(None).expect("all built-ins");
        assert_eq!(all.tool_ids().len(), BUILTIN_CAPABILITIES.len());

        let none = resolve_builtin_capability_snapshot(Some(&[])).expect("no built-ins");
        assert!(none.capabilities().is_empty());
        assert!(none.tool_ids().is_empty());

        let selected = resolve_builtin_capability_snapshot(Some(&["fs.read".into()]))
            .expect("selected built-in");
        assert_eq!(
            selected
                .capabilities()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["fs.read"]
        );
        assert!(matches!(
            resolve_builtin_capability_snapshot(Some(&["future.missing".into()])),
            Err(BuiltinCapabilityError::Unknown(id)) if id == "future.missing"
        ));
    }
}
