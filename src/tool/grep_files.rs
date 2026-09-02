//! Bounded content search over gitignore-aware workspace files.

use super::{
    EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition, discovery,
    workspace_path,
};
use crate::permission::PermissionScope;
use futures::future::BoxFuture;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs::File, io::Read, path::Path};

const MAX_QUERY_BYTES: usize = 1_024;
const MAX_MATCHES: usize = 1_000;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINE_PREVIEW_BYTES: usize = 512;
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;
const REGEX_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) struct GrepFiles;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepFilesArgs {
    query: String,
    #[serde(default = "default_root")]
    path: String,
    #[serde(default = "default_glob")]
    glob: String,
    #[serde(default)]
    regex: bool,
    #[serde(default = "default_case_sensitive")]
    case_sensitive: bool,
    #[serde(default = "default_depth")]
    max_depth: usize,
    #[serde(default = "default_matches")]
    max_matches: usize,
}

#[derive(Clone)]
struct GrepFilesPlan {
    args: GrepFilesArgs,
    discovery: discovery::DiscoveryPlan,
    matcher: Regex,
}

#[derive(Debug, Serialize)]
struct GrepFilesResult {
    matches: Vec<GrepMatch>,
    files_scanned: usize,
    bytes_scanned: usize,
    binary_files_skipped: usize,
    invalid_utf8_files_skipped: usize,
    oversized_files_skipped: usize,
    changed_files_skipped: usize,
    entries_visited: usize,
    walk_errors: usize,
    symlinks_skipped: usize,
    truncated: bool,
    match_limit_hit: bool,
    output_limit_hit: bool,
    byte_limit_hit: bool,
    entry_limit_hit: bool,
    elapsed_limit_hit: bool,
    continuation: Option<String>,
}

#[derive(Debug, Serialize)]
struct GrepMatch {
    path: String,
    line: usize,
    column_byte: usize,
    preview: String,
    preview_truncated: bool,
}

fn default_root() -> String {
    ".".into()
}

fn default_glob() -> String {
    "**/*".into()
}

fn default_case_sensitive() -> bool {
    true
}

fn default_depth() -> usize {
    16
}

fn default_matches() -> usize {
    100
}

fn plan_grep_files(arguments: &Value, workspace_root: &Path) -> Result<GrepFilesPlan, String> {
    let args: GrepFilesArgs = serde_json::from_value(arguments.clone())
        .map_err(|_| "grep_files arguments are invalid".to_owned())?;
    if args.query.trim().is_empty() || args.query.len() > MAX_QUERY_BYTES {
        return Err(format!(
            "grep_files query must be non-blank and at most {MAX_QUERY_BYTES} bytes"
        ));
    }
    if args.max_matches == 0 || args.max_matches > MAX_MATCHES {
        return Err(format!(
            "grep_files max_matches must be in 1..={MAX_MATCHES}"
        ));
    }
    let source = if args.regex {
        args.query.clone()
    } else {
        regex::escape(&args.query)
    };
    let matcher = RegexBuilder::new(&source)
        .case_insensitive(!args.case_sensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|_| "grep_files query is not a valid bounded regular expression".to_owned())?;
    let discovery = discovery::plan(
        args.path.clone(),
        &args.glob,
        args.max_depth,
        workspace_root,
    )
    .map_err(|error| error.to_string())?;
    Ok(GrepFilesPlan {
        args,
        discovery,
        matcher,
    })
}

fn execute_grep_files(plan: &GrepFilesPlan) -> Result<String, String> {
    let mut matches = Vec::new();
    let mut files_scanned = 0_usize;
    let mut bytes_scanned = 0_usize;
    let mut binary_files_skipped = 0_usize;
    let mut invalid_utf8_files_skipped = 0_usize;
    let mut oversized_files_skipped = 0_usize;
    let mut changed_files_skipped = 0_usize;
    let mut approximate_output_bytes = 512_usize;
    let mut match_limit_hit = false;
    let mut output_limit_hit = false;
    let mut byte_limit_hit = false;

    let stats = discovery::visit(&plan.discovery, |entry| {
        let resolved = match workspace_path::resolve_existing(
            entry.workspace_relative.clone(),
            &plan.discovery.canonical_workspace,
        ) {
            Ok(resolved) => resolved,
            Err(_) => {
                changed_files_skipped = changed_files_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };
        let metadata = match resolved.canonical_path.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return discovery::VisitControl::Continue,
            Err(_) => {
                changed_files_skipped = changed_files_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };
        let Ok(file_len) = usize::try_from(metadata.len()) else {
            oversized_files_skipped = oversized_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        };
        if file_len > MAX_FILE_BYTES {
            oversized_files_skipped = oversized_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        }
        if bytes_scanned.saturating_add(file_len) > MAX_TOTAL_BYTES {
            byte_limit_hit = true;
            return discovery::VisitControl::Stop;
        }
        let mut file = match File::open(&resolved.canonical_path) {
            Ok(file) => file,
            Err(_) => {
                changed_files_skipped = changed_files_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };
        if workspace_path::verify_open_file(&entry.workspace_relative, &file, &resolved.identity)
            .is_err()
        {
            changed_files_skipped = changed_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        }
        let mut bytes = Vec::with_capacity(file_len.min(MAX_FILE_BYTES));
        if (&mut file)
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            changed_files_skipped = changed_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        }
        if bytes.len() > MAX_FILE_BYTES {
            oversized_files_skipped = oversized_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        }
        files_scanned = files_scanned.saturating_add(1);
        bytes_scanned = bytes_scanned.saturating_add(bytes.len());
        if bytes.contains(&0) {
            binary_files_skipped = binary_files_skipped.saturating_add(1);
            return discovery::VisitControl::Continue;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(_) => {
                invalid_utf8_files_skipped = invalid_utf8_files_skipped.saturating_add(1);
                return discovery::VisitControl::Continue;
            }
        };

        for (line_index, line) in text.lines().enumerate() {
            for found in plan.matcher.find_iter(line) {
                if matches.len() >= plan.args.max_matches {
                    match_limit_hit = true;
                    return discovery::VisitControl::Stop;
                }
                let (preview, preview_truncated) = bounded_preview(line);
                let estimated = entry
                    .workspace_relative
                    .len()
                    .saturating_add(preview.len())
                    .saturating_add(160);
                if approximate_output_bytes.saturating_add(estimated) > discovery::MAX_RESULT_BYTES
                {
                    output_limit_hit = true;
                    return discovery::VisitControl::Stop;
                }
                approximate_output_bytes = approximate_output_bytes.saturating_add(estimated);
                matches.push(GrepMatch {
                    path: entry.workspace_relative.clone(),
                    line: line_index.saturating_add(1),
                    column_byte: found.start().saturating_add(1),
                    preview,
                    preview_truncated,
                });
            }
        }
        discovery::VisitControl::Continue
    })
    .map_err(|error| error.to_string())?;

    let truncated = match_limit_hit
        || output_limit_hit
        || byte_limit_hit
        || stats.entry_limit_hit
        || stats.elapsed_limit_hit;
    let result = GrepFilesResult {
        matches,
        files_scanned,
        bytes_scanned,
        binary_files_skipped,
        invalid_utf8_files_skipped,
        oversized_files_skipped,
        changed_files_skipped,
        entries_visited: stats.entries_visited,
        walk_errors: stats.walk_errors,
        symlinks_skipped: stats.symlinks_skipped,
        truncated,
        match_limit_hit,
        output_limit_hit,
        byte_limit_hit,
        entry_limit_hit: stats.entry_limit_hit,
        elapsed_limit_hit: stats.elapsed_limit_hit,
        continuation: truncated.then(|| {
            "rerun with a narrower path, glob, query, or result limit; filesystem search has no stable cursor"
                .to_owned()
        }),
    };
    let encoded = serde_json::to_string(&result)
        .map_err(|_| "grep_files could not encode its bounded result".to_owned())?;
    if encoded.len() > discovery::MAX_RESULT_BYTES {
        return Err("grep_files result exceeded its immutable output bound".to_owned());
    }
    Ok(encoded)
}

fn bounded_preview(line: &str) -> (String, bool) {
    if line.len() <= MAX_LINE_PREVIEW_BYTES {
        return (line.to_owned(), false);
    }
    let mut end = MAX_LINE_PREVIEW_BYTES;
    while !line.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (line[..end].to_owned(), true)
}

impl Tool for GrepFiles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep_files".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: "Search bounded UTF-8 workspace files in stable path/line order. Honors gitignore, skips binary/symlink/oversized input, and reports every truncation bound.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {"type": "string", "description": "Literal text by default, or a bounded Rust regular expression when regex=true"},
                    "path": {"type": "string", "default": ".", "description": "Existing workspace-relative directory to search"},
                    "glob": {"type": "string", "default": "**/*", "description": "Relative '/'-separated file glob"},
                    "regex": {"type": "boolean", "default": false},
                    "case_sensitive": {"type": "boolean", "default": true},
                    "max_depth": {"type": "integer", "minimum": 0, "maximum": discovery::MAX_DEPTH, "default": 16},
                    "max_matches": {"type": "integer", "minimum": 1, "maximum": MAX_MATCHES, "default": 100}
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
        let plan = plan_grep_files(arguments, workspace_root)?;
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
            let plan = planned.executable::<GrepFilesPlan>("grep_files")?.clone();
            tokio::task::spawn_blocking(move || execute_grep_files(&plan))
                .await
                .map_err(|error| format!("grep_files worker stopped: {error}"))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn literal_search_is_stable_gitignore_aware_and_skips_binary_input() {
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join("a.txt"), "alpha\nXana here\n").expect("a");
        fs::write(workspace.path().join("b.txt"), b"Xana\0binary").expect("binary");
        fs::write(workspace.path().join("ignored.txt"), "Xana ignored\n").expect("ignored");
        fs::write(workspace.path().join(".gitignore"), "ignored.txt\n").expect("ignore");
        let output = execute_grep_files(
            &plan_grep_files(&json!({"query": "Xana"}), workspace.path()).expect("plan"),
        )
        .expect("grep");
        let value: Value = serde_json::from_str(&output).expect("JSON");

        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["matches"][0]["path"], "a.txt");
        assert_eq!(value["matches"][0]["line"], 2);
        assert_eq!(value["binary_files_skipped"], 1);
        assert!(output.len() <= discovery::MAX_RESULT_BYTES);
    }

    #[test]
    fn regex_and_result_complexity_are_bounded_before_traversal() {
        let parent = tempdir().expect("parent");
        let missing = parent.path().join("missing");
        let huge = "(".repeat(MAX_QUERY_BYTES + 1);

        assert!(plan_grep_files(&json!({"query": huge, "regex": true}), &missing).is_err());
        assert!(plan_grep_files(&json!({"query": "(", "regex": true}), &missing).is_err());
        assert!(
            plan_grep_files(
                &json!({"query": "x", "max_matches": MAX_MATCHES + 1}),
                &missing
            )
            .is_err()
        );
    }

    #[test]
    fn large_lines_and_match_counts_never_escape_the_result_bound() {
        let workspace = tempdir().expect("workspace");
        let text = (0..400)
            .map(|_| format!("needle {}\n", "x".repeat(1_000)))
            .collect::<String>();
        fs::write(workspace.path().join("large.txt"), text).expect("large");
        let output = execute_grep_files(
            &plan_grep_files(
                &json!({"query": "needle", "max_matches": 1000}),
                workspace.path(),
            )
            .expect("plan"),
        )
        .expect("grep");
        let value: Value = serde_json::from_str(&output).expect("JSON");

        assert!(output.len() <= discovery::MAX_RESULT_BYTES);
        assert_eq!(value["truncated"], true);
        assert!(value["matches"].as_array().unwrap().len() < 400);
    }
}
