//! Small deterministic owner-input grammar, not an automatic fact extractor.

use super::*;

#[cfg(test)]
mod tests;

pub(crate) enum NaturalIntent {
    Remember {
        scope: Option<MemoryScope>,
        statement: String,
    },
    ClarifyRememberScope,
    ClarifyRememberRequest,
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
    if let Some(intent) = conversational_remember(input) {
        return Some(Ok(intent));
    }
    // Established explicit command forms accept literal code, punctuation and
    // multiline facts even when the conversational templates cannot parse them.
    for (prefix, scope) in [
        ("remember that ", None),
        ("remember for this conversation: ", None),
        ("remember for all conversations: ", Some(MemoryScope::User)),
    ] {
        if lower.starts_with(prefix) {
            return Some(Ok(remember_intent(scope, input[prefix.len()..].trim())));
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

/// Explicit owner imperatives only. This is not inference over arbitrary prose:
/// quoted examples, recall questions and third-party content are not commands.
fn conversational_remember(input: &str) -> Option<NaturalIntent> {
    if input.contains(['\n', '\r', '`']) {
        return None;
    }
    if let Some(request) = remember_request(input) {
        let lower = request.to_ascii_lowercase();
        for (prefix, scope) in [
            ("remember that ", None),
            ("remember this: ", None),
            ("remember for this conversation: ", None),
            ("remember for all conversations: ", Some(MemoryScope::User)),
        ] {
            if lower.starts_with(prefix) {
                return Some(remember_intent(scope, request[prefix.len()..].trim()));
            }
        }
        if let Some(fact) = lower.strip_prefix("remember ")
            && personal_statement(fact)
            && !request_question(input)
        {
            return Some(remember_intent(None, request["remember ".len()..].trim()));
        }
    }
    // Split fact from request, not on a fixed sentence spelling. A later comma
    // can belong to a polite tag, so examine separators from the end. Only an
    // exact reference clause may consume the remainder of the owner input.
    for (index, separator) in input.char_indices().rev() {
        if !matches!(separator, '.' | ',' | ';' | '—') {
            continue;
        }
        let clause = &input[index + separator.len_utf8()..];
        // Only the short request clause is interpreted. Long pasted personal
        // text with many separators must not cause quadratic normalization.
        if clause.len() > 128 {
            break;
        }
        let statement = input[..index].trim();
        if !personal_statement(statement) {
            continue;
        }
        let Some(request) = remember_request(clause) else {
            continue;
        };
        let request = request.to_ascii_lowercase();
        let (reference, scope) = match request.strip_suffix(" for all conversations") {
            Some(reference) => (reference, Some(MemoryScope::User)),
            None => (request.as_str(), None),
        };
        if !matches!(reference, "remember that" | "remember this" | "remember it") {
            if [
                "remember that for ",
                "remember this for ",
                "remember it for ",
            ]
            .iter()
            .any(|prefix| request.starts_with(prefix))
            {
                return Some(NaturalIntent::ClarifyRememberScope);
            }
            continue;
        }
        return Some(remember_intent(scope, statement));
    }
    None
}

fn remember_intent(scope: Option<MemoryScope>, statement: &str) -> NaturalIntent {
    let lower = statement.to_ascii_lowercase();
    // A request with a deferred/contradictory save condition needs an owner
    // clarification, not partial execution. Fact negation (e.g. "not red") is
    // ordinary data and deliberately does not match these save-specific cues.
    if [
        "only if",
        "if i approve",
        "do not save",
        "don't save",
        "don’t save",
        "do not store",
        "don't store",
        "don’t store",
        "do not remember",
        "don't remember",
        "don’t remember",
        "not yet",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
    {
        return NaturalIntent::ClarifyRememberRequest;
    }
    if scope.is_none()
        && [
            " for all conversations",
            " for this project",
            " globally",
            " everywhere",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return NaturalIntent::ClarifyRememberScope;
    }
    NaturalIntent::Remember {
        scope,
        statement: statement.to_owned(),
    }
}

fn request_question(input: &str) -> bool {
    let lower = input.trim_start().to_ascii_lowercase();
    ["can you ", "could you ", "would you ", "will you "]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// Strip only explicit request wrappers. A recall question is not permission
/// to save, and an embedded question/compound request must not be partly run.
fn remember_request(input: &str) -> Option<&str> {
    let mut input = input.trim().trim_end_matches(['.', '!']).trim_end();
    let lower = input.to_ascii_lowercase();
    for suffix in [
        ", ok?",
        ", okay?",
        ", please?",
        ", please",
        ", ok",
        ", okay",
    ] {
        if lower.ends_with(suffix) {
            input = input[..input.len() - suffix.len()].trim_end();
            break;
        }
    }
    let lower = input.to_ascii_lowercase();
    for prefix in ["can you ", "could you ", "would you ", "will you "] {
        if lower.starts_with(prefix) {
            input = input[prefix.len()..].trim_end_matches('?').trim_end();
            break;
        }
    }
    if input.contains('?') {
        return None;
    }
    if input.to_ascii_lowercase().starts_with("please ") {
        input = &input["please ".len()..];
    }
    Some(input)
}

fn personal_statement(text: &str) -> bool {
    ["i ", "i'm ", "my ", "we ", "our "].iter().any(|prefix| {
        text.get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
    })
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
                NaturalIntent::ClarifyRememberRequest => {
                    return Ok("Xana memory control (local; no model call)\nNothing was saved: this request contains a condition or instruction not to save. Please clarify the fact and whether you want it saved now. No workspace files were read or changed.".into());
                }
                NaturalIntent::ClarifyRememberScope => {
                    return Ok("Xana memory control (local; no model call)\nNothing was saved: I could not resolve the requested memory scope. Say `remember for this conversation: FACT`, `remember for all conversations: FACT`, or `remember in SCOPE that FACT` with an exact scope ID. No workspace files were read or changed.".into());
                }
                NaturalIntent::Remember { scope, statement } => {
                    let ambiguous = scope.is_none();
                    let record =
                        self.remember(scope.map(Ok).unwrap_or_else(narrow)?, statement, None)?;
                    let notice = if ambiguous {
                        format!(
                            "Remembered for this Conversation only. To apply everywhere, say move memory {} to user.",
                            record.id
                        )
                    } else {
                        format!(
                            "Remembered in the explicitly selected scope: {}.",
                            record.scope
                        )
                    };
                    return Ok(format!(
                        "Xana memory control (local; no model call)\n{notice}\n{}\nMemory ID: {}",
                        record.statement, record.id
                    ));
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
