//! Loopback-only provider contracts: exact wire options and terminal accounting.

use super::*;
use crate::{credential::SecretString, message::Role};
use openai_compat::{HelperDialect, OpenAiCompatClient};
use serde_json::{Value, json};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct Sink(Mutex<Option<ProviderUsage>>);
impl DeltaSink for Sink {
    fn text_delta(&self, _: StepId, _: &str) {}
    fn usage(&self, usage: ProviderUsage) {
        *self.0.lock().unwrap() = Some(usage);
    }
}

fn policy(schema: Option<&Value>, disable_reasoning: bool) -> HelperGenerationPolicy<'_> {
    HelperGenerationPolicy {
        max_output_tokens: 123,
        json_schema: schema,
        disable_reasoning,
    }
}

fn schema() -> Value {
    json!({"type":"object","properties":{"summary":{"type":"string"}},
        "required":["summary"],"additionalProperties":false})
}

async fn server(body: String) -> (String, tokio::task::JoinHandle<(String, Value)>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let (headers, request) = loop {
            let mut chunk = [0; 2048];
            let count = stream.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + size {
                    break (
                        headers,
                        serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap(),
                    );
                }
            }
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        (headers, request)
    });
    (url, task)
}

fn chat_response(reason: &str, reasoning: bool) -> String {
    let delta = if reasoning {
        json!({"content":"{}", "reasoning":"private reasoning"})
    } else {
        json!({"content":"{}"})
    };
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":delta,"finish_reason":reason}]}),
        json!({"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":123,"total_tokens":132}})
    )
}

fn anthropic_response(reason: &str) -> String {
    [
        json!({"type":"message_start","message":{"usage":{"input_tokens":9}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"{}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":123}}),
        json!({"type":"message_stop"}),
    ].into_iter().map(|event| format!("data: {event}\n\n")).collect()
}

#[tokio::test]
async fn helper_wire_dialects_send_real_caps_schema_and_no_tools() {
    for dialect in [
        HelperDialect::Generic,
        HelperDialect::Ollama,
        HelperDialect::OpenAi,
        HelperDialect::OpenRouter,
    ] {
        let (url, server) = server(chat_response("stop", false)).await;
        let client = OpenAiCompatClient::with_bearer_and_attribution(
            url,
            "fixture-model".into(),
            SecretString::new("fixture-key".into()).unwrap(),
            Some("https://fixture.invalid".into()),
            Some("fixture-title".into()),
        )
        .with_helper_dialect(dialect);
        let capabilities = client.helper_capabilities();
        let schema = schema();
        client
            .stream_helper_message(
                &[Message::text(Role::User, "fixture input")],
                policy(
                    capabilities.structured_output.then_some(&schema),
                    capabilities.disable_reasoning,
                ),
                StepId::new(),
                &Sink::default(),
            )
            .await
            .unwrap();
        let (headers, request) = server.await.unwrap();
        assert!(headers.contains("Bearer fixture-key"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("http-referer: https://fixture.invalid")
        );
        assert_eq!(request["model"], "fixture-model");
        assert_eq!(request["stream"], true);
        assert_eq!(request["stream_options"]["include_usage"], true);
        assert!(request.get("tools").is_none());
        assert!(request.get("max_output_tokens").is_none());
        let cap = if dialect == HelperDialect::OpenAi {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        assert_eq!(request[cap], 123);
        let other = if dialect == HelperDialect::OpenAi {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        assert!(request.get(other).is_none());
        if capabilities.structured_output {
            assert_eq!(request["response_format"]["type"], "json_schema");
            assert_eq!(request["response_format"]["json_schema"]["schema"], schema);
            assert_eq!(request["response_format"]["json_schema"]["strict"], true);
        } else {
            assert!(request.get("response_format").is_none());
        }
        if dialect == HelperDialect::Ollama {
            assert_eq!(request["reasoning_effort"], "none");
        } else {
            assert!(request.get("reasoning_effort").is_none());
        }
        if dialect == HelperDialect::OpenRouter {
            assert_eq!(request["provider"]["require_parameters"], true);
        } else {
            assert!(request.get("provider").is_none());
        }
    }
}

#[tokio::test]
async fn ordinary_chat_does_not_inherit_helper_policy() {
    for dialect in [
        HelperDialect::Generic,
        HelperDialect::Ollama,
        HelperDialect::OpenAi,
        HelperDialect::OpenRouter,
    ] {
        let (url, server) = server(chat_response("length", false)).await;
        let client = OpenAiCompatClient::new(url, "fixture".into()).with_helper_dialect(dialect);
        client
            .stream_message(
                &[Message::text(Role::User, "fixture")],
                &[],
                StepId::new(),
                &Sink::default(),
            )
            .await
            .unwrap();
        let (_, request) = server.await.unwrap();
        for field in [
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "response_format",
            "reasoning_effort",
            "provider",
        ] {
            assert!(request.get(field).is_none(), "ordinary chat field: {field}");
        }
    }
}

#[tokio::test]
async fn unsupported_or_invalid_helper_options_fail_before_transport() {
    let client = OpenAiCompatClient::new("http://127.0.0.1:1".into(), "fixture".into());
    let schema = schema();
    for policy in [
        policy(Some(&schema), false),
        policy(None, true),
        HelperGenerationPolicy {
            max_output_tokens: 0,
            ..policy(None, false)
        },
    ] {
        let error = client
            .stream_helper_message(&[], policy, StepId::new(), &Sink::default())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Request);
    }
    let client = client.with_helper_dialect(HelperDialect::OpenAi);
    let malformed_schema = json!(null);
    let error = client
        .stream_helper_message(
            &[],
            policy(Some(&malformed_schema), false),
            StepId::new(),
            &Sink::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Request);
}

#[tokio::test]
async fn chat_helper_length_is_failure_even_for_valid_json_and_keeps_usage() {
    let (url, server) = server(chat_response("length", false)).await;
    let client = OpenAiCompatClient::new(url, "fixture".into()).with_usage();
    let sink = Sink::default();
    let error = client
        .stream_helper_message(&[], policy(None, false), StepId::new(), &sink)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::OutputLimit);
    assert_eq!(sink.0.lock().unwrap().unwrap().total_tokens, Some(132));
    server.await.unwrap();
}

#[tokio::test]
async fn helper_does_not_accept_reasoning_after_requesting_disabled() {
    let (url, server) = server(chat_response("stop", true)).await;
    let client = OpenAiCompatClient::new(url, "fixture".into())
        .with_helper_dialect(HelperDialect::Ollama)
        .with_usage();
    let sink = Sink::default();
    let error = client
        .stream_helper_message(&[], policy(None, true), StepId::new(), &sink)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::InvalidStream);
    assert_eq!(sink.0.lock().unwrap().unwrap().total_tokens, Some(132));
    server.await.unwrap();
}

#[tokio::test]
async fn anthropic_helper_wire_contract_and_output_limit_preserve_usage() {
    let (url, server) = server(anthropic_response("max_tokens")).await;
    let client = anthropic::AnthropicClient::new(
        url,
        SecretString::new("fixture-key".into()).unwrap(),
        "fixture-model",
    );
    let schema = schema();
    let sink = Sink::default();
    let error = client
        .stream_helper_message(
            &[Message::text(Role::User, "fixture")],
            policy(Some(&schema), true),
            StepId::new(),
            &sink,
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::OutputLimit);
    assert_eq!(sink.0.lock().unwrap().unwrap().input_tokens, Some(9));
    assert_eq!(sink.0.lock().unwrap().unwrap().output_tokens, Some(123));
    let (headers, request) = server.await.unwrap();
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("x-api-key: fixture-key")
    );
    assert!(headers.contains("2023-06-01"));
    assert_eq!(request["max_tokens"], 123);
    assert_eq!(request["output_config"]["format"]["type"], "json_schema");
    assert_eq!(request["output_config"]["format"]["schema"], schema);
    assert_eq!(request["thinking"]["type"], "disabled");
    assert!(request.get("tools").is_none());
    assert!(request.get("output_format").is_none());
}

#[tokio::test]
async fn anthropic_ordinary_request_preserves_default_body_and_completion() {
    let (url, server) = server(anthropic_response("max_tokens")).await;
    let client = anthropic::AnthropicClient::new(
        url,
        SecretString::new("fixture-key".into()).unwrap(),
        "fixture",
    );
    client
        .stream_message(
            &[Message::text(Role::User, "fixture")],
            &[],
            StepId::new(),
            &Sink::default(),
        )
        .await
        .unwrap();
    let (_, request) = server.await.unwrap();
    assert_eq!(request["max_tokens"], 4096);
    assert!(request.get("output_config").is_none());
    assert!(request.get("thinking").is_none());
}

#[tokio::test]
async fn ordinary_only_provider_cannot_silently_discard_helper_policy() {
    struct OrdinaryOnly;
    impl ConversationalProvider for OrdinaryOnly {
        fn stream_message<'a>(
            &'a self,
            _: &'a [Message],
            _: &'a [&'a ToolDefinition],
            _: StepId,
            _: &'a dyn DeltaSink,
        ) -> BoxFuture<'a, Result<Message, ProviderError>> {
            panic!("bounded helper must never fall through to ordinary chat")
        }
    }
    assert_eq!(
        OrdinaryOnly.helper_capabilities(),
        HelperCapabilities::default()
    );
    let error = OrdinaryOnly
        .stream_helper_message(&[], policy(None, false), StepId::new(), &Sink::default())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Request);
}
