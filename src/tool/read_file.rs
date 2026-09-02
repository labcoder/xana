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
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

const MAX_READ_BYTES: usize = 64 * 1024;

pub(crate) struct ReadFile;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    offset_bytes: Option<u64>,
    max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ReadPageResult {
    path: String,
    content: String,
    offset_bytes: u64,
    next_offset_bytes: Option<u64>,
    total_bytes: u64,
    truncated: bool,
}

#[derive(Debug)]
enum ReadFileError {
    InvalidArguments(serde_json::Error),
    Path(WorkspacePathError),
    InvalidLineRange {
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
    InvalidPaging,
    Unavailable {
        requested_path: String,
        source: io::Error,
    },
    NotRegularFile {
        requested_path: String,
    },
    InvalidUtf8 {
        requested_path: String,
        source: FromUtf8Error,
    },
    TooLarge {
        requested_path: String,
        limit: usize,
    },
}

impl fmt::Display for ReadFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(_) => write!(f, "read_file arguments are invalid"),
            Self::Path(error) => write!(f, "{error}"),
            Self::InvalidLineRange {
                start_line,
                end_line,
            } => write!(
                f,
                "line range start_line={start_line:?}, end_line={end_line:?} is invalid; \
                values must be one-based and start_line must not exceed end_line"
            ),
            Self::InvalidPaging => write!(
                f,
                "byte paging requires max_bytes in 4..={MAX_READ_BYTES}, an in-range UTF-8 boundary offset, and no line range"
            ),
            Self::Unavailable { requested_path, .. } => {
                write!(f, "file {requested_path:?} is unavailable")
            }
            Self::NotRegularFile { requested_path } => {
                write!(f, "path {requested_path:?} is not a regular file")
            }
            Self::InvalidUtf8 { requested_path, .. } => {
                write!(f, "file {requested_path:?} is not valid UTF-8")
            }
            Self::TooLarge {
                requested_path,
                limit,
            } => write!(
                f,
                "selected output from file {requested_path:?} is larger than the {limit}-byte limit"
            ),
        }
    }
}

impl Error for ReadFileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidArguments(source) => Some(source),
            Self::Path(source) => Some(source),
            Self::Unavailable { source, .. } => Some(source),
            Self::InvalidUtf8 { source, .. } => Some(source),
            Self::InvalidLineRange { .. }
            | Self::InvalidPaging
            | Self::NotRegularFile { .. }
            | Self::TooLarge { .. } => None,
        }
    }
}

fn plan_read_file(arguments: &Value, workspace_root: &Path) -> Result<ReadFilePlan, ReadFileError> {
    let args: ReadFileArgs =
        serde_json::from_value(arguments.clone()).map_err(ReadFileError::InvalidArguments)?;

    let start_line = args.start_line.unwrap_or(1);
    let end_line = args.end_line;
    let paging = args.offset_bytes.is_some() || args.max_bytes.is_some();

    if start_line == 0
        || end_line == Some(0)
        || end_line.is_some_and(|end_line| start_line > end_line)
    {
        return Err(ReadFileError::InvalidLineRange {
            start_line: args.start_line,
            end_line: args.end_line,
        });
    }
    if paging
        && (args.start_line.is_some()
            || args.end_line.is_some()
            || !(4..=MAX_READ_BYTES).contains(&args.max_bytes.unwrap_or(MAX_READ_BYTES)))
    {
        return Err(ReadFileError::InvalidPaging);
    }

    let (resolved, location) = resolve_existing_for_read(args.path.clone(), workspace_root)
        .map_err(ReadFileError::Path)?;
    let requested_path = resolved.requested_path;
    let canonical_path = resolved.canonical_path;
    let identity = resolved.identity;

    let metadata = canonical_path
        .metadata()
        .map_err(|source| ReadFileError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;

    if !metadata.is_file() {
        return Err(ReadFileError::NotRegularFile { requested_path });
    }

    Ok(ReadFilePlan {
        args,
        requested_path,
        canonical_path,
        identity,
        location,
    })
}

fn execute_read_file(plan: &ReadFilePlan) -> Result<String, ReadFileError> {
    let start_line = plan.args.start_line.unwrap_or(1);
    let end_line = plan.args.end_line;
    let requested_path = plan.requested_path.clone();

    revalidate_path(&requested_path, &plan.canonical_path, &plan.identity)
        .map_err(ReadFileError::Path)?;
    let file = File::open(&plan.canonical_path).map_err(|source| ReadFileError::Unavailable {
        requested_path: requested_path.clone(),
        source,
    })?;
    verify_open_file(&requested_path, &file, &plan.identity).map_err(ReadFileError::Path)?;

    if plan.args.offset_bytes.is_some() || plan.args.max_bytes.is_some() {
        return execute_paged_read(file, plan);
    }

    let mut reader = BufReader::new(file);
    let mut selected = Vec::new();
    let mut line_number = 1_usize;

    loop {
        let mut line = Vec::new();

        let bytes_read =
            reader
                .read_until(b'\n', &mut line)
                .map_err(|source| ReadFileError::Unavailable {
                    requested_path: requested_path.clone(),
                    source,
                })?;

        if bytes_read == 0 {
            break;
        }

        let in_range =
            line_number >= start_line && end_line.is_none_or(|end_line| line_number <= end_line);

        if in_range {
            if line.len() > MAX_READ_BYTES - selected.len() {
                return Err(ReadFileError::TooLarge {
                    requested_path,
                    limit: MAX_READ_BYTES,
                });
            }

            selected.extend_from_slice(&line);
        }

        if end_line == Some(line_number) {
            break;
        }

        line_number = line_number.saturating_add(1);
    }

    String::from_utf8(selected).map_err(|source| ReadFileError::InvalidUtf8 {
        requested_path,
        source,
    })
}

fn execute_paged_read(mut file: File, plan: &ReadFilePlan) -> Result<String, ReadFileError> {
    let requested_path = plan.requested_path.clone();
    let offset = plan.args.offset_bytes.unwrap_or(0);
    let max_bytes = plan.args.max_bytes.unwrap_or(MAX_READ_BYTES);
    let total_bytes = file
        .metadata()
        .map_err(|source| ReadFileError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?
        .len();
    if offset > total_bytes {
        return Err(ReadFileError::InvalidPaging);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| ReadFileError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;
    let remaining = total_bytes.saturating_sub(offset);
    let read_limit = u64::try_from(max_bytes.saturating_add(4))
        .unwrap_or(u64::MAX)
        .min(remaining);
    let mut bytes = Vec::with_capacity(usize::try_from(read_limit).unwrap_or(max_bytes));
    (&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| ReadFileError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;
    let mut retained = bytes.len().min(max_bytes);
    let content = loop {
        match std::str::from_utf8(&bytes[..retained]) {
            Ok(text) => break text.to_owned(),
            Err(error) if error.error_len().is_none() && error.valid_up_to() < retained => {
                retained = error.valid_up_to();
            }
            Err(_) => {
                return Err(ReadFileError::InvalidUtf8 {
                    requested_path,
                    source: String::from_utf8(bytes).unwrap_err(),
                });
            }
        }
    };
    if retained == 0 && offset < total_bytes {
        return Err(ReadFileError::InvalidPaging);
    }
    let retained = u64::try_from(retained).map_err(|_| ReadFileError::InvalidPaging)?;
    let next = offset.saturating_add(retained);
    let truncated = next < total_bytes;
    serde_json::to_string(&ReadPageResult {
        path: requested_path,
        content,
        offset_bytes: offset,
        next_offset_bytes: truncated.then_some(next),
        total_bytes,
        truncated,
    })
    .map_err(ReadFileError::InvalidArguments)
}

#[cfg(test)]
fn read_file(arguments: &Value, workspace_root: &Path) -> Result<String, ReadFileError> {
    execute_read_file(&plan_read_file(arguments, workspace_root)?)
}

impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Read a UTF-8 file. Use offset_bytes/max_bytes for deterministic bounded pages of large files; line ranges retain the concise legacy text result. Workspace-relative paths use normal workspace policy and absolute external paths require exact approval.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path, or an absolute path that Xana must ask permission to read"
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to return, one-based; defaults to 1"
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line to return, one-based and inclusive; defaults to end of file"
                    },
                    "offset_bytes": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Zero-based UTF-8 byte boundary for paged mode; cannot be combined with line ranges"
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 4,
                        "maximum": MAX_READ_BYTES,
                        "description": "Maximum UTF-8 bytes returned in paged mode; result includes the next offset and truncation facts"
                    }
                }
            }),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String> {
        let plan = plan_read_file(arguments, workspace_root).map_err(|error| error.to_string())?;
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
            let plan = planned.executable::<ReadFilePlan>("read_file")?;
            execute_read_file(plan).map_err(|error| error.to_string())
        })
    }
}

struct ReadFilePlan {
    args: ReadFileArgs,
    requested_path: String,
    canonical_path: PathBuf,
    identity: FileIdentity,
    location: ResolvedPathLocation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn reads_nested_utf8_file() {
        let workspace = tempdir().expect("temporary workspace");
        let notes = workspace.path().join("notes");
        fs::create_dir_all(&notes).expect("notes directory");
        fs::write(notes.join("hello.txt"), "hello from Xana\n").expect("nested UTF-8 fixture");

        let output = read_file(&json!({"path": "notes/hello.txt"}), workspace.path())
            .expect("nested file should be readable");

        assert_eq!(output, "hello from Xana\n");
    }

    #[test]
    fn pages_large_utf8_files_with_an_exact_continuation() {
        let workspace = tempdir().expect("workspace");
        let content = format!("{}é{}", "a".repeat(64), "b".repeat(MAX_READ_BYTES));
        fs::write(workspace.path().join("large.txt"), &content).expect("large");

        let first = read_file(
            &json!({"path": "large.txt", "offset_bytes": 0, "max_bytes": 65}),
            workspace.path(),
        )
        .expect("first page");
        let first: Value = serde_json::from_str(&first).expect("page JSON");
        assert_eq!(first["content"], "a".repeat(64));
        assert_eq!(first["next_offset_bytes"], 64);
        assert_eq!(first["truncated"], true);

        let second = read_file(
            &json!({"path": "large.txt", "offset_bytes": 64, "max_bytes": 4}),
            workspace.path(),
        )
        .expect("second page");
        let second: Value = serde_json::from_str(&second).expect("page JSON");
        assert!(second["content"].as_str().unwrap().starts_with('é'));
    }

    #[test]
    fn byte_paging_rejects_line_mix_and_non_utf8_boundary() {
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("utf8.txt"), "éclair").expect("utf8");

        assert!(matches!(
            read_file(
                &json!({"path": "utf8.txt", "offset_bytes": 0, "start_line": 1}),
                workspace.path()
            ),
            Err(ReadFileError::InvalidPaging)
        ));
        assert!(matches!(
            read_file(
                &json!({"path": "utf8.txt", "offset_bytes": 1, "max_bytes": 4}),
                workspace.path()
            ),
            Err(ReadFileError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn rejects_invalid_arguments_before_workspace_io() {
        let parent = tempdir().expect("temporary parent");
        let unavailable_workspace = parent.path().join("does-not-exist");

        let result = read_file(&json!({"extra": true}), &unavailable_workspace);

        assert!(matches!(result, Err(ReadFileError::InvalidArguments(_))));
    }

    #[test]
    fn rejects_blank_and_parent_paths() {
        let workspace = tempdir().expect("temporary workspace");
        let cases = ["   ".to_owned(), "../outside.txt".to_owned()];

        for requested_path in cases {
            let result = read_file(&json!({"path": &requested_path}), workspace.path());

            assert!(matches!(
                result,
                Err(ReadFileError::Path(WorkspacePathError::InvalidPath {
                    requested_path: actual,
                })) if actual == requested_path
            ));
        }
    }

    #[test]
    fn plans_an_absolute_file_outside_the_workspace_as_an_exact_external_scope() {
        let workspace = tempdir().expect("temporary workspace");
        let outside = tempdir().expect("outside directory");
        let path = outside.path().join("notes.txt");
        fs::write(&path, "outside notes\n").expect("outside fixture");
        let tool = ReadFile;

        let planned = tool
            .plan(&json!({"path": path.to_string_lossy()}), workspace.path())
            .expect("an existing external file should reach the permission boundary");

        assert_eq!(
            planned.scope,
            PermissionScope::ExternalPath {
                canonical_path: path.canonicalize().expect("canonical outside fixture")
            }
        );
    }

    #[test]
    fn missing_file_retains_requested_path_and_source() {
        let workspace = tempdir().expect("temporary workspace");

        let result = read_file(&json!({"path": "missing.txt"}), workspace.path());

        match result {
            Err(ReadFileError::Path(WorkspacePathError::Unavailable {
                requested_path,
                source,
            })) => {
                assert_eq!(requested_path, "missing.txt");
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected unavailable file, got {other:?}"),
        }
    }

    #[test]
    fn rejects_directory() {
        let workspace = tempdir().expect("temporary workspace");
        fs::create_dir(workspace.path().join("folder")).expect("directory fixture");

        let result = read_file(&json!({"path": "folder"}), workspace.path());

        assert!(matches!(
            result,
            Err(ReadFileError::NotRegularFile { requested_path })
                if requested_path == "folder"
        ));
    }

    #[test]
    fn rejects_invalid_utf8() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("binary.dat"), [0xff_u8, 0xfe])
            .expect("invalid UTF-8 fixture");

        let result = read_file(&json!({"path": "binary.dat"}), workspace.path());

        assert!(matches!(
            result,
            Err(ReadFileError::InvalidUtf8 {
                requested_path,
                ..
            }) if requested_path == "binary.dat"
        ));
    }

    #[test]
    fn rejects_file_above_read_limit() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(
            workspace.path().join("large.txt"),
            vec![b'x'; MAX_READ_BYTES + 1],
        )
        .expect("oversized fixture");

        let result = read_file(&json!({"path": "large.txt"}), workspace.path());

        assert!(matches!(
            result,
            Err(ReadFileError::TooLarge {
                requested_path,
                limit,
            }) if requested_path == "large.txt" && limit == MAX_READ_BYTES
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_that_resolves_outside_workspace() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("temporary workspace");
        let outside = tempdir().expect("temporary outside directory");
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "outside contents").expect("outside file fixture");
        symlink(&outside_file, workspace.path().join("escape.txt"))
            .expect("escape symlink fixture");

        let result = read_file(&json!({"path": "escape.txt"}), workspace.path());

        assert!(matches!(
            result,
            Err(ReadFileError::Path(WorkspacePathError::OutsideWorkspace { requested_path }))
                if requested_path == "escape.txt"
        ));
    }

    #[test]
    fn reads_inclusive_one_based_line_range() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(
            workspace.path().join("lines.txt"),
            "one\ntwo\nthree\nfour\n",
        )
        .expect("line fixture");

        let output = read_file(
            &json!({
                "path": "lines.txt",
                "start_line": 2,
                "end_line": 3
            }),
            workspace.path(),
        )
        .expect("valid line range");

        assert_eq!(output, "two\nthree\n");
    }

    #[test]
    fn supports_open_ended_line_ranges() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("lines.txt"), "one\ntwo\nthree\n").expect("line fixture");

        let from_second = read_file(
            &json!({"path": "lines.txt", "start_line": 2}),
            workspace.path(),
        )
        .expect("start-only range");

        let through_second = read_file(
            &json!({"path": "lines.txt", "end_line": 2}),
            workspace.path(),
        )
        .expect("end-only range");

        assert_eq!(from_second, "two\nthree\n");
        assert_eq!(through_second, "one\ntwo\n");
    }

    #[test]
    fn rejects_zero_and_reversed_line_ranges() {
        let workspace = tempdir().expect("temporary workspace");
        let cases = [
            (
                json!({"path": "unused.txt", "start_line": 0}),
                Some(0),
                None,
            ),
            (json!({"path": "unused.txt", "end_line": 0}), None, Some(0)),
            (
                json!({
                    "path": "unused.txt",
                    "start_line": 3,
                    "end_line": 2
                }),
                Some(3),
                Some(2),
            ),
        ];

        for (arguments, expected_start, expected_end) in cases {
            let result = read_file(&arguments, workspace.path());

            assert!(matches!(
                result,
                Err(ReadFileError::InvalidLineRange {
                    start_line,
                    end_line,
                }) if start_line == expected_start && end_line == expected_end
            ));
        }
    }

    #[test]
    fn applies_byte_limit_to_selected_output() {
        let workspace = tempdir().expect("temporary workspace");
        let mut contents = vec![b'x'; MAX_READ_BYTES + 1];
        contents.extend_from_slice(b"\nselected\n");

        fs::write(workspace.path().join("large.txt"), contents).expect("large fixture");

        let output = read_file(
            &json!({
                "path": "large.txt",
                "start_line": 2,
                "end_line": 2
            }),
            workspace.path(),
        )
        .expect("small selected output from large file");

        assert_eq!(output, "selected\n");
    }
}
