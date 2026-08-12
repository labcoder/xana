use super::{PermissionRequest, PermissionRule, PermissionScope, PolicyDecision};
use crate::tool::{BUILTIN_TOOL_NAMES, EffectClass};
use std::{collections::HashSet, error::Error, fmt, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Evaluation {
    Denied { rule_ids: Vec<String> },
    Ask { rule_ids: Vec<String> },
    AllowedByPolicy { rule_ids: Vec<String> },
    AllowedBySessionGrant { grant_id: String },
}

impl Evaluation {
    pub(crate) fn policy_decision(&self) -> PolicyDecision {
        match self {
            Self::Denied { .. } => PolicyDecision::Deny,
            Self::Ask { .. } | Self::AllowedBySessionGrant { .. } => PolicyDecision::Ask,
            Self::AllowedByPolicy { .. } => PolicyDecision::Allow,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyExplanation {
    pub(crate) matched_rule_ids: Vec<String>,
    pub(crate) winning_decision: PolicyDecision,
}

pub(crate) struct PermissionPolicy {
    default: PolicyDecision,
    rules: Vec<PermissionRule>,
}

impl PermissionPolicy {
    pub(crate) fn validate_rules(rules: &[PermissionRule]) -> Result<(), PolicyError> {
        let mut ids = HashSet::new();
        for rule in rules {
            let id = rule.id.trim();
            if id.is_empty() {
                return Err(PolicyError::BlankRuleId);
            }
            if !ids.insert(id) {
                return Err(PolicyError::DuplicateRuleId(id.to_owned()));
            }
            if rule.tool.is_none()
                && rule.effect.is_none()
                && rule.workspace.is_none()
                && rule.command.is_none()
            {
                return Err(PolicyError::NoMatchers(id.to_owned()));
            }
            if let Some(tool) = &rule.tool
                && !BUILTIN_TOOL_NAMES.contains(&tool.as_str())
            {
                return Err(PolicyError::UnknownTool {
                    rule_id: id.to_owned(),
                    tool: tool.clone(),
                });
            }
            if let Some(workspace) = &rule.workspace
                && (workspace.as_os_str().is_empty() || workspace.is_absolute())
            {
                return Err(PolicyError::InvalidWorkspace {
                    rule_id: id.to_owned(),
                    reason: "expected a non-empty relative path".to_owned(),
                });
            }
            if let Some(command) = &rule.command
                && command.trim().is_empty()
            {
                return Err(PolicyError::BlankCommand(id.to_owned()));
            }
        }
        Ok(())
    }

    pub(crate) fn new(
        default: PolicyDecision,
        mut rules: Vec<PermissionRule>,
        workspace_root: &Path,
    ) -> Result<Self, PolicyError> {
        Self::validate_rules(&rules)?;
        for rule in &mut rules {
            if let Some(workspace) = &rule.workspace {
                rule.workspace = Some(
                    super::scope::resolve_rule_workspace(workspace, workspace_root).map_err(
                        |source| PolicyError::InvalidWorkspace {
                            rule_id: rule.id.clone(),
                            reason: source.to_string(),
                        },
                    )?,
                );
            }
        }
        Ok(Self { default, rules })
    }

    pub(super) fn evaluate(
        &self,
        request: &PermissionRequest,
        grants: &SessionGrants,
    ) -> Evaluation {
        let explanation = self.explain(request);
        match explanation.winning_decision {
            PolicyDecision::Deny => Evaluation::Denied {
                rule_ids: explanation.matched_rule_ids,
            },
            PolicyDecision::Ask => grants.matching_grant(request).map_or_else(
                || Evaluation::Ask {
                    rule_ids: explanation.matched_rule_ids,
                },
                |grant| Evaluation::AllowedBySessionGrant {
                    grant_id: grant.id.clone(),
                },
            ),
            PolicyDecision::Allow => Evaluation::AllowedByPolicy {
                rule_ids: explanation.matched_rule_ids,
            },
        }
    }

    pub(crate) fn explain(&self, request: &PermissionRequest) -> PolicyExplanation {
        let matching = self
            .rules
            .iter()
            .filter(|rule| rule_matches(rule, request))
            .collect::<Vec<_>>();
        let winning_decision = if matching
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Deny)
        {
            PolicyDecision::Deny
        } else if matching
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Ask)
        {
            PolicyDecision::Ask
        } else if matching
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Allow)
        {
            PolicyDecision::Allow
        } else {
            self.default
        };
        PolicyExplanation {
            matched_rule_ids: matching.iter().map(|rule| rule.id.clone()).collect(),
            winning_decision,
        }
    }
}

fn rule_matches(rule: &PermissionRule, request: &PermissionRequest) -> bool {
    if rule
        .tool
        .as_ref()
        .is_some_and(|tool| tool != &request.tool_name)
    {
        return false;
    }
    if rule
        .effect
        .is_some_and(|effect| effect != request.effect_class)
    {
        return false;
    }
    if let Some(workspace) = &rule.workspace {
        let request_path = match &request.scope {
            PermissionScope::WorkspacePath { canonical_path } => canonical_path,
            PermissionScope::Command { canonical_cwd, .. } => canonical_cwd,
            PermissionScope::External { .. } => return false,
            PermissionScope::Unscoped => return false,
        };
        if !request_path.starts_with(workspace) {
            return false;
        }
    }
    if let Some(expected) = &rule.command {
        match &request.scope {
            PermissionScope::Command { command, .. } if command == expected => {}
            PermissionScope::Command { .. }
            | PermissionScope::WorkspacePath { .. }
            | PermissionScope::External { .. }
            | PermissionScope::Unscoped => return false,
        }
    }
    true
}

#[derive(Debug, Default)]
pub(super) struct SessionGrants {
    grants: Vec<SessionGrant>,
    next_id: usize,
}

pub(super) const MAX_SESSION_GRANTS: usize = 256;

impl SessionGrants {
    pub(super) fn insert(
        &mut self,
        request: &PermissionRequest,
        scope: PermissionScope,
    ) -> Result<String, ()> {
        if let Some(existing) = self.grants.iter().find(|grant| {
            grant.tool_name == request.tool_name
                && grant.effect_class == request.effect_class
                && grant.scope == scope
        }) {
            return Ok(existing.id.clone());
        }
        if self.grants.len() >= MAX_SESSION_GRANTS {
            return Err(());
        }
        self.next_id += 1;
        let id = format!("session-grant-{}", self.next_id);
        self.grants.push(SessionGrant {
            id: id.clone(),
            tool_name: request.tool_name.clone(),
            effect_class: request.effect_class,
            scope,
        });
        Ok(id)
    }

    fn matching_grant(&self, request: &PermissionRequest) -> Option<&SessionGrant> {
        self.grants.iter().find(|grant| {
            grant.tool_name == request.tool_name
                && grant.effect_class == request.effect_class
                && super::scope_contains(&grant.scope, &request.scope)
        })
    }
}

#[derive(Debug)]
struct SessionGrant {
    id: String,
    tool_name: String,
    effect_class: EffectClass,
    scope: PermissionScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyError {
    BlankRuleId,
    DuplicateRuleId(String),
    NoMatchers(String),
    UnknownTool { rule_id: String, tool: String },
    InvalidWorkspace { rule_id: String, reason: String },
    BlankCommand(String),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankRuleId => write!(f, "permission rule id must not be blank"),
            Self::DuplicateRuleId(id) => write!(f, "permission rule id {id:?} is duplicated"),
            Self::NoMatchers(id) => {
                write!(
                    f,
                    "permission rule {id:?} must include at least one matcher"
                )
            }
            Self::UnknownTool { rule_id, tool } => {
                write!(f, "permission rule {rule_id:?} names unknown tool {tool:?}")
            }
            Self::InvalidWorkspace { rule_id, reason } => {
                write!(
                    f,
                    "permission rule {rule_id:?} has invalid workspace: {reason}"
                )
            }
            Self::BlankCommand(id) => {
                write!(f, "permission rule {id:?} command must not be blank")
            }
        }
    }
}

impl Error for PolicyError {}
