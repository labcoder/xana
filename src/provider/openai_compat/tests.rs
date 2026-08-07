use super::*;
use crate::message::ToolResult;
use std::error::Error;

fn first_fixture_message(json: &str) -> WireMessage {
    let response = match serde_json::from_str::<WireChatResponse>(json) {
        Ok(response) => response,
        Err(error) => panic!("expected captured response to decode: {error}"),
    };

    match response.choices.into_iter().next() {
        Some(choice) => choice.message,
        None => panic!("expected captured response to contain one choice"),
    }
}

fn assert_wire_messages_match(expected: &WireMessage, actual: &WireMessage) {
    assert_eq!(actual.role, expected.role);
    assert_eq!(actual.content, expected.content);
    assert_eq!(actual.tool_call_id, expected.tool_call_id);
    let expected_calls = expected.tool_calls.as_deref().unwrap_or(&[]);
    let actual_calls = actual.tool_calls.as_deref().unwrap_or(&[]);

    assert_eq!(actual_calls.len(), expected_calls.len());

    for (expected_call, actual_call) in expected_calls.iter().zip(actual_calls) {
        assert_eq!(actual_call.id, expected_call.id);
        assert_eq!(actual_call.kind, expected_call.kind);
        assert_eq!(actual_call.function.name, expected_call.function.name);

        let expected_arguments =
            match serde_json::from_str::<serde_json::Value>(&expected_call.function.arguments) {
                Ok(arguments) => arguments,
                Err(error) => {
                    panic!("expected fixture tool arguments to be JSON: {error}")
                }
            };

        let actual_arguments =
            match serde_json::from_str::<serde_json::Value>(&actual_call.function.arguments) {
                Ok(arguments) => arguments,
                Err(error) => {
                    panic!("expected round-trip tool arguments to be JSON: {error}")
                }
            };

        assert_eq!(actual_arguments, expected_arguments);
    }
}

#[test]
fn roles_convert_to_wire() {
    assert_eq!(WireRole::from(Role::System), WireRole::System);
    assert_eq!(WireRole::from(Role::User), WireRole::User);
    assert_eq!(WireRole::from(Role::Assistant), WireRole::Assistant);
    assert_eq!(WireRole::from(Role::Tool), WireRole::Tool);
}

#[test]
fn system_role_serializes_at_the_wire_edge() {
    let message = Message::text(Role::System, "frozen system prompt");
    let wire = WireMessage::try_from(&message).expect("system wire message");
    let value = serde_json::to_value(&wire).expect("wire JSON");

    assert_eq!(wire.role, WireRole::System);
    assert_eq!(wire.content.as_deref(), Some("frozen system prompt"));
    assert_eq!(value["role"], "system");
}

#[test]
fn tool_results_serialize_status_and_original_call_id() {
    let success = Message::tool_result(ToolResult::success("call-ok", "contents"));
    let error = Message::tool_result(ToolResult::error("call-err", "missing file"));

    let success_wire = WireMessage::try_from(&success).expect("success wire message");
    let error_wire = WireMessage::try_from(&error).expect("error wire message");

    assert_eq!(success_wire.role, WireRole::Tool);
    assert_eq!(success_wire.content.as_deref(), Some("contents"));
    assert_eq!(success_wire.tool_call_id.as_deref(), Some("call-ok"));
    assert_eq!(error_wire.content.as_deref(), Some("ERROR: missing file"));
    assert_eq!(error_wire.tool_call_id.as_deref(), Some("call-err"));
}

#[test]
fn tool_role_from_provider_is_rejected_as_a_response() {
    let wire = WireMessage {
        role: WireRole::Tool,
        content: Some("contents".to_owned()),
        tool_calls: None,
        tool_call_id: Some("call-1".to_owned()),
    };

    assert!(matches!(
        Message::try_from(wire),
        Err(MessageConversionError::UnexpectedResponseRole(
            WireRole::Tool
        ))
    ));
}

#[test]
fn invalid_tool_message_fails_before_http() {
    let client =
        OpenAiCompatClient::new("http://127.0.0.1:9/v1".to_owned(), "test-model".to_owned());
    let message = Message::text(Role::Tool, "tool messages require a tool-result block");

    let result = client.send_message(&[message], &[]);

    match result {
        Err(error) => {
            assert_eq!(error.kind, OpenAiCompatErrorKind::RequestConversion);
            assert!(error.source().is_some());
        }
        Ok(_) => panic!("expected request conversion to fail"),
    }
}

#[test]
fn tool_call_arguments_cross_as_structured_json() {
    let original = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        arguments: serde_json::json!({"path": "README.md"}),
    };

    let wire = WireToolCall::from(&original);

    assert_eq!(wire.function.arguments, r#"{"path":"README.md"}"#);

    let round_tripped = match ToolCall::try_from(wire) {
        Ok(tool_call) => tool_call,
        Err(error) => panic!("expected tool call to convert: {error}"),
    };

    assert_eq!(round_tripped, original);
}

#[test]
fn invalid_tool_arguments_return_the_call_id() {
    let wire = WireToolCall {
        id: "call-bad-json".to_owned(),
        kind: WireToolKind::Function,
        function: WireFunctionCall {
            name: "read_file".to_owned(),
            arguments: "not json".to_owned(),
        },
    };

    let result = ToolCall::try_from(wire);

    assert!(matches!(
        result,
        Err(MessageConversionError::InvalidToolArguments {
            tool_call_id,
            ..
        }) if tool_call_id == "call-bad-json"
    ));
}

#[test]
fn null_tool_calls_decode_as_absent() {
    let json = r#"{
        "role": "assistant",
        "content": "hello",
        "tool_calls": null
    }"#;

    let wire = match serde_json::from_str::<WireMessage>(json) {
        Ok(wire) => wire,
        Err(error) => panic!("expected wire message to decode: {error}"),
    };

    assert!(wire.tool_calls.is_none());

    let message = match Message::try_from(wire) {
        Ok(message) => message,
        Err(error) => panic!("expected wire message to convert: {error}"),
    };

    assert!(matches!(
        message.content.as_slice(),
        [ContentBlock::Text(text)] if text == "hello"
    ));
}

#[test]
fn text_and_tool_calls_use_the_wire_canonical_order() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text("first".to_owned()),
            ContentBlock::Text(" second".to_owned()),
            ContentBlock::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "README.md"}),
            }),
        ],
    };

    let wire = match WireMessage::try_from(&message) {
        Ok(wire) => wire,
        Err(error) => panic!("expected message to convert: {error}"),
    };

    assert_eq!(wire.content.as_deref(), Some("first second"));
    assert!(matches!(
        wire.tool_calls.as_deref(),
        Some([tool_call])
            if tool_call.id == "call-1"
                && tool_call.function.name == "read_file"
    ));
}

#[test]
fn text_after_a_tool_call_is_rejected() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "README.md"}),
            }),
            ContentBlock::Text("this cannot remain after the call".to_owned()),
        ],
    };

    let result = WireMessage::try_from(&message);

    assert!(matches!(
        result,
        Err(MessageConversionError::TextAfterToolCall)
    ));
}

#[test]
fn unrepresentable_history_fails_before_http() {
    let client =
        OpenAiCompatClient::new("http://127.0.0.1:9/v1".to_owned(), "test-model".to_owned());
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "README.md"}),
            }),
            ContentBlock::Text("not representable afterward".to_owned()),
        ],
    };

    let result = client.send_message(&[message], &[]);

    match result {
        Err(error) => {
            assert_eq!(error.kind, OpenAiCompatErrorKind::RequestConversion);
            assert!(error.source().is_some());
        }
        Ok(_) => panic!("expected request conversion to fail"),
    }
}

#[test]
fn captured_text_response_round_trips_through_internal_message() {
    let original = first_fixture_message(include_str!("fixtures/chat_text_response.json"));

    let internal = match Message::try_from(original.clone()) {
        Ok(message) => message,
        Err(error) => panic!("expected text fixture to convert: {error}"),
    };

    assert_eq!(internal.role, Role::Assistant);
    assert!(
        internal
            .content
            .iter()
            .any(|block| { matches!(block, ContentBlock::Text(text) if !text.is_empty()) })
    );

    let round_tripped = match WireMessage::try_from(&internal) {
        Ok(message) => message,
        Err(error) => panic!("expected internal message to convert: {error}"),
    };

    assert_wire_messages_match(&original, &round_tripped);
}

#[test]
fn captured_tool_call_response_round_trips_through_internal_message() {
    let original = first_fixture_message(include_str!("fixtures/chat_tool_call_response.json"));

    let internal = match Message::try_from(original.clone()) {
        Ok(message) => message,
        Err(error) => panic!("expected tool fixture to convert: {error}"),
    };

    assert!(internal.content.iter().any(|block| {
        matches!(
            block,
            ContentBlock::ToolCall(tool_call)
                if tool_call.name == "lookup_weather"
                    && tool_call
                        .arguments
                        .get("city")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|city| city.to_lowercase().contains("paris"))
        )
    }));

    let round_tripped = match WireMessage::try_from(&internal) {
        Ok(message) => message,
        Err(error) => panic!("expected internal message to convert: {error}"),
    };

    assert_wire_messages_match(&original, &round_tripped);
}

#[test]
fn endpoint_appends_chat_route_without_double_slash() {
    let without_trailing_slash = chat_endpoint("http://localhost:11434/v1");
    let with_trailing_slash = chat_endpoint("http://localhost:11434/v1/");

    assert_eq!(
        without_trailing_slash,
        "http://localhost:11434/v1/chat/completions"
    );
    assert_eq!(
        with_trailing_slash,
        "http://localhost:11434/v1/chat/completions"
    );
}

#[test]
fn request_serializes_internal_history_at_the_wire_edge() {
    let messages = [
        Message::text(Role::User, "Read README.md"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                arguments: serde_json::json!({"path": "README.md"}),
            })],
        },
        Message::tool_result(ToolResult::success("call-1", "README contents")),
    ];

    let wire_messages = match messages
        .iter()
        .map(WireMessage::try_from)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(messages) => messages,
        Err(error) => panic!("expected history to convert: {error}"),
    };

    let request = WireChatRequest {
        model: "qwen2.5-coder:7b",
        messages: wire_messages,
        stream: false,
        tools: Vec::new(),
    };

    let value = match serde_json::to_value(&request) {
        Ok(value) => value,
        Err(error) => panic!("expected chat request to serialize: {error}"),
    };

    assert_eq!(value["model"], "qwen2.5-coder:7b");
    assert_eq!(value["stream"], false);
    assert!(value.get("tools").is_none());

    let messages = &value["messages"];

    assert_eq!(messages.as_array().map(Vec::len), Some(3));

    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "Read README.md");

    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(messages[1]["tool_calls"][0]["type"], "function");
    assert_eq!(
        messages[1]["tool_calls"][0]["function"]["name"],
        "read_file"
    );
    assert_eq!(
        messages[1]["tool_calls"][0]["function"]["arguments"],
        r#"{"path":"README.md"}"#
    );

    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["content"], "README contents");
    assert_eq!(messages[2]["tool_call_id"], "call-1");
}

#[test]
fn request_serializes_all_registry_definitions_without_runtime_metadata() {
    let registry = crate::tool::ToolRegistry::builtins_for_tests().expect("built-in registry");
    let definitions = registry.definitions();
    let request = WireChatRequest {
        model: "test-model",
        messages: Vec::new(),
        stream: false,
        tools: definitions
            .iter()
            .map(|definition| WireToolDefinition::from(*definition))
            .collect(),
    };

    let value = serde_json::to_value(&request).expect("request JSON");
    let tools = value["tools"].as_array().expect("tool array");

    assert_eq!(tools.len(), 4);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["function"]["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>(),
        vec!["read_file", "list_files", "edit_file", "run_command"]
    );

    for tool in tools {
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["parameters"]["type"], "object");
        assert_eq!(
            tool["function"]["parameters"]["additionalProperties"],
            false
        );
        assert!(tool.get("effect_class").is_none());
        assert!(tool.get("replay_safety").is_none());
        assert!(tool["function"].get("effect_class").is_none());
        assert!(tool["function"].get("replay_safety").is_none());
    }

    let read_file = tools
        .iter()
        .find(|tool| tool["function"]["name"] == "read_file")
        .expect("read_file schema");
    let list_files = tools
        .iter()
        .find(|tool| tool["function"]["name"] == "list_files")
        .expect("list_files schema");
    let edit_file = tools
        .iter()
        .find(|tool| tool["function"]["name"] == "edit_file")
        .expect("edit_file schema");

    assert_eq!(
        read_file["function"]["parameters"]["required"],
        serde_json::json!(["path"])
    );
    assert_eq!(
        read_file["function"]["parameters"]["properties"]["start_line"]["minimum"],
        1
    );
    assert_eq!(
        read_file["function"]["parameters"]["properties"]["end_line"]["minimum"],
        1
    );
    assert_eq!(
        list_files["function"]["parameters"]["required"],
        serde_json::json!(["path"])
    );
    assert_eq!(
        edit_file["function"]["parameters"]["required"],
        serde_json::json!(["path", "old_text", "new_text"])
    );
}

#[test]
fn adjacent_text_blocks_normalize_to_one_wire_string() {
    let message = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text("one".to_owned()),
            ContentBlock::Text(" two".to_owned()),
            ContentBlock::Text("\nthree".to_owned()),
        ],
    };

    let wire = match WireMessage::try_from(&message) {
        Ok(message) => message,
        Err(error) => panic!("expected adjacent text to convert: {error}"),
    };

    assert_eq!(wire.content.as_deref(), Some("one two\nthree"));
    assert!(wire.tool_calls.is_none());

    let round_tripped = match Message::try_from(wire) {
        Ok(message) => message,
        Err(error) => panic!("expected wire text to convert: {error}"),
    };

    assert!(matches!(
        round_tripped.content.as_slice(),
        [ContentBlock::Text(text)] if text == "one two\nthree"
    ));
}
