//! Pure selectors, canonical text handling, and conservative token estimates.

use super::{ContextError, ContextPreview, ContextSource, PreviewSelector};

const CHARS_PER_ESTIMATED_TOKEN: usize = 3;

/// Estimates input tokens as one token per three Unicode scalar values,
/// rounded up. It intentionally errs above the common four-character rule and
/// is not a provider tokenizer.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_ESTIMATED_TOKEN)
}

pub(crate) fn preview(
    source: &ContextSource,
    selector: PreviewSelector,
) -> Result<ContextPreview, ContextError> {
    if source.max_tokens == 0 {
        return Err(ContextError::InvalidSourceBudget {
            id: source.id.clone(),
        });
    }

    let canonical = canonical_text(&source.content);
    let (selected, selector_truncated) = match &selector {
        PreviewSelector::Head => (canonical, false),
        PreviewSelector::Lines { start, end } => {
            if *start == 0 || end < start {
                return Err(ContextError::InvalidLineRange {
                    start: *start,
                    end: *end,
                });
            }
            let text = canonical
                .lines()
                .enumerate()
                .filter(|(index, _)| (*start..=*end).contains(&(index + 1)))
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n");
            (text, false)
        }
        PreviewSelector::LiteralSearch { query, max_matches } => {
            if query.is_empty() {
                return Err(ContextError::InvalidSearch {
                    reason: "query must not be empty",
                });
            }
            if *max_matches == 0 {
                return Err(ContextError::InvalidSearch {
                    reason: "max_matches must be greater than zero",
                });
            }

            let matching = canonical
                .lines()
                .filter(|line| line.contains(query))
                .collect::<Vec<_>>();
            let truncated = matching.len() > *max_matches;
            (
                matching
                    .into_iter()
                    .take(*max_matches)
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated,
            )
        }
    };
    let (text, budget_truncated) = bound_to_tokens(&selected, source.max_tokens);

    Ok(ContextPreview {
        source_id: source.id.clone(),
        provenance: source.provenance.clone(),
        trust: source.trust,
        selector,
        estimated_tokens: estimate_tokens(&text),
        text,
        truncated: selector_truncated || budget_truncated,
    })
}

pub(crate) fn canonical_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn bound_to_tokens(text: &str, max_tokens: usize) -> (String, bool) {
    let max_chars = max_tokens.saturating_mul(CHARS_PER_ESTIMATED_TOKEN);
    let mut chars = text.chars();
    let selected = chars.by_ref().take(max_chars).collect::<String>();
    let truncated = chars.next().is_some();
    (selected, truncated)
}
