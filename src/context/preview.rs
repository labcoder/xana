//! Pure selectors, canonical text handling, and conservative token estimates.

use super::{ContextError, ContextPreview, ContextSource, PreviewSelector};

const CHARS_PER_ESTIMATED_TOKEN: usize = 3;

/// ASCII uses the existing three-byte heuristic. Non-ASCII is charged at one
/// token per UTF-8 byte so CJK, combining marks and emoji cannot receive the
/// English compression discount. This is an estimate, not a provider tokenizer
/// or a guaranteed upper bound for an unknown model.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.bytes()
        .fold(0_usize, |units, byte| {
            units.saturating_add(if byte.is_ascii() {
                1
            } else {
                CHARS_PER_ESTIMATED_TOKEN
            })
        })
        .div_ceil(CHARS_PER_ESTIMATED_TOKEN)
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
    let bounded = bounded_text(text, text.len(), max_tokens);
    (bounded.to_owned(), bounded.len() < text.len())
}

/// Shared budget rule for transient previews and durable context views.
pub(crate) fn bounded_text(text: &str, max_bytes: usize, max_tokens: usize) -> &str {
    let text = &text[..text.floor_char_boundary(text.len().min(max_bytes))];
    let max_units = max_tokens.saturating_mul(CHARS_PER_ESTIMATED_TOKEN);
    let mut used = 0_usize;
    for (offset, character) in text.char_indices() {
        let units = if character.is_ascii() {
            1
        } else {
            character.len_utf8() * CHARS_PER_ESTIMATED_TOKEN
        };
        used = used.saturating_add(units);
        if used > max_units {
            return &text[..offset];
        }
    }
    text
}
