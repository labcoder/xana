//! Typed permission-rule management for graphical frontends.

use super::{DesktopControlPlane, DesktopEntityMutationReceipt, DesktopError, control_error};
use crate::{
    config::{PermissionMode, XanaConfig},
    permission::{PermissionPolicy, PermissionRule, PolicyDecision},
    tool::{BUILTIN_TOOL_NAMES, EffectClass},
};
use std::path::PathBuf;

const PERMISSION_VERSION: u16 = 1;
const MAX_RULE_TEXT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPermissionDecision {
    Deny,
    Ask,
    Allow,
}

impl DesktopPermissionDecision {
    pub const ALL: [Self; 3] = [Self::Deny, Self::Ask, Self::Allow];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPermissionEffect {
    Read,
    Write,
    Execute,
    Network,
    External,
}

impl DesktopPermissionEffect {
    pub const ALL: [Self; 5] = [
        Self::Read,
        Self::Write,
        Self::Execute,
        Self::Network,
        Self::External,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Network => "network",
            Self::External => "external",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPermissionRuleDraft {
    pub id: String,
    pub decision: DesktopPermissionDecision,
    pub tool: Option<String>,
    pub effect: Option<DesktopPermissionEffect>,
    pub workspace: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPermissionRuleSummary {
    pub id: String,
    pub decision: DesktopPermissionDecision,
    pub tool: Option<String>,
    pub effect: Option<DesktopPermissionEffect>,
    pub workspace: Option<String>,
    pub command: Option<String>,
    pub matcher_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPermissionSnapshot {
    pub version: u16,
    pub default: DesktopPermissionDecision,
    pub rules: Vec<DesktopPermissionRuleSummary>,
    pub tools: Vec<String>,
    pub precedence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPermissionPreview {
    pub rule: DesktopPermissionRuleSummary,
    pub exact_overlap_ids: Vec<String>,
    pub effective_for_exact_overlap: DesktopPermissionDecision,
    pub effect_timing: String,
}

impl DesktopControlPlane {
    pub fn permission_snapshot(&self) -> Result<DesktopPermissionSnapshot, DesktopError> {
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        Ok(DesktopPermissionSnapshot {
            version: PERMISSION_VERSION,
            default: desktop_decision(registry.permission_mode.into()),
            rules: registry
                .permission_rules
                .iter()
                .map(desktop_rule)
                .collect(),
            tools: BUILTIN_TOOL_NAMES
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect(),
            precedence: "Any matching deny wins; otherwise ask, then allow, then the global default. Matchers within one rule are conjunctive.".to_owned(),
        })
    }

    pub fn preview_permission_rule(
        &self,
        draft: &DesktopPermissionRuleDraft,
    ) -> Result<DesktopPermissionPreview, DesktopError> {
        let candidate = core_rule(draft)?;
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        let mut rules = registry
            .permission_rules
            .iter()
            .filter(|rule| rule.id != candidate.id)
            .cloned()
            .collect::<Vec<_>>();
        rules.push(candidate.clone());
        PermissionPolicy::validate_rules(&rules).map_err(control_error)?;
        let overlaps = rules
            .iter()
            .filter(|rule| rule.id != candidate.id && same_matchers(rule, &candidate))
            .collect::<Vec<_>>();
        let mut decisions = overlaps
            .iter()
            .map(|rule| rule.decision)
            .collect::<Vec<_>>();
        decisions.push(candidate.decision);
        Ok(DesktopPermissionPreview {
            rule: desktop_rule(&candidate),
            exact_overlap_ids: overlaps.iter().map(|rule| rule.id.clone()).collect(),
            effective_for_exact_overlap: desktop_decision(winning_decision(&decisions)),
            effect_timing:
                "Applies only to new Conversations; the active permission snapshot is immutable."
                    .to_owned(),
        })
    }

    pub fn save_permission_rule(
        &self,
        draft: DesktopPermissionRuleDraft,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let rule = core_rule(&draft)?;
        self.preview_permission_rule(&draft)?;
        XanaConfig::upsert_permission_rule(self.paths.config_file(), rule)
            .map_err(control_error)?;
        Ok(permission_receipt(
            "permission_rule.save.completed.v1",
            draft.id,
            "saved",
        ))
    }

    pub fn remove_permission_rule(
        &self,
        id: &str,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        validate_text("permission rule id", id)?;
        XanaConfig::remove_permission_rule(self.paths.config_file(), id).map_err(control_error)?;
        Ok(permission_receipt(
            "permission_rule.remove.completed.v1",
            id.to_owned(),
            "removed",
        ))
    }
}

fn core_rule(draft: &DesktopPermissionRuleDraft) -> Result<PermissionRule, DesktopError> {
    validate_text("permission rule id", &draft.id)?;
    for (label, value) in [
        ("tool", draft.tool.as_deref()),
        ("workspace", draft.workspace.as_deref()),
        ("command", draft.command.as_deref()),
    ] {
        if let Some(value) = value {
            validate_text(label, value)?;
        }
    }
    let rule = PermissionRule {
        id: draft.id.trim().to_owned(),
        decision: core_decision(draft.decision),
        tool: clean(&draft.tool),
        effect: draft.effect.map(core_effect),
        workspace: clean(&draft.workspace).map(PathBuf::from),
        command: clean(&draft.command),
    };
    PermissionPolicy::validate_rules(std::slice::from_ref(&rule)).map_err(control_error)?;
    Ok(rule)
}

fn clean(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn validate_text(label: &str, value: &str) -> Result<(), DesktopError> {
    if value.len() > MAX_RULE_TEXT_BYTES {
        return Err(control_error(format!(
            "{label} exceeds {MAX_RULE_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn desktop_rule(rule: &PermissionRule) -> DesktopPermissionRuleSummary {
    let mut matchers = Vec::new();
    if let Some(tool) = &rule.tool {
        matchers.push(format!("tool = {tool}"));
    }
    if let Some(effect) = rule.effect {
        matchers.push(format!("effect = {}", desktop_effect(effect).id()));
    }
    if let Some(workspace) = &rule.workspace {
        matchers.push(format!("workspace = {}", workspace.display()));
    }
    if let Some(command) = &rule.command {
        matchers.push(format!("command = {command}"));
    }
    DesktopPermissionRuleSummary {
        id: rule.id.clone(),
        decision: desktop_decision(rule.decision),
        tool: rule.tool.clone(),
        effect: rule.effect.map(desktop_effect),
        workspace: rule
            .workspace
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned()),
        command: rule.command.clone(),
        matcher_summary: matchers.join(" AND "),
    }
}

fn same_matchers(left: &PermissionRule, right: &PermissionRule) -> bool {
    left.tool == right.tool
        && left.effect == right.effect
        && left.workspace == right.workspace
        && left.command == right.command
}

fn winning_decision(decisions: &[PolicyDecision]) -> PolicyDecision {
    if decisions.contains(&PolicyDecision::Deny) {
        PolicyDecision::Deny
    } else if decisions.contains(&PolicyDecision::Ask) {
        PolicyDecision::Ask
    } else if decisions.contains(&PolicyDecision::Allow) {
        PolicyDecision::Allow
    } else {
        PolicyDecision::Ask
    }
}

fn permission_receipt(
    semantic_code: &str,
    subject: String,
    effect: &str,
) -> DesktopEntityMutationReceipt {
    DesktopEntityMutationReceipt {
        semantic_code: semantic_code.to_owned(),
        subject,
        effect: effect.to_owned(),
        detail: "Configuration backup created; existing Conversations are unchanged.".to_owned(),
    }
}

fn core_decision(value: DesktopPermissionDecision) -> PolicyDecision {
    match value {
        DesktopPermissionDecision::Deny => PolicyDecision::Deny,
        DesktopPermissionDecision::Ask => PolicyDecision::Ask,
        DesktopPermissionDecision::Allow => PolicyDecision::Allow,
    }
}

fn desktop_decision(value: PolicyDecision) -> DesktopPermissionDecision {
    match value {
        PolicyDecision::Deny => DesktopPermissionDecision::Deny,
        PolicyDecision::Ask => DesktopPermissionDecision::Ask,
        PolicyDecision::Allow => DesktopPermissionDecision::Allow,
    }
}

fn core_effect(value: DesktopPermissionEffect) -> EffectClass {
    match value {
        DesktopPermissionEffect::Read => EffectClass::Read,
        DesktopPermissionEffect::Write => EffectClass::Write,
        DesktopPermissionEffect::Execute => EffectClass::Execute,
        DesktopPermissionEffect::Network => EffectClass::Network,
        DesktopPermissionEffect::External => EffectClass::External,
    }
}

fn desktop_effect(value: EffectClass) -> DesktopPermissionEffect {
    match value {
        EffectClass::Read => DesktopPermissionEffect::Read,
        EffectClass::Write => DesktopPermissionEffect::Write,
        EffectClass::Execute => DesktopPermissionEffect::Execute,
        EffectClass::Network => DesktopPermissionEffect::Network,
        EffectClass::External => DesktopPermissionEffect::External,
    }
}

impl From<PermissionMode> for DesktopPermissionDecision {
    fn from(value: PermissionMode) -> Self {
        desktop_decision(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection},
        shell::ShellConfig,
    };
    use std::fs;
    use tempfile::tempdir;

    fn control() -> (tempfile::TempDir, DesktopControlPlane) {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        fs::write(control.paths.config_file(), rendered).unwrap();
        (directory, control)
    }

    #[test]
    fn preview_and_save_share_real_policy_validation() {
        let (_directory, control) = control();
        let draft = DesktopPermissionRuleDraft {
            id: "allow-reads".to_owned(),
            decision: DesktopPermissionDecision::Allow,
            tool: Some("read_file".to_owned()),
            effect: Some(DesktopPermissionEffect::Read),
            workspace: Some(".".to_owned()),
            command: None,
        };
        let preview = control.preview_permission_rule(&draft).unwrap();
        assert_eq!(
            preview.effective_for_exact_overlap,
            DesktopPermissionDecision::Allow
        );
        control.save_permission_rule(draft).unwrap();
        assert_eq!(control.permission_snapshot().unwrap().rules.len(), 1);
    }

    #[test]
    fn exact_overlap_preview_uses_real_precedence() {
        let (_directory, control) = control();
        let mut draft = DesktopPermissionRuleDraft {
            id: "deny-tests".to_owned(),
            decision: DesktopPermissionDecision::Deny,
            tool: Some("run_command".to_owned()),
            effect: Some(DesktopPermissionEffect::Execute),
            workspace: None,
            command: Some("cargo test".to_owned()),
        };
        control.save_permission_rule(draft.clone()).unwrap();
        draft.id = "allow-tests".to_owned();
        draft.decision = DesktopPermissionDecision::Allow;
        let preview = control.preview_permission_rule(&draft).unwrap();
        assert_eq!(preview.exact_overlap_ids, vec!["deny-tests"]);
        assert_eq!(
            preview.effective_for_exact_overlap,
            DesktopPermissionDecision::Deny
        );
    }
}
