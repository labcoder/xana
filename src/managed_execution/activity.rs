//! Bounded terminal rendering for typed managed-runtime activity.

use crate::{
    frontend::{ManagedClientEvent, ManagedClientItem},
    managed::codex::{
        ApprovalDecision, ApprovalRequest, CodexError, ManagedEventHandler, ManagedNotification,
    },
};
use futures::future::BoxFuture;
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, IsTerminal, Write},
    str::FromStr,
};

const MAX_RETAINED_ACTIVITY_BYTES: usize = 256 * 1024;
const MAX_RETAINED_ACTIVITY_EVENTS: usize = 512;
const MAX_PENDING_ITEMS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityLevel {
    Quiet,
    Normal,
    Verbose,
}

impl fmt::Display for ActivityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Quiet => "quiet",
            Self::Normal => "normal",
            Self::Verbose => "verbose",
        })
    }
}

impl FromStr for ActivityLevel {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "quiet" => Ok(Self::Quiet),
            "normal" => Ok(Self::Normal),
            "verbose" => Ok(Self::Verbose),
            _ => Err("expected quiet, normal, or verbose"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Assistant,
    ReasoningSummary,
    Reasoning,
    Plan,
    Tool,
}

#[derive(Default)]
pub(super) struct RetainedActivity {
    events: Vec<ManagedClientEvent>,
    bytes: usize,
    truncated: bool,
    pub(super) assistant_streamed: bool,
}

impl RetainedActivity {
    fn push(&mut self, notification: &ManagedClientEvent) {
        let is_content_item = matches!(
            notification,
            ManagedClientEvent::ItemStarted(ManagedClientItem { kind, .. })
                | ManagedClientEvent::ItemCompleted(ManagedClientItem { kind, .. })
                if matches!(kind.as_str(), "agentMessage" | "userMessage" | "reasoning" | "plan")
        );
        if matches!(notification, ManagedClientEvent::AssistantDelta(_)) || is_content_item {
            return;
        }
        let cost = retained_cost(notification);
        if self.events.len() >= MAX_RETAINED_ACTIVITY_EVENTS
            || self.bytes.saturating_add(cost) > MAX_RETAINED_ACTIVITY_BYTES
        {
            self.truncated = true;
            return;
        }
        self.bytes += cost;
        self.events.push(notification.clone());
    }

    pub(super) fn usage(&self) -> Option<(u64, u64, u64)> {
        self.events.iter().rev().find_map(|event| match event {
            ManagedClientEvent::TokenUsageUpdated {
                input_tokens,
                output_tokens,
                total_tokens,
            } => Some((*input_tokens, *output_tokens, *total_tokens)),
            _ => None,
        })
    }
}

pub(super) struct TerminalManagedHandler {
    activity: ActivityLevel,
    current_stream: Option<StreamKind>,
    pending_items: BTreeMap<String, ManagedClientItem>,
    retained: RetainedActivity,
}

impl TerminalManagedHandler {
    pub(super) fn new(activity: ActivityLevel) -> Self {
        Self {
            activity,
            current_stream: None,
            pending_items: BTreeMap::new(),
            retained: RetainedActivity::default(),
        }
    }

    pub(super) fn into_retained(self) -> RetainedActivity {
        self.retained
    }

    fn write_delta(
        &mut self,
        kind: StreamKind,
        prefix: &str,
        delta: &str,
    ) -> Result<(), CodexError> {
        if self.current_stream != Some(kind) {
            self.finish_stream()?;
            print!("{prefix} ");
            self.current_stream = Some(kind);
        }
        print!("{delta}");
        if delta.ends_with('\n') {
            self.current_stream = None;
        }
        io::stdout()
            .flush()
            .map_err(|error| CodexError::Io(error.to_string()))
    }

    pub(super) fn finish_stream(&mut self) -> Result<(), CodexError> {
        if self.current_stream.take().is_some() {
            println!();
            io::stdout()
                .flush()
                .map_err(|error| CodexError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn line(&mut self, message: impl fmt::Display) -> Result<(), CodexError> {
        self.finish_stream()?;
        println!("xana> {message}");
        Ok(())
    }

    fn render_item(&mut self, verb: &str, item: &ManagedClientItem) -> Result<(), CodexError> {
        if !is_visible_work_item(&item.kind) || self.activity == ActivityLevel::Quiet {
            return Ok(());
        }
        let status = item
            .status
            .as_deref()
            .map(|status| format!(" ({status})"))
            .unwrap_or_default();
        self.line(format_args!("Codex {verb} {}{status}", item.label))
    }
}

impl ManagedEventHandler for TerminalManagedHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        if let Some(event) = ManagedClientEvent::from_notification(notification) {
            self.render_event(event)?;
        }
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        let result = self.approve_now(request);
        Box::pin(async move { result })
    }
}

impl TerminalManagedHandler {
    fn render_event(&mut self, event: ManagedClientEvent) -> Result<(), CodexError> {
        self.retained.push(&event);
        match event {
            ManagedClientEvent::ThreadReady | ManagedClientEvent::TokenUsageUpdated { .. } => {}
            ManagedClientEvent::AssistantDelta(delta) => {
                self.retained.assistant_streamed |= !delta.is_empty();
                self.write_delta(StreamKind::Assistant, "xana>", &delta)?;
            }
            ManagedClientEvent::ReasoningSummaryDelta(delta)
                if self.activity != ActivityLevel::Quiet =>
            {
                self.write_delta(StreamKind::ReasoningSummary, "summary>", &delta)?;
            }
            ManagedClientEvent::ReasoningSummaryPartAdded
                if self.activity != ActivityLevel::Quiet =>
            {
                self.finish_stream()?;
            }
            ManagedClientEvent::ReasoningDelta(delta)
                if self.activity == ActivityLevel::Verbose =>
            {
                self.write_delta(StreamKind::Reasoning, "reasoning>", &delta)?;
            }
            ManagedClientEvent::PlanDelta(delta) if self.activity == ActivityLevel::Verbose => {
                self.write_delta(StreamKind::Plan, "plan>", &delta)?;
            }
            ManagedClientEvent::CommandOutputDelta(delta)
                if self.activity == ActivityLevel::Verbose =>
            {
                self.write_delta(StreamKind::Tool, "tool>", &delta)?;
            }
            ManagedClientEvent::PlanUpdated {
                explanation,
                steps,
                truncated,
            } if self.activity != ActivityLevel::Quiet => {
                self.finish_stream()?;
                if let Some(explanation) = explanation {
                    println!("xana> plan: {explanation}");
                } else {
                    println!("xana> plan updated:");
                }
                for step in steps {
                    println!("  [{}] {}", plan_marker(&step.status), step.step);
                }
                if truncated {
                    println!("  [...] additional plan steps omitted by the frontend bound");
                }
            }
            ManagedClientEvent::DiffUpdated { preview, truncated }
                if self.activity == ActivityLevel::Verbose =>
            {
                self.finish_stream()?;
                println!("xana> working diff updated:\n{preview}");
                if truncated {
                    println!("xana> additional diff content omitted by the frontend bound");
                }
            }
            ManagedClientEvent::DiffUpdated { .. } if self.activity == ActivityLevel::Normal => {
                self.line("working diff updated; use /details or verbose activity to inspect it")?;
            }
            ManagedClientEvent::ItemStarted(item) => {
                self.render_item("started", &item)?;
                if !self.pending_items.contains_key(&item.id)
                    && self.pending_items.len() >= MAX_PENDING_ITEMS
                {
                    return Err(CodexError::Protocol(format!(
                        "managed runtime exceeds the {MAX_PENDING_ITEMS}-item pending limit"
                    )));
                }
                self.pending_items.insert(item.id.clone(), item);
            }
            ManagedClientEvent::ItemCompleted(item) => {
                if item.kind == "contextCompaction" && self.activity != ActivityLevel::Quiet {
                    self.line("Codex compacted the managed conversation context")?;
                } else {
                    self.render_item("finished", &item)?;
                }
                self.pending_items.remove(&item.id);
            }
            ManagedClientEvent::ModelRerouted {
                from_model,
                to_model,
                reason,
            } => self.line(format_args!(
                "Codex rerouted {from_model} to {to_model}: {reason}"
            ))?,
            ManagedClientEvent::Warning(message) => {
                self.line(format_args!("Codex warning: {message}"))?;
            }
            ManagedClientEvent::TurnCompleted { status, error } if status != "completed" => {
                self.line(format_args!(
                    "Codex turn ended with status {status}{}",
                    error
                        .as_deref()
                        .map(|value| format!(": {value}"))
                        .unwrap_or_default()
                ))?;
            }
            ManagedClientEvent::ReasoningSummaryDelta(_)
            | ManagedClientEvent::ReasoningSummaryPartAdded
            | ManagedClientEvent::ReasoningDelta(_)
            | ManagedClientEvent::PlanDelta(_)
            | ManagedClientEvent::CommandOutputDelta(_)
            | ManagedClientEvent::PlanUpdated { .. }
            | ManagedClientEvent::DiffUpdated { .. }
            | ManagedClientEvent::TurnCompleted { .. } => {}
        }
        Ok(())
    }
    fn approve_now(&mut self, request: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        self.finish_stream()?;
        println!("xana> Codex requests approval");
        println!("request: {}", request.method);
        if let Some(item_id) = &request.item_id
            && let Some(item) = self.pending_items.get(item_id)
        {
            println!("proposed {}:", item.kind);
            println!("{}", item.details);
        }
        if let Some(command) = request.command {
            println!("command: {command}");
        }
        if let Some(cwd) = request.cwd {
            println!("cwd: {cwd}");
        }
        if let Some(reason) = request.reason {
            println!("reason: {reason}");
        }
        let safe_rejection = if request.available_decisions.contains("decline") {
            Some(ApprovalDecision::Decline)
        } else if request.available_decisions.contains("cancel") {
            Some(ApprovalDecision::Cancel)
        } else {
            None
        };
        if !io::stdin().is_terminal() {
            return safe_rejection.ok_or_else(|| {
                CodexError::Protocol(
                    "approval request offered no supported fail-closed decision".into(),
                )
            });
        }
        let once_allowed = request.available_decisions.contains("accept");
        let session_allowed = request.available_decisions.contains("acceptForSession");
        if once_allowed && session_allowed {
            print!("Allow? [y] once, [a] session, [n] decline, [c] cancel: ");
        } else if once_allowed {
            print!("Allow? [y] once, [n] decline, [c] cancel: ");
        } else {
            print!("Xana cannot grant the advertised amendment safely; [n] decline, [c] cancel: ");
        }
        io::stdout()
            .flush()
            .map_err(|error| CodexError::Io(error.to_string()))?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|error| CodexError::Io(error.to_string()))?;
        Ok(match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" if once_allowed => ApprovalDecision::AcceptOnce,
            "a" | "session" if session_allowed => ApprovalDecision::AcceptForSession,
            "c" | "cancel" if request.available_decisions.contains("cancel") => {
                ApprovalDecision::Cancel
            }
            _ => safe_rejection.ok_or_else(|| {
                CodexError::Protocol(
                    "approval request offered no supported fail-closed decision".into(),
                )
            })?,
        })
    }
}

pub(super) fn render_retained_activity(activity: &RetainedActivity) -> Result<(), CodexError> {
    if activity.events.is_empty() {
        println!("xana> no additional managed activity was retained for the last turn");
        return Ok(());
    }
    println!("xana> retained managed activity for the last turn:");
    let mut renderer = TerminalManagedHandler::new(ActivityLevel::Verbose);
    for event in &activity.events {
        renderer.render_event(event.clone())?;
    }
    renderer.finish_stream()?;
    if activity.truncated {
        println!(
            "xana> retained activity reached its {} KiB / {} event bound",
            MAX_RETAINED_ACTIVITY_BYTES / 1024,
            MAX_RETAINED_ACTIVITY_EVENTS
        );
    }
    Ok(())
}

fn retained_cost(notification: &ManagedClientEvent) -> usize {
    match notification {
        ManagedClientEvent::AssistantDelta(delta)
        | ManagedClientEvent::ReasoningSummaryDelta(delta)
        | ManagedClientEvent::ReasoningDelta(delta)
        | ManagedClientEvent::PlanDelta(delta)
        | ManagedClientEvent::CommandOutputDelta(delta)
        | ManagedClientEvent::Warning(delta) => delta.len(),
        ManagedClientEvent::DiffUpdated { preview, .. } => preview.len(),
        ManagedClientEvent::ItemStarted(item) | ManagedClientEvent::ItemCompleted(item) => {
            item.details.len() + item.label.len()
        }
        ManagedClientEvent::PlanUpdated {
            explanation, steps, ..
        } => {
            explanation.as_deref().map_or(0, str::len)
                + steps
                    .iter()
                    .map(|step| step.step.len() + step.status.len())
                    .sum::<usize>()
        }
        ManagedClientEvent::ModelRerouted {
            from_model,
            to_model,
            reason,
        } => from_model.len() + to_model.len() + reason.len(),
        ManagedClientEvent::TurnCompleted { error, .. } => error.as_deref().map_or(64, str::len),
        ManagedClientEvent::ThreadReady | ManagedClientEvent::ReasoningSummaryPartAdded => 64,
        ManagedClientEvent::TokenUsageUpdated { .. } => 24,
    }
}

fn is_visible_work_item(kind: &str) -> bool {
    matches!(
        kind,
        "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "collabToolCall"
            | "collabAgentToolCall"
            | "subAgentActivity"
            | "webSearch"
            | "imageView"
            | "sleep"
            | "imageGeneration"
            | "enteredReviewMode"
            | "exitedReviewMode"
    )
}

fn plan_marker(status: &str) -> &'static str {
    match status {
        "completed" => "x",
        "inProgress" | "in_progress" => ">",
        _ => " ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed::codex::ManagedItem;

    #[test]
    fn activity_levels_are_explicit_and_do_not_change_reasoning_effort() {
        assert_eq!("quiet".parse::<ActivityLevel>(), Ok(ActivityLevel::Quiet));
        assert_eq!("normal".parse::<ActivityLevel>(), Ok(ActivityLevel::Normal));
        assert_eq!(
            "verbose".parse::<ActivityLevel>(),
            Ok(ActivityLevel::Verbose)
        );
        assert!("thinking".parse::<ActivityLevel>().is_err());
    }

    #[test]
    fn retained_activity_is_bounded_and_omits_visible_assistant_text() {
        let mut retained = RetainedActivity::default();
        retained.push(&ManagedClientEvent::AssistantDelta(
            "already visible".into(),
        ));
        retained.push(&ManagedClientEvent::ItemCompleted(ManagedClientItem {
            id: "answer".into(),
            kind: "agentMessage".into(),
            status: None,
            label: "agentMessage".into(),
            details: "final assistant text".into(),
        }));
        assert!(retained.events.is_empty());
        for _ in 0..MAX_RETAINED_ACTIVITY_EVENTS + 1 {
            retained.push(&ManagedClientEvent::Warning("test event".into()));
        }
        assert_eq!(retained.events.len(), MAX_RETAINED_ACTIVITY_EVENTS);
        assert!(retained.truncated);
    }

    #[test]
    fn normal_activity_includes_work_but_not_message_or_reasoning_lifecycle_items() {
        assert!(is_visible_work_item("commandExecution"));
        assert!(is_visible_work_item("collabAgentToolCall"));
        assert!(is_visible_work_item("subAgentActivity"));
        assert!(!is_visible_work_item("agentMessage"));
        assert!(!is_visible_work_item("reasoning"));
    }

    #[test]
    fn empty_assistant_delta_does_not_hide_the_completed_answer() {
        let mut handler = TerminalManagedHandler::new(ActivityLevel::Quiet);
        handler
            .notification(ManagedNotification::AssistantDelta {
                item_id: None,
                delta: String::new(),
            })
            .expect("empty delta");

        assert!(!handler.into_retained().assistant_streamed);
    }

    #[test]
    fn pending_item_tracking_is_bounded() {
        let mut handler = TerminalManagedHandler::new(ActivityLevel::Quiet);
        for index in 0..MAX_PENDING_ITEMS {
            handler
                .notification(ManagedNotification::ItemStarted(ManagedItem {
                    id: format!("item-{index}"),
                    kind: "commandExecution".into(),
                    status: None,
                    phase: None,
                    label: "command".into(),
                    details: String::new(),
                    text: None,
                }))
                .expect("pending item within limit");
        }
        let error = handler
            .notification(ManagedNotification::ItemStarted(ManagedItem {
                id: "overflow".into(),
                kind: "commandExecution".into(),
                status: None,
                phase: None,
                label: "command".into(),
                details: String::new(),
                text: None,
            }))
            .expect_err("pending item overflow");
        assert!(error.to_string().contains("pending limit"));
    }
}
