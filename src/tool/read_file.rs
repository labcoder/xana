use super::workspace_path::{WorkspacePathError, resolve_existing};
use super::{EffectClass, ReplaySafety, Tool, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::string::FromUtf8Error;

const MAX_READ_BYTES: usize = 64 * 1024;

pub(crate) struct ReadFile;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Debug)]
enum ReadFileError {
    InvalidArguments(serde_json::Error),
    Path(WorkspacePathError),
    InvalidLineRange {
        start_line: Option<usize>,
        end_line: Option<usize>,
    },
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
            Self::InvalidLineRange { .. } | Self::NotRegularFile { .. } | Self::TooLarge { .. } => {
                None
            }
        }
    }
}

fn read_file(arguments: &Value, workspace_root: &Path) -> Result<String, ReadFileError> {
    let args: ReadFileArgs =
        serde_json::from_value(arguments.clone()).map_err(ReadFileError::InvalidArguments)?;

    let start_line = args.start_line.unwrap_or(1);
    let end_line = args.end_line;

    if start_line == 0
        || end_line == Some(0)
        || end_line.is_some_and(|end_line| start_line > end_line)
    {
        return Err(ReadFileError::InvalidLineRange {
            start_line: args.start_line,
            end_line: args.end_line,
        });
    }

    let resolved = resolve_existing(args.path, workspace_root).map_err(ReadFileError::Path)?;
    let requested_path = resolved.requested_path;
    let canonical_path = resolved.canonical_path;

    let metadata = canonical_path
        .metadata()
        .map_err(|source| ReadFileError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;

    if !metadata.is_file() {
        return Err(ReadFileError::NotRegularFile { requested_path });
    }

    let file = File::open(&canonical_path).map_err(|source| ReadFileError::Unavailable {
        requested_path: requested_path.clone(),
        source,
    })?;

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

impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file",
            description: "Read a UTF-8 file beneath the workspace root using a relative path",
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "UTF-8 file path relative to the workspace root"
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
                    }
                }
            }),
            effect_class: EffectClass::Read,
            replay_safety: ReplaySafety::Safe,
        }
    }

    fn execute(&self, arguments: &Value, workspace_root: &Path) -> Result<String, String> {
        read_file(arguments, workspace_root).map_err(|error| error.to_string())
    }
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
    fn rejects_invalid_arguments_before_workspace_io() {
        let parent = tempdir().expect("temporary parent");
        let unavailable_workspace = parent.path().join("does-not-exist");

        let result = read_file(&json!({"extra": true}), &unavailable_workspace);

        assert!(matches!(result, Err(ReadFileError::InvalidArguments(_))));
    }

    #[test]
    fn rejects_blank_absolute_and_parent_paths() {
        let workspace = tempdir().expect("temporary workspace");
        let cases = [
            "   ".to_owned(),
            workspace
                .path()
                .join("absolute.txt")
                .to_string_lossy()
                .into_owned(),
            "../outside.txt".to_owned(),
        ];

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

    #[test]
    fn definition_declares_read_safe_and_line_ranges() {
        let definition = ReadFile.definition();

        assert_eq!(definition.name, "read_file");
        assert_eq!(definition.effect_class, EffectClass::Read);
        assert_eq!(definition.replay_safety, ReplaySafety::Safe);
        assert_eq!(
            definition.parameters["properties"]["start_line"]["minimum"],
            1
        );
        assert_eq!(
            definition.parameters["properties"]["end_line"]["minimum"],
            1
        );
    }
}
