use super::workspace_path::{
    FileIdentity, ResolvedPathLocation, WorkspacePathError, resolve_existing_for_read,
    revalidate_path, verify_open_file,
};
use super::{EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;
use std::time::SystemTime;

const MAX_EDIT_BYTES: usize = 64 * 1024;
const MAX_EDITS: usize = 32;
const MAX_EXPECTED_OCCURRENCES: usize = 1_000;

pub(crate) struct EditFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edits: Option<Vec<EditSpec>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditSpec {
    old_text: String,
    new_text: String,
    #[serde(default = "one_occurrence")]
    expected_occurrences: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    occurrence: Option<usize>,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug)]
enum EditFileError {
    InvalidArguments(serde_json::Error),
    Path(WorkspacePathError),
    NotRegularFile {
        requested_path: String,
    },
    TooLarge {
        requested_path: String,
        limit: usize,
    },
    Read {
        requested_path: String,
        source: io::Error,
    },
    InvalidUtf8 {
        requested_path: String,
        source: FromUtf8Error,
    },
    EmptyOldText {
        requested_path: String,
    },
    InvalidEditSet {
        requested_path: String,
        reason: &'static str,
    },
    MatchCount {
        requested_path: String,
        expected: usize,
        actual: usize,
    },
    OverlappingEdits {
        requested_path: String,
    },
    ChangedContents {
        requested_path: String,
    },
    Write {
        requested_path: String,
        source: io::Error,
    },
}

impl fmt::Display for EditFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(_) => write!(f, "edit_file arguments are invalid"),
            Self::Path(error) => write!(f, "{error}"),
            Self::NotRegularFile { requested_path } => {
                write!(f, "path {requested_path:?} is not a regular file")
            }
            Self::TooLarge {
                requested_path,
                limit,
            } => write!(
                f,
                "edit for {requested_path:?} exceeds the {limit}-byte limit"
            ),
            Self::Read { requested_path, .. } => {
                write!(f, "file {requested_path:?} could not be read")
            }
            Self::InvalidUtf8 { requested_path, .. } => {
                write!(f, "file {requested_path:?} is not valid UTF-8")
            }
            Self::EmptyOldText { requested_path } => {
                write!(f, "old_text for {requested_path:?} must not be empty")
            }
            Self::InvalidEditSet {
                requested_path,
                reason,
            } => write!(f, "edit set for {requested_path:?} is invalid: {reason}"),
            Self::MatchCount {
                requested_path,
                expected,
                actual,
            } => write!(
                f,
                "old_text matched {actual} times in {requested_path:?}; expected {expected}"
            ),
            Self::OverlappingEdits { requested_path } => write!(
                f,
                "edit spans for {requested_path:?} overlap in the original file"
            ),
            Self::ChangedContents { requested_path } => write!(
                f,
                "file {requested_path:?} changed after permission planning"
            ),
            Self::Write { requested_path, .. } => {
                write!(f, "file {requested_path:?} could not be written")
            }
        }
    }
}

impl Error for EditFileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidArguments(source) => Some(source),
            Self::Path(source) => Some(source),
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::InvalidUtf8 { source, .. } => Some(source),
            Self::NotRegularFile { .. }
            | Self::TooLarge { .. }
            | Self::EmptyOldText { .. }
            | Self::InvalidEditSet { .. }
            | Self::MatchCount { .. }
            | Self::OverlappingEdits { .. }
            | Self::ChangedContents { .. } => None,
        }
    }
}

fn one_occurrence() -> usize {
    1
}

fn plan_edit_file(arguments: &Value, workspace_root: &Path) -> Result<EditFilePlan, EditFileError> {
    let args: EditFileArgs =
        serde_json::from_value(arguments.clone()).map_err(EditFileError::InvalidArguments)?;
    let requested_path = args.path.clone();

    let edits = normalize_edits(&args, &requested_path)?;
    let resolved = resolve_existing_for_read(args.path.clone(), workspace_root)
        .map_err(EditFileError::Path)?;
    let (resolved, location) = resolved;
    let requested_path = resolved.requested_path;
    let canonical_path = resolved.canonical_path;
    let identity = resolved.identity;
    let metadata = canonical_path
        .metadata()
        .map_err(|source| EditFileError::Read {
            requested_path: requested_path.clone(),
            source,
        })?;

    if !metadata.is_file() {
        return Err(EditFileError::NotRegularFile { requested_path });
    }

    if metadata.len() > MAX_EDIT_BYTES as u64 {
        return Err(EditFileError::TooLarge {
            requested_path,
            limit: MAX_EDIT_BYTES,
        });
    }
    let stamp = FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    };

    Ok(EditFilePlan {
        args,
        edits,
        requested_path,
        canonical_path,
        identity,
        location,
        stamp,
    })
}

fn normalize_edits(
    args: &EditFileArgs,
    requested_path: &str,
) -> Result<Vec<EditSpec>, EditFileError> {
    let edits = match (&args.edits, &args.old_text, &args.new_text) {
        (Some(edits), None, None) => edits.clone(),
        (None, Some(old_text), Some(new_text)) => vec![EditSpec {
            old_text: old_text.clone(),
            new_text: new_text.clone(),
            expected_occurrences: 1,
            occurrence: None,
            replace_all: false,
        }],
        _ => {
            return Err(EditFileError::InvalidEditSet {
                requested_path: requested_path.to_owned(),
                reason: "provide old_text/new_text or edits, but not both",
            });
        }
    };
    if edits.is_empty() || edits.len() > MAX_EDITS {
        return Err(EditFileError::InvalidEditSet {
            requested_path: requested_path.to_owned(),
            reason: "edits must contain between 1 and 32 replacements",
        });
    }
    let mut replacement_bytes = 0_usize;
    for edit in &edits {
        if edit.old_text.is_empty() {
            return Err(EditFileError::EmptyOldText {
                requested_path: requested_path.to_owned(),
            });
        }
        if edit.expected_occurrences == 0
            || edit.expected_occurrences > MAX_EXPECTED_OCCURRENCES
            || edit.occurrence == Some(0)
            || edit
                .occurrence
                .is_some_and(|occurrence| occurrence > edit.expected_occurrences)
            || (edit.replace_all && edit.occurrence.is_some())
            || (!edit.replace_all && edit.expected_occurrences > 1 && edit.occurrence.is_none())
        {
            return Err(EditFileError::InvalidEditSet {
                requested_path: requested_path.to_owned(),
                reason: "expected_occurrences and occurrence/replace_all controls conflict",
            });
        }
        replacement_bytes = replacement_bytes.saturating_add(edit.new_text.len());
    }
    if replacement_bytes > MAX_EDIT_BYTES {
        return Err(EditFileError::TooLarge {
            requested_path: requested_path.to_owned(),
            limit: MAX_EDIT_BYTES,
        });
    }
    Ok(edits)
}

fn execute_edit_file(plan: &EditFilePlan) -> Result<String, EditFileError> {
    let requested_path = plan.requested_path.clone();

    revalidate_path(&requested_path, &plan.canonical_path, &plan.identity)
        .map_err(EditFileError::Path)?;
    let metadata = plan
        .canonical_path
        .metadata()
        .map_err(|source| EditFileError::Read {
            requested_path: requested_path.clone(),
            source,
        })?;
    if FileStamp::from_metadata(&metadata) != plan.stamp {
        return Err(EditFileError::ChangedContents { requested_path });
    }
    let mut file = File::open(&plan.canonical_path).map_err(|source| EditFileError::Read {
        requested_path: requested_path.clone(),
        source,
    })?;
    verify_open_file(&requested_path, &file, &plan.identity).map_err(EditFileError::Path)?;
    let mut bytes = Vec::with_capacity(MAX_EDIT_BYTES + 1);
    (&mut file)
        .take(MAX_EDIT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| EditFileError::Read {
            requested_path: requested_path.clone(),
            source,
        })?;

    if bytes.len() > MAX_EDIT_BYTES {
        return Err(EditFileError::TooLarge {
            requested_path,
            limit: MAX_EDIT_BYTES,
        });
    }

    let contents = String::from_utf8(bytes).map_err(|source| EditFileError::InvalidUtf8 {
        requested_path: requested_path.clone(),
        source,
    })?;
    let (updated, replacement_count) = apply_edits(&contents, &plan.edits, &requested_path)?;

    if updated.len() > MAX_EDIT_BYTES {
        return Err(EditFileError::TooLarge {
            requested_path,
            limit: MAX_EDIT_BYTES,
        });
    }

    revalidate_path(&requested_path, &plan.canonical_path, &plan.identity)
        .map_err(EditFileError::Path)?;
    let current = plan
        .canonical_path
        .metadata()
        .map_err(|source| EditFileError::Read {
            requested_path: requested_path.clone(),
            source,
        })?;
    if FileStamp::from_metadata(&current) != plan.stamp {
        return Err(EditFileError::ChangedContents { requested_path });
    }
    let permissions = current.permissions();
    let mut staged =
        atomic_write_file::AtomicWriteFile::open(&plan.canonical_path).map_err(|source| {
            EditFileError::Write {
                requested_path: requested_path.clone(),
                source: io::Error::other(source),
            }
        })?;
    staged
        .as_file()
        .set_permissions(permissions)
        .and_then(|()| staged.write_all(updated.as_bytes()))
        .map_err(|source| EditFileError::Write {
            requested_path: requested_path.clone(),
            source,
        })?;
    staged.commit().map_err(|source| EditFileError::Write {
        requested_path: requested_path.clone(),
        source: io::Error::other(source),
    })?;

    if replacement_count == 1 {
        Ok(format!("edited {requested_path:?}"))
    } else {
        Ok(format!(
            "edited {requested_path:?} with {replacement_count} exact replacements"
        ))
    }
}

fn apply_edits(
    contents: &str,
    edits: &[EditSpec],
    requested_path: &str,
) -> Result<(String, usize), EditFileError> {
    struct Span<'a> {
        start: usize,
        end: usize,
        replacement: &'a str,
    }
    let mut spans = Vec::new();
    for edit in edits {
        let occurrences = contents
            .match_indices(&edit.old_text)
            .map(|(start, matched)| (start, start + matched.len()))
            .collect::<Vec<_>>();
        if occurrences.len() != edit.expected_occurrences {
            return Err(EditFileError::MatchCount {
                requested_path: requested_path.to_owned(),
                expected: edit.expected_occurrences,
                actual: occurrences.len(),
            });
        }
        if edit.replace_all {
            spans.extend(occurrences.into_iter().map(|(start, end)| Span {
                start,
                end,
                replacement: &edit.new_text,
            }));
        } else {
            let index = edit.occurrence.unwrap_or(1) - 1;
            let (start, end) = occurrences[index];
            spans.push(Span {
                start,
                end,
                replacement: &edit.new_text,
            });
        }
    }
    spans.sort_by_key(|span| (span.start, span.end));
    if spans.windows(2).any(|pair| pair[1].start < pair[0].end) {
        return Err(EditFileError::OverlappingEdits {
            requested_path: requested_path.to_owned(),
        });
    }
    let mut updated = String::with_capacity(contents.len());
    let mut cursor = 0_usize;
    for span in &spans {
        updated.push_str(&contents[cursor..span.start]);
        updated.push_str(span.replacement);
        cursor = span.end;
    }
    updated.push_str(&contents[cursor..]);
    Ok((updated, spans.len()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

#[cfg(test)]
fn edit_file(arguments: &Value, workspace_root: &Path) -> Result<String, EditFileError> {
    execute_edit_file(&plan_edit_file(arguments, workspace_root)?)
}

impl Tool for EditFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Atomically apply one or more exact, non-overlapping replacements against the original UTF-8 file. Use expected occurrence controls; search and read first, then verify the result.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "oneOf": [
                    {
                        "required": ["old_text", "new_text"],
                        "not": {"required": ["edits"]}
                    },
                    {
                        "required": ["edits"],
                        "not": {
                            "anyOf": [
                                {"required": ["old_text"]},
                                {"required": ["new_text"]}
                            ]
                        }
                    }
                ],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative UTF-8 file, or an absolute external file requiring exact approval"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Legacy single-edit form: non-empty text that must occur exactly once"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Legacy single-edit replacement text"
                    },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_EDITS,
                        "description": "Atomic exact replacements evaluated against the original file; cannot be combined with old_text/new_text",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["old_text", "new_text"],
                            "properties": {
                                "old_text": {"type": "string", "minLength": 1},
                                "new_text": {"type": "string"},
                                "expected_occurrences": {"type": "integer", "minimum": 1, "maximum": MAX_EXPECTED_OCCURRENCES, "default": 1},
                                "occurrence": {"type": "integer", "minimum": 1, "description": "Replace this one-based occurrence after confirming expected_occurrences"},
                                "replace_all": {"type": "boolean", "default": false, "description": "Replace every confirmed occurrence; cannot be combined with occurrence"}
                            }
                        }
                    }
                }
            }),
            effect_class: EffectClass::Write,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String> {
        let plan = plan_edit_file(arguments, workspace_root).map_err(|error| error.to_string())?;
        let final_arguments =
            serde_json::to_value(&plan.args).map_err(|error| error.to_string())?;
        let scope = match plan.location {
            ResolvedPathLocation::Workspace => PermissionScope::WorkspacePath {
                canonical_path: plan.canonical_path.clone(),
            },
            ResolvedPathLocation::External => PermissionScope::ExternalPath {
                canonical_path: plan.canonical_path.clone(),
            },
        };
        Ok(PlannedToolInvocation::new(final_arguments, scope, plan))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: super::ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<EditFilePlan>("edit_file")?;
            execute_edit_file(plan).map_err(|error| error.to_string())
        })
    }
}

struct EditFilePlan {
    args: EditFileArgs,
    edits: Vec<EditSpec>,
    requested_path: String,
    canonical_path: PathBuf,
    identity: FileIdentity,
    location: ResolvedPathLocation,
    stamp: FileStamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn fixture(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let workspace = tempdir().expect("temporary workspace");
        let path = workspace.path().join("state.txt");
        fs::write(&path, contents).expect("file fixture");
        (workspace, path)
    }

    #[test]
    fn replaces_exactly_one_match_and_preserves_other_content() {
        let (workspace, path) = fixture(b"before\nstatus=rough\nafter\n");

        let output = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "status=rough",
                "new_text": "status=ready"
            }),
            workspace.path(),
        )
        .expect("single match should be edited");

        assert_eq!(output, "edited \"state.txt\"");
        assert_eq!(
            fs::read_to_string(path).expect("edited fixture"),
            "before\nstatus=ready\nafter\n"
        );
    }

    #[test]
    fn rejects_unknown_arguments_before_workspace_io() {
        let parent = tempdir().expect("temporary parent");
        let missing_workspace = parent.path().join("missing");

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "old",
                "new_text": "new",
                "create": true
            }),
            &missing_workspace,
        );

        assert!(matches!(result, Err(EditFileError::InvalidArguments(_))));
    }

    #[test]
    fn rejects_empty_old_text_without_mutating_file() {
        let original = b"status=rough\n";
        let (workspace, path) = fixture(original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "",
                "new_text": "status=ready"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::EmptyOldText { requested_path })
                if requested_path == "state.txt"
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_zero_matches_without_mutating_file() {
        let original = b"status=rough\n";
        let (workspace, path) = fixture(original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "status=missing",
                "new_text": "status=ready"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::MatchCount {
                requested_path,
                expected: 1,
                actual: 0,
            }) if requested_path == "state.txt"
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_multiple_matches_without_mutating_file() {
        let original = b"same\nmiddle\nsame\n";
        let (workspace, path) = fixture(original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "same",
                "new_text": "changed"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::MatchCount {
                requested_path,
                expected: 1,
                actual: 2,
            }) if requested_path == "state.txt"
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_directory_without_mutating_any_file() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir(workspace.path().join("folder")).expect("directory fixture");

        let result = edit_file(
            &json!({
                "path": "folder",
                "old_text": "old",
                "new_text": "new"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::NotRegularFile { requested_path })
                if requested_path == "folder"
        ));
    }

    #[test]
    fn rejects_invalid_utf8_without_mutating_file() {
        let original = [0xff_u8, 0xfe];
        let (workspace, path) = fixture(&original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "old",
                "new_text": "new"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::InvalidUtf8 { requested_path, .. })
                if requested_path == "state.txt"
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_oversized_input_without_mutating_file() {
        let original = vec![b'x'; MAX_EDIT_BYTES + 1];
        let (workspace, path) = fixture(&original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "x",
                "new_text": "y"
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::TooLarge {
                requested_path,
                limit,
            }) if requested_path == "state.txt" && limit == MAX_EDIT_BYTES
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_oversized_result_without_mutating_file() {
        let original = b"prefix TARGET suffix";
        let (workspace, path) = fixture(original);

        let result = edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "TARGET",
                "new_text": "x".repeat(MAX_EDIT_BYTES)
            }),
            workspace.path(),
        );

        assert!(matches!(
            result,
            Err(EditFileError::TooLarge {
                requested_path,
                limit,
            }) if requested_path == "state.txt" && limit == MAX_EDIT_BYTES
        ));
        assert_eq!(fs::read(path).expect("unchanged fixture"), original);
    }

    #[test]
    fn rejects_file_replacement_after_permission_planning() {
        let (workspace, path) = fixture(b"status=rough\n");
        let plan = plan_edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "status=rough",
                "new_text": "status=ready"
            }),
            workspace.path(),
        )
        .expect("edit plan");
        fs::remove_file(&path).expect("remove planned file");
        fs::write(&path, b"replacement with different identity\n").expect("replacement file");

        assert!(matches!(
            execute_edit_file(&plan),
            Err(EditFileError::Path(
                WorkspacePathError::ChangedSincePlanning { .. }
            ))
        ));
        assert_eq!(
            fs::read(&path).expect("replacement remains unchanged"),
            b"replacement with different identity\n"
        );
    }

    #[test]
    fn applies_multiple_non_overlapping_edits_against_original_bytes_atomically() {
        let (workspace, path) = fixture(b"alpha beta alpha gamma\n");
        let output = edit_file(
            &json!({
                "path": "state.txt",
                "edits": [
                    {"old_text": "alpha", "new_text": "A", "expected_occurrences": 2, "replace_all": true},
                    {"old_text": "gamma", "new_text": "G"}
                ]
            }),
            workspace.path(),
        )
        .expect("multi-edit");

        assert!(output.contains("3 exact replacements"));
        assert_eq!(fs::read_to_string(path).unwrap(), "A beta A G\n");
    }

    #[test]
    fn occurrence_selection_and_overlap_fail_without_partial_mutation() {
        let original = b"one two one\n";
        let (workspace, path) = fixture(original);
        edit_file(
            &json!({
                "path": "state.txt",
                "edits": [{"old_text": "one", "new_text": "ONE", "expected_occurrences": 2, "occurrence": 2}]
            }),
            workspace.path(),
        )
        .expect("second occurrence");
        assert_eq!(fs::read_to_string(&path).unwrap(), "one two ONE\n");

        fs::write(&path, original).expect("restore");
        let result = edit_file(
            &json!({
                "path": "state.txt",
                "edits": [
                    {"old_text": "one two", "new_text": "x"},
                    {"old_text": "two one", "new_text": "y"}
                ]
            }),
            workspace.path(),
        );
        assert!(matches!(
            result,
            Err(EditFileError::OverlappingEdits { .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn in_place_change_after_permission_planning_is_rejected() {
        let (workspace, path) = fixture(b"status=rough\n");
        let plan = plan_edit_file(
            &json!({
                "path": "state.txt",
                "old_text": "status=rough",
                "new_text": "status=ready"
            }),
            workspace.path(),
        )
        .expect("plan");
        fs::write(&path, b"status=changed and longer\n").expect("concurrent write");

        assert!(matches!(
            execute_edit_file(&plan),
            Err(EditFileError::ChangedContents { .. })
        ));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "status=changed and longer\n"
        );
    }

    #[test]
    fn absolute_external_edit_uses_exact_external_permission_scope() {
        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside");
        let path = outside.path().join("outside.txt");
        fs::write(&path, "old").expect("outside fixture");
        let planned = EditFile
            .plan(
                &json!({
                    "path": path.to_string_lossy(),
                    "old_text": "old",
                    "new_text": "new"
                }),
                workspace.path(),
            )
            .expect("external plan");

        assert!(matches!(
            planned.scope,
            PermissionScope::ExternalPath { .. }
        ));
    }
}
