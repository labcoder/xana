//! Small deterministic owner-input grammar, not an automatic fact extractor.

use super::*;

pub(crate) enum NaturalIntent {
    Remember {
        scope: Option<MemoryScope>,
        statement: String,
    },
    Inspect,
    Correct {
        id: Uuid,
        statement: String,
    },
    Disable {
        id: Uuid,
    },
    Forget {
        id: Uuid,
    },
    Restore {
        id: Uuid,
    },
    Scope {
        id: Uuid,
        target: MemoryScope,
    },
    NoMemory(bool),
}

/// Recognizes only whole direct requests. Quoted/tool/child content is never
/// passed here by the owner adapters; ordinary questions remain model input.
pub(crate) fn parse_natural(input: &str) -> Option<Result<NaturalIntent>> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    if matches!(lower.as_str(), "what do you remember?" | "show my memories") {
        return Some(Ok(NaturalIntent::Inspect));
    }
    if lower == "disable memory for this conversation" {
        return Some(Ok(NaturalIntent::NoMemory(true)));
    }
    if lower == "enable memory for this conversation" {
        return Some(Ok(NaturalIntent::NoMemory(false)));
    }
    for (prefix, scope) in [
        ("remember that ", None),
        ("remember for this conversation: ", None),
        ("remember for all conversations: ", Some(MemoryScope::User)),
    ] {
        if lower.starts_with(prefix) {
            return Some(Ok(NaturalIntent::Remember {
                scope,
                statement: input[prefix.len()..].trim().to_owned(),
            }));
        }
    }
    if lower.starts_with("remember in ") {
        return Some((|| {
            let rest = &input["remember in ".len()..];
            let (scope,statement)=rest.split_once(" that ").context("Use remember in SCOPE that FACT; otherwise say remember that FACT to keep it in this Conversation")?;
            Ok(NaturalIntent::Remember {
                scope: Some(scope.parse()?),
                statement: statement.to_owned(),
            })
        })());
    }
    if lower.starts_with("correct memory ") {
        return Some((|| {
            let (id, statement) = input["correct memory ".len()..]
                .split_once(':')
                .context("Use correct memory UUID: FACT")?;
            Ok(NaturalIntent::Correct {
                id: id.trim().parse()?,
                statement: statement.trim().to_owned(),
            })
        })());
    }
    if lower.starts_with("disable memory ") {
        return Some(
            input["disable memory ".len()..]
                .trim()
                .parse()
                .map(|id| NaturalIntent::Disable { id })
                .map_err(Into::into),
        );
    }
    for (prefix, restore) in [("forget memory ", false), ("restore memory ", true)] {
        if lower.starts_with(prefix) {
            return Some(
                input[prefix.len()..]
                    .trim()
                    .parse()
                    .map(|id| {
                        if restore {
                            NaturalIntent::Restore { id }
                        } else {
                            NaturalIntent::Forget { id }
                        }
                    })
                    .map_err(Into::into),
            );
        }
    }
    if lower.starts_with("move memory ") {
        return Some((|| {
            let (id, target) = input["move memory ".len()..]
                .split_once(" to ")
                .context("Use move memory UUID to SCOPE")?;
            Ok(NaturalIntent::Scope {
                id: id.parse()?,
                target: target.parse()?,
            })
        })());
    }
    None
}

use anyhow::Context as _;

impl MemoryOwner {
    pub(crate) fn respond(&self, input: &str) -> Option<Result<String>> {
        let intent = parse_natural(input)?;
        Some((|| {
            let intent = intent?;
            let narrow = || {
                self.context.conversation.map(MemoryScope::Conversation).context("This control needs a Conversation identity; choose an explicit scope with xana memory instead")
            };
            let (value, notice) = match intent {
                NaturalIntent::Remember { scope, statement } => {
                    let ambiguous = scope.is_none();
                    let record =
                        self.remember(scope.map(Ok).unwrap_or_else(narrow)?, statement, None)?;
                    (
                        serde_json::to_value(record)?,
                        if ambiguous {
                            "Remembered for this Conversation only. Should this apply everywhere? You can say move memory UUID to user; nothing was widened."
                        } else {
                            "Remembered in the explicitly selected scope."
                        },
                    )
                }
                NaturalIntent::Inspect => {
                    let eligible = self.eligible()?;
                    // This reply becomes transcript context. Keep it much smaller
                    // than the storage page; full records remain in owner controls.
                    let previews = eligible.records.iter().take(8).map(|record| {
                        serde_json::json!({
                            "id":record.id,"revision":record.revision,"scope":record.scope,
                            "statement_preview":record.statement.chars().take(256).collect::<String>(),
                            "statement_truncated":record.statement.chars().count()>256,
                            "valid_until_unix_seconds":record.valid_until_unix_seconds,
                        })
                    }).collect::<Vec<_>>();
                    (
                        serde_json::json!({
                            "records":previews,"has_more":eligible.has_more || eligible.records.len()>8,
                            "use_enabled":eligible.use_enabled,"learning_enabled":eligible.learning_enabled,
                            "restore_review_required":eligible.restore_review_required,
                        }),
                        "Eligible memory preview only. Use memory show UUID, memory list, or Desktop Memory for complete records. Automatic prompt selection is not enabled by this inspection.",
                    )
                }
                NaturalIntent::Correct { id, statement } => {
                    let old = self.record(id)?;
                    (
                        serde_json::to_value(self.revise(
                            id,
                            old.revision,
                            MemoryEdit::Correct {
                                statement,
                                valid_until_unix_seconds: old.valid_until_unix_seconds,
                            },
                        )?)?,
                        "Corrected for the next eligible read/turn. Already dispatched work was not restarted.",
                    )
                }
                NaturalIntent::Disable { id } => {
                    let old = self.record(id)?;
                    (
                        serde_json::to_value(self.revise(
                            id,
                            old.revision,
                            MemoryEdit::Disable,
                        )?)?,
                        "Disabled for use; retained history is not erased. This is not robust forgetting.",
                    )
                }
                NaturalIntent::Forget { id } => {
                    let old = self.record(id)?;
                    let record = self.revise(id, old.revision, MemoryEdit::Forget)?;
                    (
                        serde_json::json!({"id":record.id,"revision":record.revision,"state":record.state}),
                        "Forgotten. Its source Conversation is excluded from automatic learning, recall and compaction; raw history and already disclosed provider context are separate. Start a fresh Conversation to avoid reusing old prompt context.",
                    )
                }
                NaturalIntent::Restore { id } => {
                    let old = self.record(id)?;
                    let record =
                        self.revise(id, old.revision, MemoryEdit::Restore { confirm: true })?;
                    (
                        serde_json::json!({"id":record.id,"revision":record.revision,"state":record.state}),
                        "The explicitly selected fact is restored; old source history remains excluded from automatic processing.",
                    )
                }
                NaturalIntent::Scope { id, target } => {
                    let old = self.record(id)?;
                    (
                        serde_json::to_value(self.revise(
                            id,
                            old.revision,
                            MemoryEdit::Scope {
                                target,
                                confirm: true,
                            },
                        )?)?,
                        "Moved only the named record to your explicitly requested scope.",
                    )
                }
                NaturalIntent::NoMemory(disabled) => (
                    serde_json::to_value(self.controls(
                        narrow()?,
                        MemoryControlEdit {
                            no_memory: Some(disabled),
                            ..Default::default()
                        },
                    )?)?,
                    "Conversation memory override updated. History and provider retention are separate.",
                ),
            };
            Ok(format!(
                "Xana memory control (local; no model call)\n{notice}\n{}",
                serde_json::to_string_pretty(&value)?
            ))
        })())
    }
}
