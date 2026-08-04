mod read_file;

use crate::message::{ToolCall, ToolResult};
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolDefinition {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) parameters: serde_json::Value,
}

pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
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
    }]
}

pub(crate) fn execute(call: &ToolCall, workspace_root: &Path) -> ToolResult {
    match call.name.as_str() {
        "read_file" => match read_file::execute(&call.arguments, workspace_root) {
            Ok(output) => ToolResult::success(call.id.clone(), output),
            Err(error) => ToolResult::error(call.id.clone(), error.to_string()),
        },
        unknown => ToolResult::error(call.id.clone(), format!("unknown tool {unknown:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolResultStatus;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn read_file_success_preserves_call_id_and_output() {
        let workspace = tempdir().expect("temporary workspace");
        fs::write(workspace.path().join("hello.txt"), "hello from Xana\n").expect("text fixture");

        let call = ToolCall {
            id: "call-success".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::json!({"path": "hello.txt"}),
        };

        let result = execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-success");
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.output, "hello from Xana\n");
    }

    #[test]
    fn missing_file_returns_correlated_error_without_host_path() {
        let workspace = tempdir().expect("temporary workspace");
        let canonical_workspace = workspace
            .path()
            .canonicalize()
            .expect("canonical workspace");
        let canonical_workspace = canonical_workspace.display().to_string();

        let call = ToolCall {
            id: "call-missing".to_owned(),
            name: "read_file".to_owned(),
            arguments: serde_json::json!({"path": "missing.txt"}),
        };

        let result = execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-missing");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(result.output.contains("missing.txt"));
        assert!(!result.output.starts_with("ERROR:"));
        assert!(!result.output.contains(&canonical_workspace));
    }

    #[test]
    fn unknown_tool_returns_correlated_error() {
        let workspace = tempdir().expect("temporary workspace");

        let call = ToolCall {
            id: "call-unknown".to_owned(),
            name: "load_theme".to_owned(),
            arguments: serde_json::json!({}),
        };

        let result = execute(&call, workspace.path());

        assert_eq!(result.call_id, "call-unknown");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(result.output.contains("load_theme"));
        assert!(!result.output.starts_with("ERROR:"));
    }
}
