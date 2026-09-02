//! Bounded, gitignore-aware workspace path discovery.

use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, discovery,
    workspace_path,
};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path};

pub(crate) struct FindFiles;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindFilesArgs {
    #[serde(default = "default_root")]
    path: String,
    pattern: String,
    #[serde(default = "default_depth")]
    max_depth: usize,
    #[serde(default = "default_results")]
    max_results: usize,
    #[serde(default)]
    include_metadata: bool,
}

#[derive(Debug, Serialize)]
struct FindFilesResult {
    entries: Vec<FoundEntry>,
    entries_visited: usize,
    walk_errors: usize,
    symlinks_skipped: usize,
    changed_entries_skipped: usize,
    truncated: bool,
    result_limit_hit: bool,
    output_limit_hit: bool,
    entry_limit_hit: bool,
    elapsed_limit_hit: bool,
    continuation: Option<String>,
}

#[derive(Debug, Serialize)]
struct FoundEntry {
    path: String,
    kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    depth: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
    Other,
}

#[derive(Clone)]
struct FindFilesPlan {
    args: FindFilesArgs,
    discovery: discovery::DiscoveryPlan,
}

fn default_root() -> String {
    ".".into()
}

fn default_depth() -> usize {
    8
}

fn default_results() -> usize {
    200
}

fn plan_find_files(arguments: &Value, workspace_root: &Path) -> Result<FindFilesPlan, String> {
    let args: FindFilesArgs = serde_json::from_value(arguments.clone())
        .map_err(|_| "find_files arguments are invalid".to_owned())?;
    if args.max_results == 0 || args.max_results > discovery::MAX_RESULTS {
        return Err(format!(
            "find_files max_results must be in 1..={}",
            discovery::MAX_RESULTS
        ));
    }
    let discovery = discovery::plan(
        args.path.clone(),
        &args.pattern,
        args.max_depth,
        workspace_root,
    )
    .map_err(|error| error.to_string())?;
    Ok(FindFilesPlan { args, discovery })
}

fn execute_find_files(plan: &FindFilesPlan) -> Result<String, String> {
    let mut entries = Vec::new();
    let mut approximate_bytes = 256_usize;
    let mut result_limit_hit = false;
    let mut output_limit_hit = false;
    let mut changed_entries_skipped = 0_usize;
    let stats = discovery::visit(&plan.discovery, |entry| {
        if entries.len() >= plan.args.max_results {
            result_limit_hit = true;
            return discovery::VisitControl::Stop;
        }
        let resolved = match workspace_path::resolve_existing(
            entry.workspace_relative.clone(),
            &plan.discovery.canonical_workspace,
        ) {
            Ok(resolved) => resolved,
            Err(_) => {
                changed_entries_skipped = changed_entries_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };
        let metadata = match fs::metadata(&resolved.canonical_path) {
            Ok(metadata) => metadata,
            Err(_) => {
                changed_entries_skipped = changed_entries_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };
        let kind = if metadata.is_file() {
            EntryKind::File
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::Other
        };
        let size_bytes =
            (plan.args.include_metadata && metadata.is_file()).then_some(metadata.len());
        let estimated = entry.workspace_relative.len().saturating_add(128);
        if approximate_bytes.saturating_add(estimated) > discovery::MAX_RESULT_BYTES {
            output_limit_hit = true;
            return discovery::VisitControl::Stop;
        }
        approximate_bytes = approximate_bytes.saturating_add(estimated);
        entries.push(FoundEntry {
            path: entry.workspace_relative,
            kind,
            size_bytes,
            depth: entry.depth,
        });
        discovery::VisitControl::Continue
    })
    .map_err(|error| error.to_string())?;
    let truncated =
        result_limit_hit || output_limit_hit || stats.entry_limit_hit || stats.elapsed_limit_hit;
    let result = FindFilesResult {
        entries,
        entries_visited: stats.entries_visited,
        walk_errors: stats.walk_errors,
        symlinks_skipped: stats.symlinks_skipped,
        changed_entries_skipped,
        truncated,
        result_limit_hit,
        output_limit_hit,
        entry_limit_hit: stats.entry_limit_hit,
        elapsed_limit_hit: stats.elapsed_limit_hit,
        continuation: truncated.then(|| {
            "rerun with a narrower path or pattern; filesystem discovery has no stable cursor"
                .to_owned()
        }),
    };
    let encoded = serde_json::to_string(&result)
        .map_err(|_| "find_files could not encode its bounded result".to_owned())?;
    if encoded.len() > discovery::MAX_RESULT_BYTES {
        return Err("find_files result exceeded its immutable output bound".to_owned());
    }
    Ok(encoded)
}

impl Tool for FindFiles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "find_files".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Find workspace paths with one bounded, gitignore-aware glob. Symlinks are never followed; narrow path or pattern when the result reports truncation.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["pattern"],
                "properties": {
                    "path": {"type": "string", "default": ".", "description": "Existing workspace-relative directory to search"},
                    "pattern": {"type": "string", "description": "Relative '/'-separated glob such as '**/*.rs'"},
                    "max_depth": {"type": "integer", "minimum": 0, "maximum": discovery::MAX_DEPTH, "default": 8},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": discovery::MAX_RESULTS, "default": 200},
                    "include_metadata": {"type": "boolean", "default": false, "description": "Include kind for every result and byte size for regular files; timestamps and platform-dependent directory sizes are omitted"}
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
        let plan = plan_find_files(arguments, workspace_root)?;
        let final_arguments =
            serde_json::to_value(&plan.args).map_err(|error| error.to_string())?;
        let scope = PermissionScope::WorkspacePath {
            canonical_path: plan.discovery.canonical_root.clone(),
        };
        Ok(PlannedToolInvocation::new(final_arguments, scope, plan))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        _context: super::ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<FindFilesPlan>("find_files")?.clone();
            tokio::task::spawn_blocking(move || execute_find_files(&plan))
                .await
                .map_err(|error| format!("find_files worker stopped: {error}"))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn returns_stable_gitignore_aware_paths_and_bounded_metadata() {
        let workspace = tempdir().expect("workspace");
        fs::create_dir(workspace.path().join("src")).expect("src");
        fs::write(workspace.path().join("src/lib.rs"), "pub fn xana() {}\n").expect("lib");
        fs::write(workspace.path().join("ignored.rs"), "ignored\n").expect("ignored");
        fs::write(workspace.path().join(".gitignore"), "ignored.rs\n").expect("ignore");

        let output = execute_find_files(
            &plan_find_files(
                &json!({"pattern": "**/*.rs", "include_metadata": true}),
                workspace.path(),
            )
            .expect("plan"),
        )
        .expect("find");
        let value: Value = serde_json::from_str(&output).expect("JSON");

        assert_eq!(value["entries"][0]["path"], "src/lib.rs");
        assert_eq!(value["entries"][0]["kind"], "file");
        assert_eq!(value["truncated"], false);
        assert!(output.len() <= discovery::MAX_RESULT_BYTES);
    }

    #[test]
    fn result_count_reports_an_honest_non_cursor_continuation() {
        let workspace = tempdir().expect("workspace");
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(workspace.path().join(name), name).expect("fixture");
        }
        let output = execute_find_files(
            &plan_find_files(
                &json!({"pattern": "*.txt", "max_results": 1}),
                workspace.path(),
            )
            .expect("plan"),
        )
        .expect("find");
        let value: Value = serde_json::from_str(&output).expect("JSON");

        assert_eq!(value["entries"].as_array().unwrap().len(), 1);
        assert_eq!(value["truncated"], true);
        assert!(value["continuation"].as_str().unwrap().contains("narrower"));
    }

    #[test]
    fn wide_results_remain_bounded_and_report_their_resource_facts() {
        let workspace = tempdir().expect("workspace");
        for index in 0..250 {
            fs::write(
                workspace.path().join(format!("entry-{index:03}.txt")),
                "fixture",
            )
            .expect("fixture");
        }
        let output = execute_find_files(
            &plan_find_files(
                &json!({"pattern": "*.txt", "max_results": 40}),
                workspace.path(),
            )
            .expect("plan"),
        )
        .expect("find");
        let value: Value = serde_json::from_str(&output).expect("JSON");

        assert_eq!(value["entries"].as_array().unwrap().len(), 40);
        assert_eq!(value["result_limit_hit"], true);
        assert_eq!(value["truncated"], true);
        assert!(value["entries_visited"].as_u64().unwrap() <= 41);
        assert!(output.len() <= discovery::MAX_RESULT_BYTES);
        eprintln!(
            "find_files baseline: visited={} returned=40 output_bytes={}",
            value["entries_visited"],
            output.len()
        );
    }
}
