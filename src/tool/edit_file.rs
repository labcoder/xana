use super::workspace_path::{
    FileIdentity, WorkspacePathError, resolve_existing, revalidate_path, verify_open_file,
};
use super::{EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::error::Error;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

const MAX_EDIT_BYTES: usize = 64 * 1024;

pub(crate) struct EditFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    path: String,
    old_text: String,
    new_text: String,
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
    MatchCount {
        requested_path: String,
        actual: usize,
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
            Self::MatchCount {
                requested_path,
                actual,
            } => write!(
                f,
                "old_text matched {actual} times in {requested_path:?}; expected exactly once"
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
            | Self::MatchCount { .. } => None,
        }
    }
}

fn plan_edit_file(arguments: &Value, workspace_root: &Path) -> Result<EditFilePlan, EditFileError> {
    let args: EditFileArgs =
        serde_json::from_value(arguments.clone()).map_err(EditFileError::InvalidArguments)?;
    let requested_path = args.path.clone();

    if args.old_text.is_empty() {
        return Err(EditFileError::EmptyOldText { requested_path });
    }

    if args.new_text.len() > MAX_EDIT_BYTES {
        return Err(EditFileError::TooLarge {
            requested_path,
            limit: MAX_EDIT_BYTES,
        });
    }

    let resolved =
        resolve_existing(args.path.clone(), workspace_root).map_err(EditFileError::Path)?;
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

    Ok(EditFilePlan {
        args,
        requested_path,
        canonical_path,
        identity,
    })
}

fn execute_edit_file(plan: &EditFilePlan) -> Result<String, EditFileError> {
    let args = &plan.args;
    let requested_path = plan.requested_path.clone();

    revalidate_path(&requested_path, &plan.canonical_path, &plan.identity)
        .map_err(EditFileError::Path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&plan.canonical_path)
        .map_err(|source| EditFileError::Read {
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
    let match_count = contents.match_indices(&args.old_text).count();

    if match_count != 1 {
        return Err(EditFileError::MatchCount {
            requested_path,
            actual: match_count,
        });
    }

    let updated = contents.replacen(&args.old_text, &args.new_text, 1);

    if updated.len() > MAX_EDIT_BYTES {
        return Err(EditFileError::TooLarge {
            requested_path,
            limit: MAX_EDIT_BYTES,
        });
    }

    revalidate_path(&requested_path, &plan.canonical_path, &plan.identity)
        .map_err(EditFileError::Path)?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| file.write_all(updated.as_bytes()))
        .and_then(|_| file.flush())
        .map_err(|source| EditFileError::Write {
            requested_path: requested_path.clone(),
            source,
        })?;

    Ok(format!("edited {requested_path:?}"))
}

#[cfg(test)]
fn edit_file(arguments: &Value, workspace_root: &Path) -> Result<String, EditFileError> {
    execute_edit_file(&plan_edit_file(arguments, workspace_root)?)
}

impl Tool for EditFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file",
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Replace exactly one text occurrence in an existing UTF-8 workspace file",
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "old_text", "new_text"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "UTF-8 file path relative to the workspace root"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Non-empty text that must occur exactly once"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text"
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
        let scope = PermissionScope::WorkspacePath {
            canonical_path: plan.canonical_path.clone(),
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
    requested_path: String,
    canonical_path: PathBuf,
    identity: FileIdentity,
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
    fn definition_declares_write_and_never() {
        let definition = EditFile.definition();

        assert_eq!(definition.name, "edit_file");
        assert_eq!(definition.effect_class, EffectClass::Write);
        assert_eq!(definition.replay_safety, ReplaySafety::Never);
    }
}
