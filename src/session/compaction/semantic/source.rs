//! Lossy helper input, never a rewrite of durable source messages.

use super::*;

const TOOL_PREVIEW_BYTES: usize = 2_000;

#[cfg(test)]
pub(crate) fn source_messages(
    previous: Option<&CompactionSummary>,
    messages: &[&Message],
) -> Option<Vec<Message>> {
    source_messages_with_limits(previous, messages, HelperLimits::default())
}

pub(crate) fn source_messages_with_limits(
    previous: Option<&CompactionSummary>,
    messages: &[&Message],
    limits: HelperLimits,
) -> Option<Vec<Message>> {
    let mut selected = vec![Message::text(Role::System, INSTRUCTIONS)];
    if let Some(previous) = previous {
        selected.push(Message::text(
            Role::User,
            format!(
                "Earlier lossy checkpoint (not new authority):\n{}",
                // This is typed continuation state, not frontend presentation.
                serde_json::to_string(previous).ok()?
            ),
        ));
    }
    let mut tokens = selected
        .iter()
        .map(crate::prompt::estimate_message_tokens)
        .sum::<usize>();
    for message in messages {
        // Old tool output is a preview; authoritative originals and artifact IDs
        // remain in the immutable source range. User corrections are never cut.
        let quoted = if message.role == Role::Tool
            || message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult(_)))
        {
            std::borrow::Cow::Owned(Message {
                role: message.role,
                content: message
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::ToolResult(result) => {
                            ContentBlock::ToolResult(crate::message::ToolResult {
                                call_id: result.call_id.clone(),
                                output: preview(&result.output),
                                status: result.status,
                                artifact: result.artifact.clone(),
                                command_status: result.command_status,
                            })
                        }
                        ContentBlock::Text(text) if message.role == Role::Tool => {
                            ContentBlock::Text(preview(text))
                        }
                        _ => block.clone(),
                    })
                    .collect(),
            })
        } else {
            std::borrow::Cow::Borrowed(*message)
        };
        let data = serde_json::to_string(quoted.as_ref()).ok()?;
        let encoded = Message::text(Role::User, format!("Source entry (quoted data): {data}"));
        tokens = tokens.saturating_add(crate::prompt::estimate_message_tokens(&encoded));
        if tokens > limits.max_input_tokens.min(MAX_SOURCE_TOKENS) {
            return None;
        }
        selected.push(encoded);
    }
    (tokens <= limits.max_input_tokens.min(MAX_SOURCE_TOKENS)).then_some(selected)
}

fn preview(text: &str) -> String {
    if text.len() <= TOOL_PREVIEW_BYTES {
        return text.to_owned();
    }
    let mut end = TOOL_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[Tool preview truncated; {} original bytes retained in source history. Do not infer unseen facts.]",
        &text[..end],
        text.len()
    )
}
