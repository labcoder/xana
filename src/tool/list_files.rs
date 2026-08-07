use super::workspace_path::{WorkspacePathError, resolve_existing};
use super::{EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_LIST_ENTRIES: usize = 256;
const MAX_LIST_OUTPUT_BYTES: usize = 64 * 1024;

pub(crate) struct ListFiles;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFilesArgs {
    path: String,
}

struct ListFilesPlan {
    args: ListFilesArgs,
    requested_path: String,
    canonical_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ListEntry {
    name: String,
    kind: EntryKind,
}

#[derive(Debug)]
enum ListFilesError {
    InvalidArguments(serde_json::Error),
    Path(WorkspacePathError),
    NotDirectory {
        requested_path: String,
    },
    ReadDirectory {
        requested_path: String,
        source: io::Error,
    },
    ReadEntry {
        requested_path: String,
        source: io::Error,
    },
    NonUtf8Name {
        requested_path: String,
    },
    TooManyEntries {
        requested_path: String,
        limit: usize,
    },
    OutputTooLarge {
        requested_path: String,
        limit: usize,
    },
    Encode(serde_json::Error),
}

impl fmt::Display for ListFilesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(_) => write!(f, "list_files arguments are invalid"),
            Self::Path(error) => write!(f, "{error}"),
            Self::NotDirectory { requested_path } => {
                write!(f, "path {requested_path:?} is not a directory")
            }
            Self::ReadDirectory { requested_path, .. } | Self::ReadEntry { requested_path, .. } => {
                write!(f, "directory {requested_path:?} could not be listed")
            }
            Self::NonUtf8Name { requested_path } => write!(
                f,
                "directory {requested_path:?} contains a non-UTF-8 entry name"
            ),
            Self::TooManyEntries {
                requested_path,
                limit,
            } => write!(
                f,
                "directory {requested_path:?} contains more than {limit} entries"
            ),
            Self::OutputTooLarge {
                requested_path,
                limit,
            } => write!(
                f,
                "listing for {requested_path:?} exceeds the {limit}-byte limit"
            ),
            Self::Encode(_) => write!(f, "list_files could not encode its result"),
        }
    }
}

impl Error for ListFilesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidArguments(source) => Some(source),
            Self::Path(source) => Some(source),
            Self::ReadDirectory { source, .. } | Self::ReadEntry { source, .. } => Some(source),
            Self::Encode(source) => Some(source),
            Self::NotDirectory { .. }
            | Self::NonUtf8Name { .. }
            | Self::TooManyEntries { .. }
            | Self::OutputTooLarge { .. } => None,
        }
    }
}

fn encode_entries(
    requested_path: String,
    mut entries: Vec<ListEntry>,
) -> Result<String, ListFilesError> {
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let output = serde_json::to_string(&entries).map_err(ListFilesError::Encode)?;

    if output.len() > MAX_LIST_OUTPUT_BYTES {
        return Err(ListFilesError::OutputTooLarge {
            requested_path,
            limit: MAX_LIST_OUTPUT_BYTES,
        });
    }

    Ok(output)
}

fn plan_list_files(
    arguments: &Value,
    workspace_root: &Path,
) -> Result<ListFilesPlan, ListFilesError> {
    let args: ListFilesArgs =
        serde_json::from_value(arguments.clone()).map_err(ListFilesError::InvalidArguments)?;
    let resolved =
        resolve_existing(args.path.clone(), workspace_root).map_err(ListFilesError::Path)?;
    let requested_path = resolved.requested_path;
    let canonical_path = resolved.canonical_path;

    if !canonical_path.is_dir() {
        return Err(ListFilesError::NotDirectory { requested_path });
    }

    Ok(ListFilesPlan {
        args,
        requested_path,
        canonical_path,
    })
}

fn execute_list_files(plan: &ListFilesPlan) -> Result<String, ListFilesError> {
    let requested_path = plan.requested_path.clone();

    let directory =
        fs::read_dir(&plan.canonical_path).map_err(|source| ListFilesError::ReadDirectory {
            requested_path: requested_path.clone(),
            source,
        })?;
    let mut entries = Vec::new();

    for (index, entry) in directory.enumerate() {
        if index >= MAX_LIST_ENTRIES {
            return Err(ListFilesError::TooManyEntries {
                requested_path,
                limit: MAX_LIST_ENTRIES,
            });
        }

        let entry = entry.map_err(|source| ListFilesError::ReadEntry {
            requested_path: requested_path.clone(),
            source,
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ListFilesError::NonUtf8Name {
                requested_path: requested_path.clone(),
            })?;
        let file_type = entry
            .file_type()
            .map_err(|source| ListFilesError::ReadEntry {
                requested_path: requested_path.clone(),
                source,
            })?;
        let kind = if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        };

        entries.push(ListEntry { name, kind });
    }

    encode_entries(requested_path, entries)
}

#[cfg(test)]
fn list_files(arguments: &Value, workspace_root: &Path) -> Result<String, ListFilesError> {
    execute_list_files(&plan_list_files(arguments, workspace_root)?)
}

impl Tool for ListFiles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_files",
            description: "List one directory beneath the workspace as sorted JSON",
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory relative to the workspace root; use . for the root"
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
        let plan = plan_list_files(arguments, workspace_root).map_err(|error| error.to_string())?;
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
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<ListFilesPlan>("list_files")?;
            execute_list_files(plan).map_err(|error| error.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn lists_one_directory_as_sorted_json() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("b.txt"), "b").expect("file fixture");
        fs::create_dir(workspace.path().join("a-dir")).expect("directory fixture");

        let output = list_files(&json!({"path": "."}), workspace.path()).expect("listing");
        let entries: Vec<ListEntry> = serde_json::from_str(&output).expect("list output JSON");

        assert_eq!(
            entries,
            vec![
                ListEntry {
                    name: "a-dir".to_owned(),
                    kind: EntryKind::Directory,
                },
                ListEntry {
                    name: "b.txt".to_owned(),
                    kind: EntryKind::File,
                },
            ]
        );
    }

    #[test]
    fn rejects_unknown_arguments_before_workspace_io() {
        let parent = tempdir().expect("temporary parent");
        let missing_workspace = parent.path().join("missing");

        let result = list_files(&json!({"path": ".", "recursive": true}), &missing_workspace);

        assert!(matches!(result, Err(ListFilesError::InvalidArguments(_))));
    }

    #[test]
    fn rejects_file_when_directory_is_required() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("file.txt"), "contents").expect("fixture");

        let result = list_files(&json!({"path": "file.txt"}), workspace.path());

        assert!(matches!(
            result,
            Err(ListFilesError::NotDirectory { requested_path })
                if requested_path == "file.txt"
        ));
    }

    #[test]
    fn rejects_more_than_the_entry_limit() {
        let workspace = tempdir().expect("temporary workspace");

        for index in 0..=MAX_LIST_ENTRIES {
            fs::write(workspace.path().join(format!("entry-{index:03}.txt")), "")
                .expect("entry fixture");
        }

        let result = list_files(&json!({"path": "."}), workspace.path());

        assert!(matches!(
            result,
            Err(ListFilesError::TooManyEntries {
                requested_path,
                limit,
            }) if requested_path == "." && limit == MAX_LIST_ENTRIES
        ));
    }

    #[test]
    fn rejects_encoded_output_above_byte_limit() {
        let entries = (0..MAX_LIST_ENTRIES)
            .map(|index| ListEntry {
                name: format!("{index:03}-{}", "x".repeat(300)),
                kind: EntryKind::File,
            })
            .collect();

        let result = encode_entries(".".to_owned(), entries);

        assert!(matches!(
            result,
            Err(ListFilesError::OutputTooLarge {
                requested_path,
                limit,
            }) if requested_path == "." && limit == MAX_LIST_OUTPUT_BYTES
        ));
    }

    #[cfg(unix)]
    #[test]
    fn lists_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("temporary workspace");
        let outside = tempdir().expect("temporary outside directory");
        let target = outside.path().join("target.txt");
        fs::write(&target, "outside").expect("outside fixture");
        symlink(&target, workspace.path().join("link.txt")).expect("symlink fixture");

        let output = list_files(&json!({"path": "."}), workspace.path()).expect("listing");
        let entries: Vec<ListEntry> = serde_json::from_str(&output).expect("list output JSON");

        assert_eq!(
            entries,
            vec![ListEntry {
                name: "link.txt".to_owned(),
                kind: EntryKind::Symlink,
            }]
        );
    }

    #[test]
    fn definition_declares_read_and_safe() {
        let definition = ListFiles.definition();

        assert_eq!(definition.name, "list_files");
        assert_eq!(definition.effect_class, EffectClass::Read);
        assert_eq!(definition.replay_safety, ReplaySafety::Safe);
    }
}
