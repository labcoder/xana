//! Focused, bounded document extraction outside the headless agent loop.
//!
//! The extractor accepts bytes rather than paths. The runtime owns the one
//! checked open/read, workspace policy, artifact persistence, and provenance;
//! this service only interprets already-bounded content.

use std::{error::Error, fmt};

#[derive(Debug, Clone)]
pub struct DocumentInput {
    pub source_name: Option<String>,
    pub bytes: Vec<u8>,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAsset {
    pub name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentWarning {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedDocument {
    pub markdown: String,
    pub media_type: String,
    pub assets: Vec<ExtractedAsset>,
    pub warnings: Vec<DocumentWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentErrorKind {
    Unsupported,
    Malformed,
    Encrypted,
    InputLimit,
    WorkLimit,
    OutputLimit,
    Extractor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentError {
    pub kind: DocumentErrorKind,
    pub message: String,
}

impl DocumentError {
    fn new(kind: DocumentErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}
impl Error for DocumentError {}
impl fmt::Display for DocumentErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "unsupported",
            Self::Malformed => "malformed",
            Self::Encrypted => "encrypted",
            Self::InputLimit => "input limit",
            Self::WorkLimit => "work limit",
            Self::OutputLimit => "output limit",
            Self::Extractor => "extractor",
        })
    }
}

pub trait DocumentExtractor: Send + Sync {
    fn extract(&self, input: DocumentInput) -> Result<ExtractedDocument, DocumentError>;
}

#[derive(Debug, Clone, Copy)]
pub struct BuiltinDocumentExtractor {
    pub max_input_bytes: usize,
    pub max_cells: usize,
}

impl Default for BuiltinDocumentExtractor {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_cells: 100_000,
        }
    }
}

impl DocumentExtractor for BuiltinDocumentExtractor {
    fn extract(&self, input: DocumentInput) -> Result<ExtractedDocument, DocumentError> {
        if input.bytes.len() > self.max_input_bytes {
            return Err(DocumentError::new(
                DocumentErrorKind::InputLimit,
                "document exceeds the input byte limit",
            ));
        }
        let name = input.source_name.as_deref().unwrap_or("document");
        let extension = name
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        #[cfg(feature = "documents-csv")]
        if extension == "csv" {
            return extract_csv(input, self.max_cells);
        }
        if matches!(extension.as_str(), "docx" | "xlsx" | "xls" | "pdf" | "odt") {
            return Err(DocumentError::new(
                DocumentErrorKind::Unsupported,
                format!("{extension} extraction is not enabled in this build"),
            ));
        }
        let text = std::str::from_utf8(&input.bytes).map_err(|_| {
            DocumentError::new(DocumentErrorKind::Malformed, "document is not valid UTF-8")
        })?;
        bounded_output(text.to_owned(), "text/plain", input.max_output_bytes)
    }
}

fn bounded_output(
    markdown: String,
    media_type: &str,
    max_output_bytes: usize,
) -> Result<ExtractedDocument, DocumentError> {
    if max_output_bytes == 0 {
        return Err(DocumentError::new(
            DocumentErrorKind::OutputLimit,
            "output limit must be positive",
        ));
    }
    if markdown.len() > max_output_bytes {
        return Err(DocumentError::new(
            DocumentErrorKind::OutputLimit,
            "extracted document exceeds the output byte limit",
        ));
    }
    Ok(ExtractedDocument {
        markdown,
        media_type: media_type.to_owned(),
        assets: Vec::new(),
        warnings: Vec::new(),
    })
}

#[cfg(feature = "documents-csv")]
fn extract_csv(input: DocumentInput, max_cells: usize) -> Result<ExtractedDocument, DocumentError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(input.bytes.as_slice());
    let mut rows = Vec::new();
    let mut cells = 0usize;
    for record in reader.records() {
        let record = record
            .map_err(|error| DocumentError::new(DocumentErrorKind::Malformed, error.to_string()))?;
        cells = cells.saturating_add(record.len());
        if cells > max_cells {
            return Err(DocumentError::new(
                DocumentErrorKind::WorkLimit,
                "CSV cell limit exceeded",
            ));
        }
        rows.push(record.iter().map(escape_cell).collect::<Vec<_>>());
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return bounded_output(String::new(), "text/csv", input.max_output_bytes);
    }
    let mut markdown = String::new();
    markdown.push('|');
    for cell in rows.first().into_iter().flatten() {
        markdown.push_str(&format!(" {cell} |"));
    }
    markdown.push('\n');
    markdown.push('|');
    for _ in 0..rows.first().map_or(width, Vec::len) {
        markdown.push_str(" --- |");
    }
    markdown.push('\n');
    for row in rows.iter().skip(1) {
        markdown.push('|');
        for cell in row {
            markdown.push_str(&format!(" {cell} |"));
        }
        markdown.push('\n');
    }
    bounded_output(markdown, "text/csv", input.max_output_bytes)
}

#[cfg(feature = "documents-csv")]
fn escape_cell(cell: &str) -> String {
    cell.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_extraction_is_content_first() {
        let result = BuiltinDocumentExtractor::default()
            .extract(DocumentInput {
                source_name: Some("notes.weird".into()),
                bytes: b"# Hello".to_vec(),
                max_output_bytes: 100,
            })
            .unwrap();
        assert_eq!(result.markdown, "# Hello");
        assert_eq!(result.media_type, "text/plain");
    }

    #[cfg(feature = "documents-csv")]
    #[test]
    fn csv_is_rendered_as_bounded_markdown() {
        let result = BuiltinDocumentExtractor::default()
            .extract(DocumentInput {
                source_name: Some("rows.csv".into()),
                bytes: b"name,value\na,1\n".to_vec(),
                max_output_bytes: 100,
            })
            .unwrap();
        assert!(result.markdown.contains("| name | value |"));
    }

    #[test]
    fn output_limit_is_typed() {
        let error = BuiltinDocumentExtractor::default()
            .extract(DocumentInput {
                source_name: Some("notes.txt".into()),
                bytes: b"hello".to_vec(),
                max_output_bytes: 3,
            })
            .unwrap_err();
        assert_eq!(error.kind, DocumentErrorKind::OutputLimit);
    }
}
