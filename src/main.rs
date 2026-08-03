mod config;

use anyhow::{Context, Result};
use config::{XanaConfig, config_path};
use reqwest::blocking::Client;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Message {
    role: Role,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: Message,
}

#[derive(Debug, PartialEq, Eq)]
enum InputAction<'a> {
    Quit,
    Clear,
    Ignore,
    Send(&'a str),
}

fn classify_input(line: &str) -> InputAction<'_> {
    match line.trim() {
        "/quit" => InputAction::Quit,
        "/clear" => InputAction::Clear,
        "" => InputAction::Ignore,
        input => InputAction::Send(input),
    }
}

fn chat_endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn send_message(
    client: &Client,
    endpoint: &str,
    model: &str,
    messages: &[Message],
) -> Result<Message> {
    let request = ChatRequest {
        model,
        messages,
        stream: false,
    };

    let response = client
        .post(endpoint)
        .json(&request)
        .send()
        .with_context(|| format!("could not reach Xana provider at {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("Xana provider rejected request at {endpoint}"))?
        .json::<ChatResponse>()
        .with_context(|| format!("Xana provider returned invalid JSON at {endpoint}"))?;

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("Xana provider returned no chat choices")?;

    Ok(choice.message)
}

fn run_chat(config: XanaConfig) -> Result<()> {
    let endpoint = chat_endpoint(&config.base_url);
    let client = Client::new();
    let mut editor = DefaultEditor::new().context("could not initialize line editor")?;
    let mut messages = Vec::new();

    println!("Xana connected to {} using {}", endpoint, config.model);

    loop {
        match editor.readline("you> ") {
            Ok(line) => match classify_input(&line) {
                InputAction::Quit => {
                    break;
                }
                InputAction::Clear => {
                    messages.clear();
                    println!("rez> conversation cleared");
                }
                InputAction::Ignore => {}
                InputAction::Send(input) => {
                    editor
                        .add_history_entry(input)
                        .context("could not add input to editor history")?;

                    messages.push(Message {
                        role: Role::User,
                        content: input.to_owned(),
                    });

                    let assistant = send_message(&client, &endpoint, &config.model, &messages)?;

                    println!("rez> {}", assistant.content);

                    messages.push(assistant);
                }
            },
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let path = config_path().context("could not resolve Xana config path")?;

    println!("loading Xana config from {}", path.display());

    let config = XanaConfig::load_from(&path)
        .with_context(|| format!("failed to load config from {}", path.display()))?;

    run_chat(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_value<T: Serialize>(value: &T) -> serde_json::Value {
        match serde_json::to_value(value) {
            Ok(value) => value,
            Err(error) => panic!("test value should serialize: {error}"),
        }
    }

    #[test]
    fn classifies_commands_blanks_and_messages() {
        assert_eq!(classify_input("/quit"), InputAction::Quit);
        assert_eq!(classify_input("  /clear  "), InputAction::Clear);
        assert_eq!(classify_input("   "), InputAction::Ignore);
        assert_eq!(
            classify_input("  hello Rez  "),
            InputAction::Send("hello Rez")
        );
        assert_eq!(classify_input("clear"), InputAction::Send("clear"));
    }

    #[test]
    fn endpoint_appends_chat_route_without_double_slash() {
        assert_eq!(
            chat_endpoint("http://localhost:11434/v1"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_endpoint("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn request_serializes_role_and_full_history() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "first".to_owned(),
            },
            Message {
                role: Role::Assistant,
                content: "reply".to_owned(),
            },
        ];
        let request = ChatRequest {
            model: "qwen3:1.7b",
            messages: &messages,
            stream: false,
        };

        let value = json_value(&request);

        assert_eq!(value["model"], "qwen3:1.7b");
        assert_eq!(value["stream"], false);
        assert_eq!(value["messages"].as_array().map(Vec::len), Some(2));
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"], "first");
        assert_eq!(value["messages"][1]["role"], "assistant");
        assert_eq!(value["messages"][1]["content"], "reply");
    }

    #[test]
    fn response_deserializes_first_assistant_choice() {
        let raw = r#"{
            "id": "ignored",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "decoded"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3}
        }"#;

        let response = match serde_json::from_str::<ChatResponse>(raw) {
            Ok(response) => response,
            Err(error) => panic!("response fixture should decode: {error}"),
        };

        let message = match response.choices.into_iter().next() {
            Some(choice) => choice.message,
            None => panic!("fixture should contain one choice"),
        };

        assert_eq!(message.role, Role::Assistant);
        assert_eq!(message.content, "decoded");
    }
}
