//! Bounded terminal-native projection of the shared semantic content model.

use crate::{
    artifact::ArtifactRecord,
    frontend::semantic::ContentPartV1,
    resource::{AccessibilitySourceV1, ResourceKindV1, ResourceRefV1, ResourceValidationV1},
};

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
    Math,
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
    pub(super) details: Vec<String>,
}

impl ArtifactView {
    pub(super) fn from_resource(resource: &ResourceRefV1) -> Self {
        let declared = resource.media_type.declared.as_deref().unwrap_or("unknown");
        let detected = resource.media_type.detected.as_deref().unwrap_or("unknown");
        let mut details = vec![
            format!("declared {declared} · detected {detected}"),
            resource_dimensions(resource),
            validation_label(&resource.validation).to_owned(),
        ];
        if let Some(accessibility) = &resource.accessibility {
            details.push(format!(
                "accessibility {} · {}",
                accessibility_source_label(accessibility.source),
                accessibility.label.as_deref().unwrap_or("no text label")
            ));
            if let Some(transcript) = &accessibility.transcript {
                details.push(format!("transcript artifact {}", transcript.id));
            }
        } else {
            details.push("accessibility metadata unavailable".to_owned());
        }
        if let Some(lineage) = &resource.lineage {
            details.push(format!(
                "derived from {} via {} {}",
                lineage.source.id, lineage.transformer, lineage.transformer_version
            ));
        } else {
            details.push("original source (no derivative lineage)".to_owned());
        }
        details.retain(|detail| !detail.is_empty());
        for detail in &mut details {
            *detail = bounded(sanitize(detail), MAX_LINE_BYTES);
        }
        Self {
            record: resource.artifact.clone(),
            label: format!(
                "{} · {} · {} bytes",
                resource_kind_label(&resource.kind),
                detected,
                resource.artifact.byte_len
            ),
            details,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RichDocument {
    pub(super) lines: Vec<RichLine>,
    pub(super) links: Vec<SafeLink>,
    pub(super) artifacts: Vec<ArtifactView>,
    pub(super) truncated: bool,
}

impl RichDocument {
    /// Project already-normalized frontend semantics into inert terminal rows.
    ///
    /// This is deliberately a projection, not another content parser. Markdown
    /// remains the only variant interpreted by [`Self::parse`]; every other
    /// variant retains the structure established by the shared protocol.
    pub(super) fn from_parts(parts: &[ContentPartV1]) -> Self {
        let mut document = Self::empty();
        for part in parts {
            if document.lines.len() >= MAX_LINES {
                document.truncated = true;
                break;
            }
            match part {
                ContentPartV1::Text { text } => {
                    document.extend_plain(text, RichLineKind::Paragraph);
                }
                ContentPartV1::Markdown { source } => {
                    document.extend(Self::parse(source, Vec::new()));
                }
                ContentPartV1::Code { language, code } => {
                    if let Some(language) = language {
                        document.push_line(RichLineKind::Code, format!("[{language}]"));
                    }
                    document.extend_plain(code, RichLineKind::Code);
                }
                ContentPartV1::Table { columns, rows } => {
                    document.push_line(RichLineKind::Table, columns.join(" | "));
                    document.push_line(
                        RichLineKind::Table,
                        columns
                            .iter()
                            .map(|_| "---")
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                    for row in rows {
                        document.push_line(RichLineKind::Table, row.join(" | "));
                    }
                }
                ContentPartV1::Diff { patch } => {
                    for line in patch.lines() {
                        let kind = if line.starts_with('+') && !line.starts_with("+++") {
                            RichLineKind::DiffAdd
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            RichLineKind::DiffRemove
                        } else {
                            RichLineKind::Code
                        };
                        document.push_line(kind, line.to_owned());
                    }
                }
                ContentPartV1::Math { source, display } => {
                    let prefix = if *display {
                        "math: "
                    } else {
                        "math (inline): "
                    };
                    document.push_line(RichLineKind::Math, format!("{prefix}{source}"));
                }
                ContentPartV1::Link { label, url } => {
                    if document.links.len() < MAX_LINKS && safe_link_target(url) {
                        document.links.push(SafeLink {
                            label: bounded(sanitize(label), 512),
                            target: bounded(sanitize(url), 4096),
                        });
                        document.push_line(RichLineKind::Paragraph, format!("{label} [link]"));
                    } else {
                        document.push_line(
                            RichLineKind::Warning,
                            format!("{label} [unsafe link omitted]"),
                        );
                    }
                }
                ContentPartV1::Resource(resource) => document.push_resource(resource),
                ContentPartV1::Unknown { version, kind, .. } => document.push_line(
                    RichLineKind::Warning,
                    format!("Unsupported content: {kind} (schema version {version})"),
                ),
            }
        }
        document
    }

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

    fn empty() -> Self {
        Self {
            lines: Vec::new(),
            links: Vec::new(),
            artifacts: Vec::new(),
            truncated: false,
        }
    }

    fn extend(&mut self, mut other: Self) {
        append_limited(
            &mut self.lines,
            &mut other.lines,
            MAX_LINES,
            &mut self.truncated,
        );
        append_limited(
            &mut self.links,
            &mut other.links,
            MAX_LINKS,
            &mut self.truncated,
        );
        append_limited(
            &mut self.artifacts,
            &mut other.artifacts,
            MAX_ARTIFACTS,
            &mut self.truncated,
        );
        self.truncated |= other.truncated;
    }

    fn extend_plain(&mut self, source: &str, kind: RichLineKind) {
        for line in sanitize(source).lines() {
            self.push_line(kind, line.to_owned());
        }
        if source.is_empty() {
            self.push_line(kind, String::new());
        }
    }

    fn push_line(&mut self, kind: RichLineKind, text: String) {
        if self.lines.len() >= MAX_LINES {
            self.truncated = true;
            return;
        }
        self.lines.push(RichLine {
            kind,
            text: bounded(sanitize(&text), MAX_LINE_BYTES),
            emphasized: false,
            inline_code: false,
        });
    }

    fn push_resource(&mut self, resource: &ResourceRefV1) {
        if self.artifacts.len() >= MAX_ARTIFACTS {
            self.truncated = true;
            return;
        }
        self.artifacts.push(ArtifactView::from_resource(resource));
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

fn append_limited<T>(target: &mut Vec<T>, source: &mut Vec<T>, limit: usize, truncated: &mut bool) {
    let remaining = limit.saturating_sub(target.len());
    if source.len() > remaining {
        source.truncate(remaining);
        *truncated = true;
    }
    target.append(source);
}

fn resource_kind_label(kind: &ResourceKindV1) -> &str {
    match kind {
        ResourceKindV1::StaticRaster => "image",
        ResourceKindV1::AnimatedRaster => "animated image",
        ResourceKindV1::Svg => "SVG",
        ResourceKindV1::Lottie => "Lottie animation",
        ResourceKindV1::Audio => "audio",
        ResourceKindV1::Video => "video",
        ResourceKindV1::Binary => "binary attachment",
        ResourceKindV1::Unknown(_) => "unknown attachment",
    }
}

fn resource_dimensions(resource: &ResourceRefV1) -> String {
    let metadata = &resource.metadata;
    let dimensions = metadata
        .width
        .zip(metadata.height)
        .map(|(width, height)| format!("{width}x{height}"));
    let duration = metadata
        .duration_millis
        .map(|duration| format!("{duration} ms"));
    let codec = metadata
        .codec
        .as_ref()
        .map(|codec| format!("codec {codec}"));
    [dimensions, duration, codec]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn validation_label(validation: &ResourceValidationV1) -> &str {
    match validation {
        ResourceValidationV1::Pending => "validation pending",
        ResourceValidationV1::Accepted => "validated by Xana",
        ResourceValidationV1::Rejected { .. } => "rejected by resource policy",
    }
}

fn accessibility_source_label(source: AccessibilitySourceV1) -> &'static str {
    match source {
        AccessibilitySourceV1::User => "from user",
        AccessibilitySourceV1::EmbeddedMetadata => "from embedded metadata",
        AccessibilitySourceV1::Provider => "from provider",
        AccessibilitySourceV1::Derived => "derived",
        AccessibilitySourceV1::Unavailable => "unavailable",
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
    use crate::{
        artifact::{ArtifactRef, ContentHash},
        identity::{ArtifactId, PrincipalId},
        resource::{
            AccessibilityFactsV1, MediaTypeFactsV1, RESOURCE_SCHEMA_VERSION, ResourceMetadataV1,
        },
    };

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

    #[test]
    fn shared_semantic_matrix_keeps_structure_and_resource_evidence() {
        let resource = ResourceRefV1 {
            version: RESOURCE_SCHEMA_VERSION,
            artifact: ArtifactRecord {
                reference: ArtifactRef {
                    id: ArtifactId::new(),
                    content_hash: ContentHash::for_bytes(b"image"),
                },
                media_type: "image/png".into(),
                byte_len: 5,
                owner: PrincipalId::new(),
            },
            kind: ResourceKindV1::StaticRaster,
            media_type: MediaTypeFactsV1 {
                declared: Some("application/octet-stream".into()),
                detected: Some("image/png".into()),
            },
            metadata: ResourceMetadataV1 {
                width: Some(12),
                height: Some(8),
                ..ResourceMetadataV1::default()
            },
            accessibility: Some(AccessibilityFactsV1 {
                label: Some("a small test image".into()),
                transcript: None,
                source: AccessibilitySourceV1::User,
            }),
            validation: ResourceValidationV1::Accepted,
            lineage: None,
        };
        let document = RichDocument::from_parts(&[
            ContentPartV1::Text {
                text: "plain".into(),
            },
            ContentPartV1::Code {
                language: Some("rust".into()),
                code: "fn main() {}".into(),
            },
            ContentPartV1::Table {
                columns: vec!["a".into(), "b".into()],
                rows: vec![vec!["1".into(), "2".into()]],
            },
            ContentPartV1::Diff {
                patch: "+added\n-removed".into(),
            },
            ContentPartV1::Math {
                source: "x^2".into(),
                display: true,
            },
            ContentPartV1::Link {
                label: "docs".into(),
                url: "https://example.com".into(),
            },
            ContentPartV1::Resource(Box::new(resource)),
            ContentPartV1::Unknown {
                version: 2,
                kind: "future".into(),
                payload: serde_json::json!({}),
            },
        ]);

        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Code)
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
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Math)
        );
        assert_eq!(document.links.len(), 1);
        assert_eq!(document.artifacts.len(), 1);
        assert!(document.artifacts[0].details.iter().any(|detail| {
            detail.contains("declared application/octet-stream · detected image/png")
        }));
        assert!(
            document.artifacts[0]
                .details
                .iter()
                .any(|detail| detail.contains("a small test image"))
        );
        assert!(
            document
                .lines
                .iter()
                .any(|line| line.kind == RichLineKind::Warning)
        );
    }
}
