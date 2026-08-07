use super::*;
use crate::message::ToolResult;
use crate::provider::openai_compat::wire::{
    WireChatRequest, WireFunctionDelta, WireMessage, WireToolCallDelta,
};

fn decode_in_chunks(input: &[u8], chunk_size: usize) -> Result<Vec<SseItem>, StreamError> {
    let mut decoder = SseDecoder::default();
    let mut items = Vec::new();
    for chunk in input.chunks(chunk_size) {
        items.extend(decoder.push(chunk)?);
    }
    decoder.finish()?;
    Ok(items)
}

#[test]
fn decoder_handles_one_byte_chunks_split_utf8_and_done() {
    let input = "data: {\"choices\":[{\"delta\":{\"content\":\"hé\"}}]}\n\ndata: [DONE]\n\n";
    let items = decode_in_chunks(input.as_bytes(), 1).expect("byte-wise stream");

    assert_eq!(items.len(), 2);
    assert!(
        matches!(&items[0], SseItem::Data(data) if String::from_utf8_lossy(data).contains("hé"))
    );
    assert_eq!(items[1], SseItem::Done);
}

#[test]
fn decoder_accepts_lf_crlf_comments_and_multiple_data_lines() {
    let input = b": keepalive\r\ndata: first\r\ndata: second\r\n\r\ndata: third\n\n";
    let items = decode_in_chunks(input, 3).expect("mixed stream framing");

    assert_eq!(
        items,
        vec![
            SseItem::Data(b"first\nsecond".to_vec()),
            SseItem::Data(b"third".to_vec()),
        ]
    );
}

#[test]
fn decoder_rejects_incomplete_and_oversized_frames() {
    let mut incomplete = SseDecoder::default();
    assert!(
        incomplete
            .push(b"data: unfinished")
            .expect("buffer")
            .is_empty()
    );
    assert!(matches!(
        incomplete.finish(),
        Err(StreamError::IncompleteFrame { .. })
    ));

    let mut oversized = SseDecoder::default();
    assert!(matches!(
        oversized.push(&vec![b'x'; MAX_UNDECODED_BYTES + 1]),
        Err(StreamError::FrameTooLarge { .. })
    ));
}

#[test]
fn accumulator_preserves_text_order() {
    let mut accumulator = StreamAccumulator::default();
    assert_eq!(
        accumulator
            .apply(WireDelta {
                content: Some("hel".to_owned()),
                tool_calls: None,
            })
            .expect("first delta"),
        vec!["hel"]
    );
    assert_eq!(
        accumulator
            .apply(WireDelta {
                content: Some("lo".to_owned()),
                tool_calls: None,
            })
            .expect("second delta"),
        vec!["lo"]
    );

    assert_eq!(
        accumulator.finish().expect("assistant message"),
        Message::text(Role::Assistant, "hello")
    );
}

#[test]
fn accumulator_joins_split_tool_identity_name_and_arguments() {
    let mut accumulator = StreamAccumulator::default();
    for delta in [
        WireToolCallDelta {
            index: 0,
            id: Some("call-".to_owned()),
            function: Some(WireFunctionDelta {
                name: Some("read_".to_owned()),
                arguments: Some("{\"pa".to_owned()),
            }),
        },
        WireToolCallDelta {
            index: 0,
            id: Some("1".to_owned()),
            function: Some(WireFunctionDelta {
                name: Some("file".to_owned()),
                arguments: Some("th\":\"README.md\"}".to_owned()),
            }),
        },
    ] {
        accumulator
            .apply(WireDelta {
                content: None,
                tool_calls: Some(vec![delta]),
            })
            .expect("tool delta");
    }

    assert_eq!(
        accumulator.finish().expect("tool call").content,
        vec![ContentBlock::ToolCall(ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
        })]
    );
}

#[test]
fn accumulator_rejects_malformed_arguments_at_finish() {
    let mut accumulator = StreamAccumulator::default();
    accumulator
        .apply(WireDelta {
            content: None,
            tool_calls: Some(vec![WireToolCallDelta {
                index: 0,
                id: Some("call-1".to_owned()),
                function: Some(WireFunctionDelta {
                    name: Some("read_file".to_owned()),
                    arguments: Some("not JSON".to_owned()),
                }),
            }]),
        })
        .expect("delta is accumulated before validation");

    assert!(matches!(
        accumulator.finish(),
        Err(StreamError::InvalidToolArguments { index: 0, .. })
    ));
}

#[test]
fn captured_text_stream_decodes_to_a_message() {
    let items = decode_in_chunks(include_bytes!("../fixtures/chat_text_stream.sse"), 2)
        .expect("captured text stream");
    let mut accumulator = StreamAccumulator::default();
    for item in items {
        if let SseItem::Data(data) = item {
            let response: crate::provider::openai_compat::wire::WireStreamResponse =
                serde_json::from_slice(&data).expect("wire delta");
            accumulator
                .apply(response.choices.into_iter().next().expect("choice").delta)
                .expect("text delta");
        }
    }

    assert_eq!(
        accumulator.finish().expect("message"),
        Message::text(Role::Assistant, "Hello from Xana")
    );
}

#[test]
fn captured_tool_stream_can_be_followed_by_a_tool_result_request() {
    let items = decode_in_chunks(include_bytes!("../fixtures/chat_tool_call_stream.sse"), 5)
        .expect("captured tool stream");
    let mut accumulator = StreamAccumulator::default();
    for item in items {
        if let SseItem::Data(data) = item {
            let response: crate::provider::openai_compat::wire::WireStreamResponse =
                serde_json::from_slice(&data).expect("wire delta");
            accumulator
                .apply(response.choices.into_iter().next().expect("choice").delta)
                .expect("tool delta");
        }
    }
    let assistant = accumulator.finish().expect("tool call message");
    let history = [
        Message::text(Role::User, "Read the README"),
        assistant,
        Message::tool_result(ToolResult::success("call-1", "Xana documentation")),
    ];
    let request = WireChatRequest {
        model: "test-model",
        messages: history
            .iter()
            .map(WireMessage::try_from)
            .collect::<Result<Vec<_>, _>>()
            .expect("representable tool-result history"),
        stream: true,
        tools: Vec::new(),
    };
    let value = serde_json::to_value(request).expect("request JSON");

    assert_eq!(value["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(value["messages"][2]["tool_call_id"], "call-1");
}
