//! Normalization from Codex wire notifications to bounded managed events.

use super::*;

pub(super) fn normalize_notification(
    method: String,
    params: Value,
) -> Result<ManagedNotification, CodexError> {
    Ok(match method.as_str() {
        "thread/started" => ManagedNotification::ThreadStarted {
            thread_id: required_string(
                params
                    .get("thread")
                    .ok_or_else(|| CodexError::Protocol("thread/started omitted thread".into()))?,
                "id",
            )?,
        },
        "item/agentMessage/delta" => ManagedNotification::AssistantDelta {
            item_id: optional_string(&params, "itemId"),
            delta: bounded_event_delta(&params),
        },
        "item/reasoning/summaryTextDelta" => ManagedNotification::ReasoningSummaryDelta {
            item_id: optional_string(&params, "itemId"),
            summary_index: params
                .get("summaryIndex")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            delta: bounded_event_delta(&params),
        },
        "item/reasoning/summaryPartAdded" => ManagedNotification::ReasoningSummaryPartAdded {
            item_id: optional_string(&params, "itemId"),
            summary_index: params
                .get("summaryIndex")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        },
        "item/reasoning/textDelta" => ManagedNotification::ReasoningDelta {
            item_id: optional_string(&params, "itemId"),
            delta: bounded_event_delta(&params),
        },
        "item/plan/delta" => ManagedNotification::PlanDelta {
            item_id: optional_string(&params, "itemId"),
            delta: bounded_event_delta(&params),
        },
        "item/commandExecution/outputDelta" => ManagedNotification::CommandOutputDelta {
            item_id: optional_string(&params, "itemId"),
            delta: bounded_event_delta(&params),
        },
        "item/started" => ManagedNotification::ItemStarted(normalize_item(&params, &method)?),
        "item/completed" => ManagedNotification::ItemCompleted(normalize_item(&params, &method)?),
        "turn/plan/updated" => {
            let plan = params
                .get("plan")
                .and_then(Value::as_array)
                .ok_or_else(|| CodexError::Protocol("turn/plan/updated omitted plan".into()))?;
            if plan.len() > MAX_PLAN_STEPS {
                return Err(CodexError::Protocol(format!(
                    "turn plan exceeds the {MAX_PLAN_STEPS}-step limit"
                )));
            }
            let steps = plan
                .iter()
                .map(|value| ManagedPlanStep {
                    step: bounded_text(
                        value
                            .get("step")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                        4096,
                    ),
                    status: bounded_text(
                        value
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        64,
                    ),
                })
                .collect();
            ManagedNotification::PlanUpdated {
                explanation: params
                    .get("explanation")
                    .and_then(Value::as_str)
                    .map(|value| bounded_text(value, 4096)),
                steps,
            }
        }
        "turn/diff/updated" => ManagedNotification::DiffUpdated(bounded_text(
            params
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            MAX_EVENT_TEXT_BYTES,
        )),
        "model/rerouted" => ManagedNotification::ModelRerouted {
            from_model: bounded_text(
                params
                    .get("fromModel")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                256,
            ),
            to_model: bounded_text(
                params
                    .get("toModel")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                256,
            ),
            reason: bounded_text(
                params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unspecified"),
                4096,
            ),
        },
        "turn/completed" => ManagedNotification::TurnCompleted {
            turn_id: params
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            error: params
                .pointer("/turn/error/message")
                .and_then(Value::as_str)
                .map(|value| bounded_text(value, 4096)),
        },
        "thread/tokenUsage/updated" => ManagedNotification::TokenUsageUpdated {
            thread_id: required_string(&params, "threadId")?,
            turn_id: required_string(&params, "turnId")?,
            input_tokens: required_u64(&params, "/tokenUsage/total/inputTokens")?,
            output_tokens: required_u64(&params, "/tokenUsage/total/outputTokens")?,
            total_tokens: required_u64(&params, "/tokenUsage/total/totalTokens")?,
        },
        "warning" | "error" => ManagedNotification::Warning(bounded_text(
            params
                .get("message")
                .or_else(|| params.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("Codex reported a warning"),
            4096,
        )),
        "account/login/completed" => ManagedNotification::LoginCompleted {
            login_id: required_string(&params, "loginId")?,
            success: params
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            error: params
                .get("error")
                .and_then(Value::as_str)
                .map(|value| bounded_text(value, 4096)),
        },
        _ => ManagedNotification::Other { method },
    })
}

fn required_u64(value: &Value, pointer: &str) -> Result<u64, CodexError> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| CodexError::Protocol(format!("notification omitted non-negative {pointer}")))
}

fn bounded_event_delta(params: &Value) -> String {
    bounded_text(
        params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_EVENT_TEXT_BYTES,
    )
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn normalize_item(params: &Value, method: &str) -> Result<ManagedItem, CodexError> {
    let item = params
        .get("item")
        .ok_or_else(|| CodexError::Protocol(format!("{method} omitted item")))?;
    let kind = required_string(item, "type")?;
    Ok(ManagedItem {
        id: required_string(item, "id")?,
        status: item
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        phase: item.get("phase").and_then(Value::as_str).map(str::to_owned),
        label: item_label(&kind, item),
        details: bounded_json_summary(item),
        text: item
            .get("text")
            .and_then(Value::as_str)
            .map(|value| bounded_text(value, MAX_EVENT_TEXT_BYTES)),
        kind,
    })
}

fn item_label(kind: &str, item: &Value) -> String {
    let detail = match kind {
        "commandExecution" => item.get("command").and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Array(values) => Some(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        }),
        "fileChange" => item
            .get("changes")
            .and_then(Value::as_array)
            .map(|changes| {
                changes
                    .iter()
                    .take(8)
                    .filter_map(|change| {
                        let path = change.get("path").and_then(Value::as_str)?;
                        let change_kind = change
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("change");
                        Some(format!("{change_kind} {path}"))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        "mcpToolCall" => Some(format!(
            "{}/{}",
            item.get("server").and_then(Value::as_str).unwrap_or("MCP"),
            item.get("tool").and_then(Value::as_str).unwrap_or("tool")
        )),
        "dynamicToolCall" | "collabToolCall" | "collabAgentToolCall" => {
            item.get("tool").and_then(Value::as_str).map(str::to_owned)
        }
        "subAgentActivity" => Some(format!(
            "{} {}",
            item.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("activity"),
            item.get("agentPath")
                .and_then(Value::as_str)
                .unwrap_or("subagent")
        )),
        "webSearch" => item.get("query").and_then(Value::as_str).map(str::to_owned),
        "imageView" => item.get("path").and_then(Value::as_str).map(str::to_owned),
        "sleep" => item
            .get("durationMs")
            .and_then(Value::as_u64)
            .map(|duration| format!("{duration} ms")),
        "imageGeneration" => Some("generated image".into()),
        "enteredReviewMode" => Some("entered review mode".into()),
        "exitedReviewMode" => Some("exited review mode".into()),
        "contextCompaction" => Some("conversation context".into()),
        _ => None,
    };
    match detail.filter(|value| !value.is_empty()) {
        Some(detail) => format!("{kind}: {}", bounded_text(&detail, 1024)),
        None => kind.to_owned(),
    }
}
