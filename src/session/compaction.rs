//! Durable, lossy continuation checkpoints over immutable conversation history.

use crate::{
    identity::{CompactionId, ConversationEntryId, OperationId},
    message::{ContentBlock, Message, Role},
    prompt::{PromptBudgetPlan, estimate_message_tokens},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, error::Error, fmt};

pub(crate) const COMPACTION_CHECKPOINT_VERSION: u16 = 1;
const MAX_SUMMARY_ITEMS: usize = 16;
const MAX_ITEM_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompactionReason {
    AutomaticThreshold,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompactionSummary {
    pub(crate) goal: Option<String>,
    pub(crate) constraints: Vec<String>,
    pub(crate) progress: Vec<String>,
    pub(crate) decisions: Vec<String>,
    pub(crate) unresolved: Vec<String>,
    pub(crate) references: Vec<String>,
}

impl CompactionSummary {
    pub(crate) fn derive<'a>(
        previous: Option<&Self>,
        messages: impl IntoIterator<Item = &'a Message>,
        max_bytes: usize,
    ) -> Self {
        let mut summary = previous.cloned().unwrap_or_default();
        for message in messages {
            for text in message_texts(message) {
                for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                    let line = bounded_text(line, MAX_ITEM_BYTES);
                    let lower = line.to_ascii_lowercase();
                    if summary.goal.is_none() && message.role == Role::User {
                        summary.goal = Some(line.clone());
                    }
                    if contains_any(
                        &lower,
                        &["must", "should", "do not", "don't", "require", "constraint"],
                    ) {
                        push_unique(&mut summary.constraints, line.clone());
                    }
                    if contains_any(
                        &lower,
                        &[
                            "decided", "decision", "approved", "we will", "use ", "choose",
                        ],
                    ) {
                        push_unique(&mut summary.decisions, line.clone());
                    }
                    if line.ends_with('?')
                        || contains_any(
                            &lower,
                            &[
                                "todo",
                                "remaining",
                                "unresolved",
                                "blocked",
                                "failed",
                                "error",
                            ],
                        )
                    {
                        push_unique(&mut summary.unresolved, line.clone());
                    }
                    if matches!(message.role, Role::Assistant | Role::Tool) {
                        push_unique(&mut summary.progress, line.clone());
                    }
                    for reference in extract_references(&line) {
                        push_unique(&mut summary.references, reference);
                    }
                }
            }
            for reference in message_references(message) {
                push_unique(&mut summary.references, reference);
            }
        }
        summary.enforce_bounds(max_bytes);
        summary
    }

    pub(crate) fn render(&self) -> String {
        let mut sections = Vec::new();
        if let Some(goal) = &self.goal {
            sections.push(format!("Goal:\n- {goal}"));
        }
        append_section(&mut sections, "Constraints", &self.constraints);
        append_section(&mut sections, "Progress", &self.progress);
        append_section(&mut sections, "Decisions", &self.decisions);
        append_section(&mut sections, "Unresolved work", &self.unresolved);
        append_section(
            &mut sections,
            "File and artifact references",
            &self.references,
        );
        if sections.is_empty() {
            "No textual continuation facts were recoverable from the compacted span.".to_owned()
        } else {
            sections.join("\n\n")
        }
    }

    fn enforce_bounds(&mut self, max_bytes: usize) {
        self.constraints.truncate(MAX_SUMMARY_ITEMS);
        self.progress.truncate(MAX_SUMMARY_ITEMS);
        self.decisions.truncate(MAX_SUMMARY_ITEMS);
        self.unresolved.truncate(MAX_SUMMARY_ITEMS);
        self.references.truncate(MAX_SUMMARY_ITEMS);

        while self.render().len() > max_bytes {
            let candidates = [
                &mut self.progress,
                &mut self.references,
                &mut self.unresolved,
                &mut self.decisions,
                &mut self.constraints,
            ];
            let Some(longest) = candidates.into_iter().max_by_key(|items| items.len()) else {
                break;
            };
            if longest.pop().is_none() {
                self.goal = self
                    .goal
                    .take()
                    .map(|goal| bounded_text(&goal, max_bytes.saturating_sub(16)));
                break;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompactionCheckpoint {
    pub(crate) version: u16,
    pub(crate) id: CompactionId,
    pub(crate) operation_id: OperationId,
    pub(crate) previous_checkpoint: Option<CompactionId>,
    pub(crate) reason: CompactionReason,
    pub(crate) source_start: ConversationEntryId,
    pub(crate) source_end: ConversationEntryId,
    pub(crate) source_entry_count: usize,
    pub(crate) source_digest: String,
    pub(crate) retained_tail_start: ConversationEntryId,
    pub(crate) summary: CompactionSummary,
    pub(crate) budget: PromptBudgetPlan,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PromptContinuation {
    pub(crate) history: Vec<Message>,
    pub(crate) checkpoint: Option<CompactionCheckpoint>,
}

pub(crate) fn select_retained_start(messages: &[&Message], retained_tokens: usize) -> usize {
    if messages.len() < 2 {
        return 0;
    }
    let mut turn_starts = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == Role::User).then_some(index))
        .collect::<Vec<_>>();
    if turn_starts.is_empty() {
        turn_starts.push(messages.len() - 1);
    }

    let mut selected = *turn_starts.last().expect("turn start exists");
    for start in turn_starts.into_iter().rev() {
        let tokens = messages[start..]
            .iter()
            .map(|message| estimate_message_tokens(message))
            .fold(0_usize, usize::saturating_add);
        if tokens > retained_tokens && start != selected {
            break;
        }
        selected = start;
    }
    selected
}

pub(crate) fn source_digest<'a>(
    entries: impl IntoIterator<Item = (ConversationEntryId, &'a Message)>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    for (id, message) in entries {
        let id = id.to_string();
        hasher.update(&(id.len() as u64).to_le_bytes());
        hasher.update(id.as_bytes());
        let encoded = serde_json::to_vec(message).expect("Message serialization is infallible");
        hasher.update(&(encoded.len() as u64).to_le_bytes());
        hasher.update(&encoded);
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) fn validate_summary(summary: &CompactionSummary, max_bytes: usize) -> bool {
    summary.render().len() <= max_bytes
        && [
            &summary.constraints,
            &summary.progress,
            &summary.decisions,
            &summary.unresolved,
            &summary.references,
        ]
        .into_iter()
        .all(|items| {
            items.len() <= MAX_SUMMARY_ITEMS
                && items
                    .iter()
                    .all(|item| !item.trim().is_empty() && item.len() <= MAX_ITEM_BYTES)
        })
        && summary
            .goal
            .as_ref()
            .is_none_or(|goal| !goal.trim().is_empty() && goal.len() <= MAX_ITEM_BYTES)
}

fn message_texts(message: &Message) -> impl Iterator<Item = &str> {
    message.content.iter().filter_map(|block| match block {
        ContentBlock::Text(text) => Some(text.as_str()),
        ContentBlock::ToolResult(result) => Some(result.output.as_str()),
        ContentBlock::ToolCall(_) | ContentBlock::Image(_) => None,
    })
}

fn message_references(message: &Message) -> Vec<String> {
    let mut references = BTreeSet::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(text) => collect_text_references(text, &mut references),
            ContentBlock::ToolResult(result) => {
                collect_text_references(&result.output, &mut references);
                if let Some(artifact) = &result.artifact {
                    references.insert(format!(
                        "artifact:{}@{}",
                        artifact.reference.id,
                        artifact.reference.content_hash.as_str()
                    ));
                }
            }
            ContentBlock::ToolCall(call) => {
                collect_json_references(&call.arguments, &mut references);
            }
            ContentBlock::Image(image) => {
                references.insert(format!("artifact:{}", image.artifact.reference.id));
            }
        }
    }
    references.into_iter().collect()
}

fn collect_json_references(value: &Value, references: &mut BTreeSet<String>) {
    match value {
        Value::String(value) => collect_text_references(value, references),
        Value::Array(values) => {
            for value in values {
                collect_json_references(value, references);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_json_references(value, references);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_text_references(text: &str, references: &mut BTreeSet<String>) {
    for line in text.lines() {
        references.extend(extract_references(line));
    }
}

fn append_section(sections: &mut Vec<String>, title: &str, items: &[String]) {
    if !items.is_empty() {
        sections.push(format!(
            "{title}:\n{}",
            items
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn push_unique(items: &mut Vec<String>, value: String) {
    if items.len() < MAX_SUMMARY_ITEMS && !items.iter().any(|item| item == &value) {
        items.push(value);
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    if max_bytes <= 3 {
        return ".".repeat(max_bytes);
    }
    let mut end = (max_bytes - 3).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn extract_references(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| {
                matches!(
                    character,
                    ',' | '.' | ';' | ':' | '(' | ')' | '[' | ']' | '`' | '"'
                )
            })
        })
        .filter(|word| {
            word.starts_with("http://")
                || word.starts_with("https://")
                || word.contains('/')
                || word.contains('\\')
                || [".rs", ".toml", ".md", ".json", ".yaml", ".yml"]
                    .iter()
                    .any(|extension| word.ends_with(extension))
        })
        .filter(|word| !word.is_empty())
        .map(|word| bounded_text(word, MAX_ITEM_BYTES))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompactionError {
    NothingToCompact,
}

impl fmt::Display for CompactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NothingToCompact => "conversation has no complete older turn to compact",
        })
    }
}

impl Error for CompactionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_starts_at_a_user_turn_boundary() {
        let messages = [
            Message::text(Role::User, "old"),
            Message::text(Role::Assistant, "old response"),
            Message::text(Role::User, "recent"),
            Message::text(Role::Assistant, "recent response"),
        ];
        let references = messages.iter().collect::<Vec<_>>();

        assert_eq!(select_retained_start(&references, 8), 2);
    }

    #[test]
    fn summary_is_structured_deduplicated_and_bounded() {
        let artifact_id = crate::identity::ArtifactId::new();
        let messages = vec![
            Message::text(Role::User, "We must update src/main.rs. What remains?"),
            Message::text(
                Role::Assistant,
                "Decision: use the existing API. src/main.rs updated.",
            ),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolCall(crate::message::ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: serde_json::json!({"path": "docs/architecture/README.md"}),
                })],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Image(crate::vision::ImageRef {
                    artifact: crate::artifact::ArtifactRecord {
                        reference: crate::artifact::ArtifactRef {
                            id: artifact_id,
                            content_hash: crate::artifact::ContentHash::for_bytes(b"image"),
                        },
                        media_type: "image/png".to_owned(),
                        byte_len: 5,
                        owner: crate::identity::PrincipalId::new(),
                    },
                    media_type: "image/png".to_owned(),
                    byte_len: 5,
                    width: Some(1),
                    height: Some(1),
                })],
            },
        ];
        let summary = CompactionSummary::derive(None, &messages, 2_048);

        assert_eq!(
            summary.goal.as_deref(),
            Some("We must update src/main.rs. What remains?")
        );
        assert!(!summary.constraints.is_empty());
        assert!(!summary.decisions.is_empty());
        assert!(!summary.unresolved.is_empty());
        assert!(
            summary
                .references
                .iter()
                .any(|value| value == "src/main.rs")
        );
        assert!(
            summary
                .references
                .iter()
                .any(|value| value == "docs/architecture/README.md")
        );
        assert!(
            summary
                .references
                .iter()
                .any(|value| { value == &format!("artifact:{artifact_id}") })
        );
        assert!(validate_summary(&summary, 2_048));
    }
}
