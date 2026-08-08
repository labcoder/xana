use super::workspace_path::{WorkspacePathError, resolve_existing};
use super::{EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition};
use crate::{
    documents::{BuiltinDocumentExtractor, DocumentExtractor, DocumentInput},
    permission::PermissionScope,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReadDocument {
    extractor: BuiltinDocumentExtractor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    max_output_bytes: Option<usize>,
}

struct Plan {
    args: Args,
    canonical_path: PathBuf,
}

impl Tool for ReadDocument {
    fn definition(&self) -> ToolDefinition {
        let description = if cfg!(feature = "documents-csv") {
            "Extract bounded Markdown from a text or CSV document beneath the workspace"
        } else {
            "Extract bounded UTF-8 text from a document beneath the workspace"
        };
        ToolDefinition {
            name: "read_document",
            contract_version: 1,
            description,
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["path"],
                "properties":{
                    "path":{"type":"string"},
                    "max_output_bytes":{"type":"integer","minimum":1,"maximum":1048576}
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
        let args: Args = serde_json::from_value(arguments.clone())
            .map_err(|_| "read_document arguments are invalid".to_owned())?;
        let max_output = args.max_output_bytes.unwrap_or(DEFAULT_OUTPUT_BYTES);
        if max_output == 0 || max_output > MAX_OUTPUT_BYTES {
            return Err(format!(
                "max_output_bytes must be within 1..={MAX_OUTPUT_BYTES}"
            ));
        }
        let resolved = resolve_existing(args.path.clone(), workspace_root)
            .map_err(|error: WorkspacePathError| error.to_string())?;
        let metadata = resolved
            .canonical_path
            .metadata()
            .map_err(|_| format!("document {:?} is unavailable", args.path))?;
        if !metadata.is_file() {
            return Err(format!("document {:?} is not a regular file", args.path));
        }
        if metadata.len() > MAX_INPUT_BYTES as u64 {
            return Err(format!(
                "document {:?} exceeds the {MAX_INPUT_BYTES}-byte input limit",
                args.path
            ));
        }
        let final_arguments = serde_json::to_value(&args).map_err(|error| error.to_string())?;
        let scope = PermissionScope::WorkspacePath {
            canonical_path: resolved.canonical_path.clone(),
        };
        Ok(PlannedToolInvocation::new(
            final_arguments,
            scope,
            Plan {
                args,
                canonical_path: resolved.canonical_path,
            },
        ))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<Plan>("read_document")?;
            let mut bytes = Vec::new();
            File::open(&plan.canonical_path)
                .map_err(|_| format!("document {:?} is unavailable", plan.args.path))?
                .take((MAX_INPUT_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|_| format!("document {:?} could not be read", plan.args.path))?;
            if bytes.len() > MAX_INPUT_BYTES {
                return Err(format!(
                    "document {:?} exceeds the {MAX_INPUT_BYTES}-byte input limit",
                    plan.args.path
                ));
            }
            let document = self
                .extractor
                .extract(DocumentInput {
                    source_name: Some(plan.args.path.clone()),
                    bytes,
                    max_output_bytes: plan.args.max_output_bytes.unwrap_or(DEFAULT_OUTPUT_BYTES),
                })
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&json!({
                "markdown":document.markdown,
                "media_type":document.media_type,
                "warnings":document.warnings.into_iter().map(|warning| warning.message).collect::<Vec<_>>()
            }))
            .map_err(|error| error.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn plan_opens_only_workspace_regular_files() {
        let workspace = tempdir().unwrap();
        std::fs::write(workspace.path().join("notes.txt"), "hello").unwrap();
        assert!(
            ReadDocument::default()
                .plan(&json!({"path":"notes.txt"}), workspace.path())
                .is_ok()
        );
        assert!(
            ReadDocument::default()
                .plan(&json!({"path":"../outside.txt"}), workspace.path())
                .is_err()
        );
    }

    #[test]
    fn definition_claims_only_the_formats_compiled_into_this_build() {
        let description = ReadDocument::default().definition().description;
        assert_eq!(description.contains("CSV"), cfg!(feature = "documents-csv"));
    }
}
