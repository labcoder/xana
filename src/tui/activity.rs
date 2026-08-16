//! Bounded, owner-aware activity projection for terminal frontends.

use crate::{
    frontend::ManagedClientEvent, managed::codex::ApprovalRequest as ManagedApprovalRequest,
    orchestration::ChildAttribution, permission::PermissionRequest,
};
use std::collections::VecDeque;

const MAX_CARDS: usize = 256;
const MAX_SUMMARY_BYTES: usize = 512;
const MAX_DETAIL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityKind {
    Status,
    Plan,
    Tool,
    Child,
    Managed,
    ReasoningSummary,
    ReasoningRaw,
    Diff,
    Approval,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityState {
    Running,
    Waiting,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivityCard {
    pub(super) owner: String,
    pub(super) identity: String,
    pub(super) kind: ActivityKind,
    pub(super) state: ActivityState,
    pub(super) summary: String,
    pub(super) detail: String,
    pub(super) expanded: bool,
}

impl ActivityCard {
    pub(super) fn new(
        owner: impl Into<String>,
        identity: impl Into<String>,
        kind: ActivityKind,
        state: ActivityState,
        summary: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            owner: bounded(super::rich_text::sanitize(&owner.into()), MAX_SUMMARY_BYTES),
            identity: bounded(
                super::rich_text::sanitize(&identity.into()),
                MAX_SUMMARY_BYTES,
            ),
            kind,
            state,
            summary: bounded(
                super::rich_text::sanitize(&summary.into()),
                MAX_SUMMARY_BYTES,
            ),
            detail: bounded(super::rich_text::sanitize(&detail.into()), MAX_DETAIL_BYTES),
            expanded: matches!(
                kind,
                ActivityKind::Approval | ActivityKind::Warning | ActivityKind::Error
            ),
        }
    }

    pub(super) fn is_substantive(&self) -> bool {
        !matches!(self.kind, ActivityKind::Status | ActivityKind::ReasoningRaw)
    }

    pub(super) fn append_detail(&mut self, value: &str) {
        super::state::append_bounded(
            &mut self.detail,
            &super::rich_text::sanitize(value),
            MAX_DETAIL_BYTES,
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ApprovalTarget {
    Native(PermissionRequest),
    Child {
        attribution: ChildAttribution,
        request: PermissionRequest,
    },
    Managed(ManagedApprovalRequest),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ApprovalPrompt {
    pub(super) target: ApprovalTarget,
    pub(super) owner: String,
    pub(super) title: String,
    pub(super) details: Vec<String>,
    pub(super) allow_once: bool,
    pub(super) allow_session: bool,
    pub(super) save_allow: bool,
    pub(super) save_deny: bool,
    pub(super) deny: bool,
}

impl ApprovalPrompt {
    pub(super) fn native(request: PermissionRequest) -> Self {
        let external_scope = matches!(
            request.scope,
            crate::permission::PermissionScope::External { .. }
        );
        let exact_external = external_scope && request.outbound_review.is_some();
        let details = request.outbound_review.as_ref().map_or_else(
            || permission_details(&request),
            |review| {
                review
                    .render()
                    .lines()
                    .map(|line| bounded(line.to_owned(), MAX_DETAIL_BYTES))
                    .collect()
            },
        );
        Self {
            owner: "this Xana conversation".to_owned(),
            title: humanize_identifier(&request.tool_name),
            details,
            target: ApprovalTarget::Native(request),
            allow_once: true,
            allow_session: !external_scope,
            save_allow: exact_external,
            save_deny: exact_external,
            deny: true,
        }
    }

    pub(super) fn child(attribution: ChildAttribution, request: PermissionRequest) -> Self {
        let owner = format!(
            "Xana child {} via {}",
            attribution.agent_id, attribution.route
        );
        let external_scope = matches!(
            request.scope,
            crate::permission::PermissionScope::External { .. }
        );
        let exact_external = external_scope && request.outbound_review.is_some();
        let details = request.outbound_review.as_ref().map_or_else(
            || permission_details(&request),
            |review| {
                review
                    .render()
                    .lines()
                    .map(|line| bounded(line.to_owned(), MAX_DETAIL_BYTES))
                    .collect()
            },
        );
        Self {
            owner,
            title: humanize_identifier(&request.tool_name),
            details,
            target: ApprovalTarget::Child {
                attribution,
                request,
            },
            allow_once: true,
            allow_session: !external_scope,
            save_allow: exact_external,
            save_deny: exact_external,
            deny: true,
        }
    }

    pub(super) fn managed(request: ManagedApprovalRequest) -> Self {
        let mut details = Vec::new();
        if let Some(reason) = &request.reason {
            details.push(bounded(reason.clone(), MAX_DETAIL_BYTES));
        }
        if let Some(command) = &request.command {
            details.push(format!(
                "command: {}",
                bounded(command.clone(), MAX_DETAIL_BYTES)
            ));
        }
        if let Some(cwd) = &request.cwd {
            details.push(format!("cwd: {}", bounded(cwd.clone(), MAX_SUMMARY_BYTES)));
        }
        Self {
            owner: "Codex managed task".to_owned(),
            title: request.method.clone(),
            details,
            allow_once: request.available_decisions.contains("accept"),
            allow_session: request.available_decisions.contains("acceptForSession"),
            save_allow: false,
            save_deny: false,
            deny: request.available_decisions.contains("decline")
                || request.available_decisions.contains("cancel"),
            target: ApprovalTarget::Managed(request),
        }
    }
}

fn permission_details(request: &PermissionRequest) -> Vec<String> {
    use crate::permission::PermissionScope;

    let mut details = vec![format!("Effect: {}", effect_label(request.effect_class))];
    match &request.scope {
        PermissionScope::WorkspacePath { canonical_path } => {
            details.push(format!("Workspace path: {}", canonical_path.display()));
        }
        PermissionScope::ExternalPath { canonical_path } => {
            details.push(format!("External path: {}", canonical_path.display()));
        }
        PermissionScope::Command {
            shell,
            canonical_cwd,
            command,
        } => {
            details.push(format!("Command: {command}"));
            details.push(format!("Working directory: {}", canonical_cwd.display()));
            details.push(format!("Shell: {shell}"));
        }
        PermissionScope::External {
            recipient_identity_digest,
            operation,
        } => {
            details.push(format!("External operation: {operation}"));
            details.push(format!(
                "Recipient: {}",
                &recipient_identity_digest[..recipient_identity_digest.len().min(12)]
            ));
        }
        PermissionScope::BuiltInResource { id } => {
            details.push(format!("Built-in resource: {id}"));
        }
        PermissionScope::Unscoped => {
            details.push("Scope: no narrower resource scope was declared".to_owned());
            details.extend(argument_details(&request.final_arguments));
        }
    }
    details
        .into_iter()
        .map(|line| bounded(line, MAX_DETAIL_BYTES))
        .collect()
}

fn argument_details(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Object(values) => values
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                format!(
                    "{}: {}",
                    humanize_identifier(key),
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                )
            })
            .collect(),
        serde_json::Value::Null => Vec::new(),
        value => vec![format!("Arguments: {value}")],
    }
}

fn humanize_identifier(value: &str) -> String {
    let mut words = value.replace(['_', '-'], " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    words
}

const fn effect_label(effect: crate::tool::EffectClass) -> &'static str {
    match effect {
        crate::tool::EffectClass::Read => "read",
        crate::tool::EffectClass::Write => "write",
        crate::tool::EffectClass::Execute => "execute",
        crate::tool::EffectClass::Network => "network",
        crate::tool::EffectClass::External => "external service",
    }
}

pub(super) fn push(cards: &mut VecDeque<ActivityCard>, card: ActivityCard) {
    if let Some(existing) = cards.iter_mut().rev().find(|existing| {
        existing.owner == card.owner
            && existing.identity == card.identity
            && existing.kind == card.kind
    }) {
        *existing = card;
        return;
    }
    cards.push_back(card);
    while cards.len() > MAX_CARDS {
        cards.pop_front();
    }
}

pub(super) fn from_managed(event: &ManagedClientEvent) -> Option<ActivityCard> {
    let card = match event {
        ManagedClientEvent::ThreadReady => ActivityCard::new(
            "Codex",
            "thread",
            ActivityKind::Managed,
            ActivityState::Complete,
            "managed thread ready",
            "Codex owns this thread and its inner loop",
        ),
        ManagedClientEvent::ReasoningSummaryDelta(delta) => ActivityCard::new(
            "Codex",
            "reasoning-summary",
            ActivityKind::ReasoningSummary,
            ActivityState::Running,
            "reasoning summary",
            delta.clone(),
        ),
        ManagedClientEvent::ReasoningSummaryPartAdded => return None,
        ManagedClientEvent::ReasoningDelta(delta) => ActivityCard::new(
            "Codex",
            "reasoning",
            ActivityKind::ReasoningRaw,
            ActivityState::Running,
            "exposed reasoning",
            delta.clone(),
        ),
        ManagedClientEvent::PlanDelta(delta) => ActivityCard::new(
            "Codex",
            "plan",
            ActivityKind::Plan,
            ActivityState::Running,
            "plan",
            delta.clone(),
        ),
        ManagedClientEvent::PlanUpdated {
            explanation,
            steps,
            truncated,
        } => ActivityCard::new(
            "Codex",
            "plan",
            ActivityKind::Plan,
            ActivityState::Running,
            format!(
                "plan ({} step{})",
                steps.len(),
                if steps.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{}{}{}",
                explanation.as_deref().unwrap_or(""),
                if explanation.is_some() { "\n" } else { "" },
                steps
                    .iter()
                    .map(|step| format!("[{}] {}", step.status, step.step))
                    .collect::<Vec<_>>()
                    .join("\n")
            ) + if *truncated {
                "\n[additional steps omitted]"
            } else {
                ""
            },
        ),
        ManagedClientEvent::CommandOutputDelta(delta) => ActivityCard::new(
            "Codex",
            "command-output",
            ActivityKind::Tool,
            ActivityState::Running,
            "command output",
            delta.clone(),
        ),
        ManagedClientEvent::DiffUpdated { preview, truncated } => ActivityCard::new(
            "Codex",
            "diff",
            ActivityKind::Diff,
            ActivityState::Running,
            if *truncated {
                "diff (preview truncated)"
            } else {
                "diff"
            },
            preview.clone(),
        ),
        ManagedClientEvent::ItemStarted(item) => ActivityCard::new(
            "Codex",
            item.id.clone(),
            item_kind(&item.kind),
            ActivityState::Running,
            item.label.clone(),
            item.details.clone(),
        ),
        ManagedClientEvent::ItemCompleted(item) => ActivityCard::new(
            "Codex",
            item.id.clone(),
            item_kind(&item.kind),
            ActivityState::Complete,
            item.label.clone(),
            item.details.clone(),
        ),
        ManagedClientEvent::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => ActivityCard::new(
            "Codex",
            "model-reroute",
            ActivityKind::Warning,
            ActivityState::Complete,
            format!("rerouted {from_model} to {to_model}"),
            reason.clone(),
        ),
        ManagedClientEvent::TurnCompleted { status, error } => ActivityCard::new(
            "Codex",
            "turn",
            if error.is_some() {
                ActivityKind::Error
            } else {
                ActivityKind::Managed
            },
            if error.is_some() {
                ActivityState::Failed
            } else {
                ActivityState::Complete
            },
            format!("managed turn {status}"),
            error.clone().unwrap_or_default(),
        ),
        ManagedClientEvent::TokenUsageUpdated {
            input_tokens,
            output_tokens,
            total_tokens,
        } => ActivityCard::new(
            "Codex",
            "usage",
            ActivityKind::Status,
            ActivityState::Complete,
            format!("tokens: {input_tokens} in / {output_tokens} out / {total_tokens} total"),
            "",
        ),
        ManagedClientEvent::Warning(message) => ActivityCard::new(
            "Codex",
            "warning",
            ActivityKind::Warning,
            ActivityState::Failed,
            "managed runtime warning",
            message.clone(),
        ),
        ManagedClientEvent::AssistantDelta(_) => return None,
    };
    Some(card)
}

fn item_kind(kind: &str) -> ActivityKind {
    match kind {
        "reasoning" => ActivityKind::ReasoningSummary,
        "plan" => ActivityKind::Plan,
        "commandExecution" | "fileChange" | "mcpToolCall" => ActivityKind::Tool,
        "collabAgentToolCall" => ActivityKind::Child,
        _ => ActivityKind::Managed,
    }
}

fn bounded(mut value: String, limit: usize) -> String {
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

    #[test]
    fn activity_text_makes_terminal_and_bidi_controls_inert() {
        let mut card = ActivityCard::new(
            "owner\u{1b}[31m",
            "identity\u{202e}",
            ActivityKind::ReasoningRaw,
            ActivityState::Running,
            "summary\u{7}",
            "detail\u{202d}",
        );
        card.append_detail("delta\u{1b}[2J");

        for value in [&card.owner, &card.identity, &card.summary, &card.detail] {
            assert!(!value.contains('\u{1b}'));
            assert!(!value.contains('\u{202d}'));
            assert!(!value.contains('\u{202e}'));
        }
        assert!(card.detail.contains("delta"));
    }
}
