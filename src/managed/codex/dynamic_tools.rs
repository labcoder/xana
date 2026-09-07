//! The deliberately narrow experimental app-server personal-memory bridge.

use super::{CodexError, ManagedToolCall, ManagedToolResult};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_CALLS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_RESULT_BYTES: usize = 32 * 1024;
const TOOL_NAMES: &[&str] = &["memory_lookup", "memory_update"];

pub(super) fn definitions(
    definitions: Vec<crate::tool::ToolDefinition>,
) -> Result<Vec<Value>, CodexError> {
    if definitions.is_empty() {
        return Ok(Vec::new());
    }
    if definitions.len() != TOOL_NAMES.len()
        || TOOL_NAMES
            .iter()
            .any(|name| definitions.iter().filter(|tool| tool.name == *name).count() != 1)
    {
        return Err(CodexError::Protocol(
            "only the fixed Xana personal-memory tool contract is supported".into(),
        ));
    }
    Ok(definitions
        .into_iter()
        .map(|tool| {
            json!({
                "type":"function", "name":tool.name, "description":tool.description,
                "inputSchema":tool.parameters,
            })
        })
        .collect())
}

pub(super) fn decode(params: &Value) -> Result<ManagedToolCall, CodexError> {
    let string = |field| {
        params
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .map(str::to_owned)
            .ok_or_else(|| CodexError::Protocol(format!("invalid dynamic-tool {field}")))
    };
    let call_id = string("callId")?;
    let name = string("tool")?;
    if !TOOL_NAMES.contains(&name.as_str())
        || params
            .get("namespace")
            .is_some_and(|value| !value.is_null())
    {
        return Err(CodexError::Protocol(
            "unsupported dynamic tool or namespace".into(),
        ));
    }
    let arguments = params
        .get("arguments")
        .filter(|value| value.is_object())
        .ok_or_else(|| CodexError::Protocol("dynamic-tool arguments must be an object".into()))?;
    if serde_json::to_vec(arguments)
        .map_err(|error| CodexError::Protocol(error.to_string()))?
        .len()
        > MAX_ARGUMENT_BYTES
    {
        return Err(CodexError::Protocol(
            "dynamic-tool arguments exceed the bound".into(),
        ));
    }
    Ok(ManagedToolCall {
        call_id,
        name,
        arguments: arguments.clone(),
    })
}

pub(super) fn encode(result: ManagedToolResult) -> Result<Value, CodexError> {
    if result.text.len() > MAX_RESULT_BYTES {
        return Err(CodexError::Protocol(
            "personal-memory result exceeds the bridge bound; inspect its durable receipt".into(),
        ));
    }
    Ok(json!({"contentItems":[{"type":"inputText","text":result.text}],"success":result.success}))
}

pub(super) fn cancelled() -> ManagedToolResult {
    ManagedToolResult {
        text: "The owner turn ended before this memory operation began.".into(),
        success: false,
    }
}

#[derive(Default)]
pub(super) struct TurnReceipts {
    requests: usize,
    receipts: BTreeMap<String, (ManagedToolCall, Option<ManagedToolResult>)>,
}

impl TurnReceipts {
    pub(super) fn existing(
        &mut self,
        call: &ManagedToolCall,
    ) -> Result<Option<ManagedToolResult>, CodexError> {
        if self.requests >= MAX_CALLS {
            return Err(CodexError::Protocol(
                "personal-memory callbacks exceeded the per-turn bound".into(),
            ));
        }
        self.requests += 1;
        if let Some((previous, result)) = self.receipts.get(&call.call_id) {
            if previous != call {
                return Err(CodexError::Protocol(
                    "dynamic-tool call id was reused with different arguments".into(),
                ));
            }
            // Read results are not receipts: privacy controls or a correction
            // may change between callbacks. Re-run the governed lookup.
            return Ok(result.clone());
        }
        Ok(None)
    }

    pub(super) fn record(
        &mut self,
        call: ManagedToolCall,
        result: ManagedToolResult,
    ) -> Result<(), CodexError> {
        encode(result.clone())?;
        let receipt = (call.name == "memory_update").then_some(result);
        self.receipts.insert(call.call_id.clone(), (call, receipt));
        Ok(())
    }
}
