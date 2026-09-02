//! Shared bounded, gitignore-aware workspace discovery.
//!
//! This module owns traversal mechanics only. Each public tool keeps its own
//! schema, output contract, effect class, and replay-safety declaration.

use super::workspace_path::{self, FileIdentity, WorkspacePathError};
use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) const MAX_DEPTH: usize = 32;
pub(super) const MAX_RESULTS: usize = 1_000;
pub(super) const MAX_VISITED_ENTRIES: usize = 50_000;
pub(super) const MAX_RESULT_BYTES: usize = 64 * 1024;
pub(super) const MAX_PATTERN_BYTES: usize = 1_024;
pub(super) const MAX_ELAPSED: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) struct DiscoveryPlan {
    pub(super) requested_root: String,
    pub(super) canonical_root: PathBuf,
    pub(super) canonical_workspace: PathBuf,
    pub(super) root_identity: FileIdentity,
    matcher: GlobMatcher,
    max_depth: usize,
}

pub(super) struct DiscoveryEntry {
    pub(super) workspace_relative: String,
    pub(super) depth: usize,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub(super) struct DiscoveryStats {
    pub(super) entries_visited: usize,
    pub(super) walk_errors: usize,
    pub(super) symlinks_skipped: usize,
    pub(super) entry_limit_hit: bool,
    pub(super) elapsed_limit_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisitControl {
    Continue,
    Stop,
}

#[derive(Debug)]
pub(super) enum DiscoveryError {
    Path(WorkspacePathError),
    RootNotDirectory { requested_root: String },
    InvalidPattern,
    InvalidDepth,
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(error) => write!(f, "{error}"),
            Self::RootNotDirectory { requested_root } => {
                write!(f, "discovery root {requested_root:?} is not a directory")
            }
            Self::InvalidPattern => write!(
                f,
                "glob pattern must be non-empty, at most {MAX_PATTERN_BYTES} bytes, use '/' separators, and stay relative"
            ),
            Self::InvalidDepth => {
                write!(f, "max_depth must be in 0..={MAX_DEPTH}")
            }
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(error) => Some(error),
            Self::RootNotDirectory { .. } | Self::InvalidPattern | Self::InvalidDepth => None,
        }
    }
}

pub(super) fn plan(
    requested_root: String,
    pattern: &str,
    max_depth: usize,
    workspace_root: &Path,
) -> Result<DiscoveryPlan, DiscoveryError> {
    if max_depth > MAX_DEPTH {
        return Err(DiscoveryError::InvalidDepth);
    }
    if pattern.is_empty()
        || pattern.len() > MAX_PATTERN_BYTES
        || pattern.contains('\\')
        || pattern.split('/').any(|component| component == "..")
        || Path::new(pattern).is_absolute()
    {
        return Err(DiscoveryError::InvalidPattern);
    }
    let matcher = GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map_err(|_| DiscoveryError::InvalidPattern)?
        .compile_matcher();
    let resolved = workspace_path::resolve_existing(requested_root, workspace_root)
        .map_err(DiscoveryError::Path)?;
    if !resolved.canonical_path.is_dir() {
        return Err(DiscoveryError::RootNotDirectory {
            requested_root: resolved.requested_path,
        });
    }
    let canonical_workspace = workspace_root.canonicalize().map_err(|source| {
        DiscoveryError::Path(WorkspacePathError::WorkspaceUnavailable { source })
    })?;
    Ok(DiscoveryPlan {
        requested_root: resolved.requested_path,
        canonical_root: resolved.canonical_path,
        canonical_workspace,
        root_identity: resolved.identity,
        matcher,
        max_depth,
    })
}

pub(super) fn visit(
    plan: &DiscoveryPlan,
    mut visitor: impl FnMut(DiscoveryEntry) -> VisitControl,
) -> Result<DiscoveryStats, DiscoveryError> {
    workspace_path::revalidate_path(
        &plan.requested_root,
        &plan.canonical_root,
        &plan.root_identity,
    )
    .map_err(DiscoveryError::Path)?;

    let started = Instant::now();
    let mut stats = DiscoveryStats::default();
    let mut builder = WalkBuilder::new(&plan.canonical_root);
    builder
        .current_dir(&plan.canonical_workspace)
        .follow_links(false)
        .max_depth(Some(plan.max_depth))
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(false)
        .same_file_system(true)
        .sort_by_file_path(|left, right| left.cmp(right));
    if plan.canonical_root != plan.canonical_workspace {
        let workspace_ignore = plan.canonical_workspace.join(".gitignore");
        if workspace_ignore.is_file() && builder.add_ignore(workspace_ignore).is_some() {
            stats.walk_errors = stats.walk_errors.saturating_add(1);
        }
    }

    for item in builder.build() {
        if started.elapsed() >= MAX_ELAPSED {
            stats.elapsed_limit_hit = true;
            break;
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => {
                stats.walk_errors = stats.walk_errors.saturating_add(1);
                continue;
            }
        };
        let depth = entry.depth();
        if depth == 0 {
            continue;
        }
        stats.entries_visited = stats.entries_visited.saturating_add(1);
        if stats.entries_visited > MAX_VISITED_ENTRIES {
            stats.entry_limit_hit = true;
            break;
        }
        if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
            stats.symlinks_skipped = stats.symlinks_skipped.saturating_add(1);
            continue;
        }
        let Ok(root_relative) = entry.path().strip_prefix(&plan.canonical_root) else {
            stats.walk_errors = stats.walk_errors.saturating_add(1);
            continue;
        };
        let root_relative = portable_path(root_relative);
        if !plan.matcher.is_match(&root_relative) {
            continue;
        }
        let Ok(workspace_relative) = entry.path().strip_prefix(&plan.canonical_workspace) else {
            stats.walk_errors = stats.walk_errors.saturating_add(1);
            continue;
        };
        let control = visitor(DiscoveryEntry {
            workspace_relative: portable_path(workspace_relative),
            depth,
        });
        if control == VisitControl::Stop {
            break;
        }
        if started.elapsed() >= MAX_ELAPSED {
            stats.elapsed_limit_hit = true;
            break;
        }
    }

    workspace_path::revalidate_path(
        &plan.requested_root,
        &plan.canonical_root,
        &plan.root_identity,
    )
    .map_err(DiscoveryError::Path)?;
    Ok(stats)
}

fn portable_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn traversal_is_sorted_gitignore_aware_and_never_follows_symlinks() {
        let workspace = tempdir().expect("workspace");
        fs::write(workspace.path().join(".gitignore"), "ignored.txt\n").expect("ignore");
        fs::write(workspace.path().join("z.txt"), "z").expect("z");
        fs::write(workspace.path().join("a.txt"), "a").expect("a");
        fs::write(workspace.path().join("ignored.txt"), "ignored").expect("ignored");
        let plan = plan(".".into(), "**/*.txt", 8, workspace.path()).expect("plan");
        let mut paths = Vec::new();
        let stats = visit(&plan, |entry| {
            paths.push(entry.workspace_relative);
            VisitControl::Continue
        })
        .expect("visit");

        assert_eq!(paths, ["a.txt", "z.txt"]);
        assert!(!stats.entry_limit_hit);
        assert!(!stats.elapsed_limit_hit);
    }

    #[test]
    fn hostile_patterns_fail_before_workspace_io() {
        let missing = PathBuf::from("missing-workspace");
        for pattern in ["", "../*.rs", "C:/absolute/*", "bad\\pattern"] {
            assert!(matches!(
                plan(".".into(), pattern, 8, &missing),
                Err(DiscoveryError::InvalidPattern)
            ));
        }
    }

    #[test]
    fn a_subdirectory_search_still_applies_the_workspace_gitignore() {
        let workspace = tempdir().expect("workspace");
        fs::create_dir(workspace.path().join("src")).expect("src");
        fs::write(workspace.path().join(".gitignore"), "src/ignored.txt\n").expect("ignore");
        fs::write(workspace.path().join("src/kept.txt"), "kept").expect("kept");
        fs::write(workspace.path().join("src/ignored.txt"), "ignored").expect("ignored");
        let plan = plan("src".into(), "*.txt", 8, workspace.path()).expect("plan");
        let mut paths = Vec::new();

        visit(&plan, |entry| {
            paths.push(entry.workspace_relative);
            VisitControl::Continue
        })
        .expect("visit");

        assert_eq!(paths, ["src/kept.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn traversal_reports_and_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("workspace");
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("secret.txt"), "outside").expect("outside fixture");
        symlink(outside.path(), workspace.path().join("outside-link")).expect("symlink");
        let plan = plan(".".into(), "**/*", 8, workspace.path()).expect("plan");
        let mut paths = Vec::new();

        let stats = visit(&plan, |entry| {
            paths.push(entry.workspace_relative);
            VisitControl::Continue
        })
        .expect("visit");

        assert!(paths.is_empty());
        assert_eq!(stats.symlinks_skipped, 1);
    }
}
