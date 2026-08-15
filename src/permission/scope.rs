use super::PermissionScope;
use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

pub(super) fn resolve_rule_workspace(
    configured: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, ScopeError> {
    if configured.as_os_str().is_empty() || configured.is_absolute() {
        return Err(ScopeError::InvalidLexicalPath(configured.to_owned()));
    }
    for component in configured.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(ScopeError::InvalidLexicalPath(configured.to_owned()));
        }
    }

    let root = workspace_root
        .canonicalize()
        .map_err(|source| ScopeError::Canonicalize {
            path: workspace_root.to_owned(),
            source,
        })?;
    let canonical =
        root.join(configured)
            .canonicalize()
            .map_err(|source| ScopeError::Canonicalize {
                path: configured.to_owned(),
                source,
            })?;
    if !canonical.starts_with(&root) {
        return Err(ScopeError::EscapesWorkspace(configured.to_owned()));
    }
    Ok(canonical)
}

pub(crate) fn scope_contains(grant: &PermissionScope, request: &PermissionScope) -> bool {
    match (grant, request) {
        (
            PermissionScope::WorkspacePath {
                canonical_path: grant,
            },
            PermissionScope::WorkspacePath {
                canonical_path: request,
            },
        ) => request.starts_with(grant),
        (
            PermissionScope::ExternalPath {
                canonical_path: grant,
            },
            PermissionScope::ExternalPath {
                canonical_path: request,
            },
        ) => grant == request,
        (
            PermissionScope::Command {
                shell: grant_shell,
                canonical_cwd: grant_cwd,
                command: grant_command,
            },
            PermissionScope::Command {
                shell: request_shell,
                canonical_cwd: request_cwd,
                command: request_command,
            },
        ) => {
            grant_shell == request_shell
                && grant_cwd == request_cwd
                && grant_command == request_command
        }
        (
            PermissionScope::External {
                recipient_identity_digest: grant_recipient,
                operation: grant_operation,
            },
            PermissionScope::External {
                recipient_identity_digest: request_recipient,
                operation: request_operation,
            },
        ) => grant_recipient == request_recipient && grant_operation == request_operation,
        (PermissionScope::Unscoped, PermissionScope::Unscoped) => true,
        _ => false,
    }
}

#[derive(Debug)]
pub(super) enum ScopeError {
    InvalidLexicalPath(PathBuf),
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    EscapesWorkspace(PathBuf),
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLexicalPath(path) => write!(
                f,
                "permission workspace {:?} must be a non-empty relative path without parent traversal",
                path
            ),
            Self::Canonicalize { path, source } => write!(
                f,
                "could not canonicalize permission workspace {}: {source}",
                path.display()
            ),
            Self::EscapesWorkspace(path) => write!(
                f,
                "permission workspace {} resolves outside Xana's workspace",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ScopeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Canonicalize { source, .. } => Some(source),
            Self::InvalidLexicalPath(_) | Self::EscapesWorkspace(_) => None,
        }
    }
}
