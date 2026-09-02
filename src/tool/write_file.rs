//! Explicit bounded file creation and atomic replacement.

use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, workspace_path,
};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

const MAX_WRITE_BYTES: usize = 256 * 1024;

pub(crate) struct WriteFile;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WriteMode {
    Create,
    Overwrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    path: String,
    content: String,
    mode: WriteMode,
}

struct WriteFilePlan {
    args: WriteFileArgs,
    target: workspace_path::ResolvedWritePath,
}

#[derive(Debug, Serialize)]
struct WriteFileResult<'a> {
    path: &'a str,
    mode: WriteMode,
    bytes_written: usize,
    atomic_replacement: bool,
}

fn plan_write_file(arguments: &Value, workspace_root: &Path) -> Result<WriteFilePlan, String> {
    let args: WriteFileArgs = serde_json::from_value(arguments.clone())
        .map_err(|_| "write_file arguments are invalid".to_owned())?;
    if args.content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "write_file content exceeds the {MAX_WRITE_BYTES}-byte limit"
        ));
    }
    let target = workspace_path::resolve_for_write(args.path.clone(), workspace_root)
        .map_err(|error| error.to_string())?;
    match (args.mode, target.existing_identity.is_some()) {
        (WriteMode::Create, true) => {
            return Err(format!(
                "write_file create requires an absent path; {:?} already exists",
                args.path
            ));
        }
        (WriteMode::Overwrite, false) => {
            return Err(format!(
                "write_file overwrite requires an existing file; {:?} is absent",
                args.path
            ));
        }
        (WriteMode::Create, false) => {}
        (WriteMode::Overwrite, true) => {
            let metadata = fs::metadata(&target.canonical_path)
                .map_err(|_| format!("write_file target {:?} is unavailable", args.path))?;
            if !metadata.is_file() {
                return Err(format!(
                    "write_file overwrite target {:?} is not a regular file",
                    args.path
                ));
            }
        }
    }
    Ok(WriteFilePlan { args, target })
}

fn execute_write_file(plan: &WriteFilePlan) -> Result<String, String> {
    workspace_path::revalidate_write_target(&plan.target).map_err(|error| error.to_string())?;
    match plan.args.mode {
        WriteMode::Create => {
            create_file(&plan.target.canonical_path, plan.args.content.as_bytes())?
        }
        WriteMode::Overwrite => {
            replace_file(&plan.target.canonical_path, plan.args.content.as_bytes())?
        }
    }
    let result = WriteFileResult {
        path: &plan.args.path,
        mode: plan.args.mode,
        bytes_written: plan.args.content.len(),
        atomic_replacement: plan.args.mode == WriteMode::Overwrite,
    };
    serde_json::to_string(&result).map_err(|_| "write_file could not encode its result".to_owned())
}

fn create_file(path: &Path, content: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("write_file could not create the reviewed path: {error}"))?;
    file.write_all(content)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write_file could not finish the new file: {error}"))
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), String> {
    let permissions = fs::metadata(path)
        .map_err(|error| format!("write_file could not inspect the reviewed target: {error}"))?
        .permissions();
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .map_err(|error| format!("write_file could not stage atomic replacement: {error}"))?;
    file.as_file()
        .set_permissions(permissions)
        .map_err(|error| format!("write_file could not preserve target permissions: {error}"))?;
    file.write_all(content)
        .map_err(|error| format!("write_file could not stage replacement bytes: {error}"))?;
    file.commit()
        .map_err(|error| format!("write_file could not commit atomic replacement: {error}"))
}

impl Tool for WriteFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Create a new UTF-8 file or atomically overwrite an existing one. The mode is mandatory; workspace paths follow normal policy and absolute external paths require exact review.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "content", "mode"],
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path, or an absolute external path requiring exact approval"},
                    "content": {"type": "string", "description": "Complete UTF-8 file content; Xana enforces an immutable 262144-byte encoded limit"},
                    "mode": {"type": "string", "enum": ["create", "overwrite"], "description": "create refuses existing paths; overwrite refuses missing paths and commits atomically"}
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
        let plan = plan_write_file(arguments, workspace_root)?;
        let final_arguments =
            serde_json::to_value(&plan.args).map_err(|error| error.to_string())?;
        let scope = match plan.target.location {
            workspace_path::ResolvedPathLocation::Workspace => PermissionScope::WorkspacePath {
                canonical_path: plan.target.canonical_path.clone(),
            },
            workspace_path::ResolvedPathLocation::External => PermissionScope::ExternalPath {
                canonical_path: plan.target.canonical_path.clone(),
            },
        };
        Ok(PlannedToolInvocation::new(final_arguments, scope, plan))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: super::ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(
            async move { execute_write_file(planned.executable::<WriteFilePlan>("write_file")?) },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_overwrite_are_explicit_and_preserve_failed_preconditions() {
        let workspace = tempdir().expect("workspace");
        let path = workspace.path().join("note.txt");
        let create = plan_write_file(
            &json!({"path": "note.txt", "content": "first", "mode": "create"}),
            workspace.path(),
        )
        .expect("create plan");
        execute_write_file(&create).expect("create");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");

        assert!(
            plan_write_file(
                &json!({"path": "note.txt", "content": "wrong", "mode": "create"}),
                workspace.path()
            )
            .is_err()
        );
        let overwrite = plan_write_file(
            &json!({"path": "note.txt", "content": "second", "mode": "overwrite"}),
            workspace.path(),
        )
        .expect("overwrite plan");
        let output = execute_write_file(&overwrite).expect("overwrite");
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["atomic_replacement"],
            true
        );
    }

    #[test]
    fn external_target_uses_exact_external_scope() {
        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside");
        let arguments = json!({
            "path": outside.path().join("note.txt").to_string_lossy(),
            "content": "external",
            "mode": "create"
        });
        let planned = WriteFile.plan(&arguments, workspace.path()).expect("plan");

        assert!(matches!(
            planned.scope,
            PermissionScope::ExternalPath { .. }
        ));
    }

    #[test]
    fn target_race_is_rejected_without_overwriting_the_winner() {
        let workspace = tempdir().expect("workspace");
        let plan = plan_write_file(
            &json!({"path": "race.txt", "content": "ours", "mode": "create"}),
            workspace.path(),
        )
        .expect("plan");
        fs::write(workspace.path().join("race.txt"), "winner").expect("winner");

        assert!(execute_write_file(&plan).is_err());
        assert_eq!(
            fs::read_to_string(workspace.path().join("race.txt")).unwrap(),
            "winner"
        );
    }

    #[test]
    fn oversized_content_fails_before_workspace_io() {
        let missing = std::path::PathBuf::from("missing-workspace");
        assert!(plan_write_file(
            &json!({"path": "large.txt", "content": "x".repeat(MAX_WRITE_BYTES + 1), "mode": "create"}),
            &missing
        )
        .is_err());
    }
}
