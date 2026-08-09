//! Bounded terminal-native Markdown and untrusted-text projection.

use crate::artifact::ArtifactRecord;

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 4096;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_LINKS: usize = 64;
const MAX_ARTIFACTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RichLineKind {
    Paragraph,
    Heading,
    List,
    Quote,
    Table,
    Code,
    DiffAdd,
    DiffRemove,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RichLine {
    pub(super) kind: RichLineKind,
    pub(super) text: String,
    pub(super) emphasized: bool,
    pub(super) inline_code: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SafeLink {
    pub(super) label: String,
    pub(super) target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArtifactView {
    pub(super) record: ArtifactRecord,
    pub(super) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RichDocument {
    pub(super) lines: Vec<RichLine>,
    pub(super) links: Vec<SafeLink>,
    pub(super) artifacts: Vec<ArtifactView>,
    pub(super) truncated: bool,
}

impl RichDocument {
    pub(super) fn parse(source: &str, artifacts: Vec<ArtifactView>) -> Self {
        let source = bounded(sanitize(source), MAX_SOURCE_BYTES);
        let mut lines = Vec::new();
        let mut links = Vec::new();
        let mut fenced = false;
        let mut diff = false;
        let mut truncated = false;
        for raw in source.lines() {
            if lines.len() >= MAX_LINES {
                truncated = true;
                break;
            }
            let line = raw.trim_end();
            if let Some(language) = line.trim_start().strip_prefix("```") {
                if fenced {
                    fenced = false;
                    diff = false;
                } else {
                    fenced = true;
                    diff = language.trim().eq_ignore_ascii_case("diff");
                }
                continue;
            }
            let (kind, text) = if fenced {
                let kind = if diff && line.starts_with('+') && !line.starts_with("+++") {
                    RichLineKind::DiffAdd
                } else if diff && line.starts_with('-') && !line.starts_with("---") {
                    RichLineKind::DiffRemove
                } else {
                    RichLineKind::Code
                };
                (kind, line.to_owned())
            } else if let Some(heading) = line.trim_start().strip_prefix('#') {
                (
                    RichLineKind::Heading,
                    heading.trim_start_matches('#').trim().to_owned(),
                )
            } else if let Some(quote) = line.trim_start().strip_prefix('>') {
                (RichLineKind::Quote, quote.trim_start().to_owned())
            } else if is_list(line) {
                (RichLineKind::List, normalize_list(line))
            } else if looks_like_table(line) {
                (RichLineKind::Table, line.trim().to_owned())
            } else {
                (RichLineKind::Paragraph, line.to_owned())
            };
            let text = extract_links(&text, &mut links);
            let (text, emphasized, inline_code) = inline_markers(text);
            lines.push(RichLine {
                kind,
                text: bounded(text, MAX_LINE_BYTES),
                emphasized,
                inline_code,
            });
        }
        if fenced && lines.len() < MAX_LINES {
            lines.push(RichLine {
                kind: RichLineKind::Warning,
                text: "[unterminated code fence]".to_owned(),
                emphasized: false,
                inline_code: false,
            });
        }
        let mut artifacts = artifacts;
        if artifacts.len() > MAX_ARTIFACTS {
            artifacts.truncate(MAX_ARTIFACTS);
            truncated = true;
        }
        Self {
            lines,
            links,
            artifacts,
            truncated,
        }
    }

    pub(super) fn plain(source: &str) -> Self {
        Self::parse(source, Vec::new())
    }

    pub(super) fn stream_append(&mut self, delta: &str) {
        let delta = sanitize(delta);
        for part in delta.split_inclusive('\n') {
            if self.lines.is_empty()
                || self
                    .lines
                    .last()
                    .is_some_and(|line| line.text.ends_with('\n'))
            {
                if self.lines.len() >= MAX_LINES {
                    self.truncated = true;
                    return;
                }
                self.lines.push(RichLine {
                    kind: RichLineKind::Paragraph,
                    text: String::new(),
                    emphasized: false,
                    inline_code: false,
                });
            }
            if let Some(line) = self.lines.last_mut() {
                line.text = bounded(format!("{}{}", line.text, part), MAX_LINE_BYTES);
            }
        }
    }
}

fn inline_markers(text: String) -> (String, bool, bool) {
    let emphasized = text.contains("**") || text.contains("__");
    let inline_code = text.matches('`').count() >= 2;
    (
        text.replace("**", "").replace("__", "").replace('`', ""),
        emphasized,
        inline_code,
    )
}

fn is_list(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || line.split_once(". ").is_some_and(|(prefix, _)| {
            !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn normalize_list(line: &str) -> String {
    let line = line.trim_start();
    if line.starts_with(['-', '*', '+']) {
        format!("- {}", line[1..].trim_start())
    } else {
        line.to_owned()
    }
}

fn looks_like_table(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 3
}

fn extract_links(source: &str, links: &mut Vec<SafeLink>) -> String {
    let mut output = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(open) = rest.find('[') {
        output.push_str(&rest[..open]);
        let candidate = &rest[open + 1..];
        let Some(close) = candidate.find("](") else {
            output.push_str(&rest[open..]);
            return output;
        };
        let label = &candidate[..close];
        let target_start = close + 2;
        let Some(target_end) = candidate[target_start..].find(')') else {
            output.push_str(&rest[open..]);
            return output;
        };
        let target = &candidate[target_start..target_start + target_end];
        if links.len() < MAX_LINKS && safe_link_target(target) {
            links.push(SafeLink {
                label: bounded(label.to_owned(), 512),
                target: bounded(target.to_owned(), 4096),
            });
            output.push_str(label);
            output.push_str(" [link]");
        } else {
            output.push_str(label);
            output.push_str(" [unsafe link omitted]");
        }
        rest = &candidate[target_start + target_end + 1..];
    }
    output.push_str(rest);
    output
}

fn safe_link_target(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

pub(super) fn sanitize(source: &str) -> String {
    let mut output = String::with_capacity(source.len().min(MAX_SOURCE_BYTES));
    for character in source.chars() {
        if output.len() >= MAX_SOURCE_BYTES {
            break;
        }
        if character == '\n' || character == '\t' {
            output.push(character);
        } else if character == '\r' {
            output.push('\n');
        } else if character.is_control() || is_bidi_control(character) {
            output.push('�');
        } else {
            output.push(character);
        }
    }
    output
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn bounded(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit.saturating_sub(3);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_matrix_is_bounded_and_semantic() {
        let document = RichDocument::plain(
            "# Title\n\n- item\n> quote\n| a | b |\n```diff\n+add\n-remove\n```",
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Heading)
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::List)
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Quote)
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Table)
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::DiffAdd)
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::DiffRemove)
        );
    }

    #[test]
    fn hostile_terminal_and_bidi_controls_become_inert_text() {
        let source = "before\u{1b}]52;c;secret\u{7}after\u{202e}txt";
        let document = RichDocument::plain(source);
        let rendered = document
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.contains('�'));
    }

    #[test]
    fn only_bounded_web_and_mail_links_survive_as_inert_metadata() {
        let document = RichDocument::plain(
            "[safe](https://example.com) [bad](file:///etc/passwd) [js](javascript:alert(1))",
        );
        assert_eq!(document.links.len(), 1);
        assert_eq!(document.links[0].target, "https://example.com");
        assert!(document.lines[0].text.contains("unsafe link omitted"));
    }
}
