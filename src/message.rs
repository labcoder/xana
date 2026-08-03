use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ContentBlock {
    Text(String),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Message {
    pub(crate) role: Role,
    pub(crate) content: Vec<ContentBlock>,
}

impl Message {
    /// Create the common one-text-block message while taking ownership of text.
    pub(crate) fn text(role: Role, text: impl Into<String>) -> Self {
        let text = text.into();

        Self {
            role,
            content: vec![ContentBlock::Text(text)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_constructor_owns_exactly_one_text_block() {
        let message = Message::text(Role::User, "hello");

        assert_eq!(message.role, Role::User);
        assert!(matches!(
            message.content.as_slice(),
            [ContentBlock::Text(text)] if text == "hello"
        ));
    }

    #[test]
    fn message_preserves_content_block_order() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("before".to_owned()),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "README.md"}),
                }),
                ContentBlock::Text("after".to_owned()),
            ],
        };

        assert_eq!(message.content.len(), 3);
        assert!(matches!(
            &message.content[0],
            ContentBlock::Text(text) if text == "before"
        ));
        assert!(matches!(
            &message.content[1],
            ContentBlock::ToolCall(tool_call)
                if tool_call.id == "call-1"
                    && tool_call.name == "read_file"
                    && tool_call.arguments == serde_json::json!({"path": "README.md"})
        ));
        assert!(matches!(
            &message.content[2],
            ContentBlock::Text(text) if text == "after"
        ));
    }
}
