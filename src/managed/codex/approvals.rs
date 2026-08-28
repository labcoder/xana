//! The supported approval contract: exact scope in, an offered decision out.

use super::{
    ApprovalDecision, ApprovalRequest, CodexError, MAX_ITEM_DETAIL_BYTES, ManagedEventHandler,
    bounded_text,
};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashSet};

pub(super) fn validate_scope(
    id: &Value,
    params: &Value,
    thread_id: &str,
    turn_id: &str,
    seen: &mut HashSet<String>,
) -> Result<(), CodexError> {
    if !id.is_i64()
        && !id.is_u64()
        && !id
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 256)
    {
        return Err(CodexError::Protocol("invalid approval request id".into()));
    }
    if params.get("threadId").and_then(Value::as_str) != Some(thread_id)
        || params.get("turnId").and_then(Value::as_str) != Some(turn_id)
    {
        return Err(CodexError::Protocol(
            "approval does not belong to the active thread and turn".into(),
        ));
    }
    if seen.len() >= 1024 || !seen.insert(id.to_string()) {
        return Err(CodexError::Protocol(
            "duplicate approval request or per-turn approval limit exceeded".into(),
        ));
    }
    Ok(())
}

pub(super) fn decode(method: &str, params: &Value) -> Result<ApprovalRequest, CodexError> {
    if !matches!(
        method,
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
    ) {
        return Err(CodexError::UnsupportedServerRequest(bounded_text(
            method, 256,
        )));
    }
    // These fields change the authority represented by a plain accept. Until
    // controllers can show and enforce them, do not disguise them as a command
    // or a one-off patch. In particular, grantRoot may authorize session writes.
    for field in ["networkApprovalContext", "grantRoot"] {
        if params.get(field).is_some_and(|value| !value.is_null()) {
            return Err(CodexError::Protocol(format!(
                "managed approval {field} is not supported by the current controller"
            )));
        }
    }
    let item_id = exact_optional_string(params, "itemId", 4096)?
        .ok_or_else(|| CodexError::Protocol("approval omitted itemId".into()))?;
    let command = exact_optional_string(params, "command", MAX_ITEM_DETAIL_BYTES)?;
    if method == "item/commandExecution/requestApproval" && command.is_none() {
        return Err(CodexError::Protocol(
            "command approval omitted command".into(),
        ));
    }
    Ok(ApprovalRequest {
        item_id: Some(item_id),
        method: method.to_owned(),
        available_decisions: available_decisions(params)?,
        reason: params
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| bounded_text(reason, 4096)),
        command,
        cwd: exact_optional_string(params, "cwd", 4096)?,
    })
}

pub(super) async fn answer<H: ManagedEventHandler + ?Sized>(
    request: &ApprovalRequest,
    handler: &mut H,
) -> Result<Value, CodexError> {
    let decision = handler.approve(request.clone()).await?;
    if !request.available_decisions.contains(decision.wire()) {
        return Err(CodexError::Protocol(
            "controller selected an approval decision that was not offered".into(),
        ));
    }
    Ok(json!({"decision":decision.wire()}))
}

pub(super) fn cancel(request: &ApprovalRequest) -> Result<Value, CodexError> {
    let decision = [ApprovalDecision::Cancel, ApprovalDecision::Decline]
        .into_iter()
        .find(|decision| request.available_decisions.contains(decision.wire()))
        .ok_or_else(|| CodexError::Protocol("approval offers no cancellation or denial".into()))?;
    Ok(json!({"decision":decision.wire()}))
}

fn exact_optional_string(
    params: &Value,
    field: &str,
    limit: usize,
) -> Result<Option<String>, CodexError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= limit => {
            Ok(Some(value.clone()))
        }
        _ => Err(CodexError::Protocol(format!(
            "approval {field} is empty, malformed, or exceeds {limit} bytes"
        ))),
    }
}

fn available_decisions(params: &Value) -> Result<BTreeSet<String>, CodexError> {
    let supported = ["accept", "acceptForSession", "decline", "cancel"];
    let values = match params.get("availableDecisions") {
        // The stable older schema has no decision-list field. Null is also an
        // omitted optional list; an explicitly malformed list is not a default.
        None | Some(Value::Null) => return Ok(supported.into_iter().map(str::to_owned).collect()),
        Some(Value::Array(values)) if values.len() <= 16 => values,
        _ => return Err(CodexError::Protocol("malformed availableDecisions".into())),
    };
    let mut decisions = BTreeSet::new();
    for value in values {
        match value {
            Value::String(value) if value.len() <= 64 => {
                if supported.contains(&value.as_str()) {
                    decisions.insert(value.clone());
                }
            }
            Value::Object(_) => {} // Persistent policy amendments are not supported.
            _ => return Err(CodexError::Protocol("malformed approval decision".into())),
        }
    }
    if decisions.is_empty() {
        return Err(CodexError::Protocol(
            "approval offers no supported decisions".into(),
        ));
    }
    Ok(decisions)
}
