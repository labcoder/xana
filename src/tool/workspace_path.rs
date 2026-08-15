//! Shared lexical and canonical workspace-path enforcement for host tools.
//!
//! Callers validate arguments before filesystem access, then use this module
//! to keep resolved targets beneath the launch workspace, including symlinks.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
#[cfg(any(unix, windows))]
use std::sync::Arc;

#[derive(Debug)]
pub(super) enum WorkspacePathError {
    InvalidPath {
        requested_path: String,
    },
    WorkspaceUnavailable {
        source: io::Error,
    },
    Unavailable {
        requested_path: String,
        source: io::Error,
    },
    OutsideWorkspace {
        requested_path: String,
    },
    ChangedSincePlanning {
        requested_path: String,
    },
}

impl fmt::Display for WorkspacePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { requested_path } => write!(
                f,
                "path {requested_path:?} must be a non-empty relative path"
            ),
            Self::WorkspaceUnavailable { .. } => {
                write!(f, "the workspace root is unavailable")
            }
            Self::Unavailable { requested_path, .. } => {
                write!(f, "path {requested_path:?} is unavailable")
            }
            Self::OutsideWorkspace { requested_path } => {
                write!(f, "path {requested_path:?} resolves outside the workspace")
            }
            Self::ChangedSincePlanning { requested_path } => write!(
                f,
                "path {requested_path:?} changed after permission planning"
            ),
        }
    }
}

impl Error for WorkspacePathError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkspaceUnavailable { source } | Self::Unavailable { source, .. } => {
                Some(source)
            }
            Self::InvalidPath { .. }
            | Self::OutsideWorkspace { .. }
            | Self::ChangedSincePlanning { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct ResolvedPath {
    pub(super) requested_path: String,
    pub(super) canonical_path: PathBuf,
    pub(super) identity: FileIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedPathLocation {
    Workspace,
    External,
}

/// Retains the OS handle so its filesystem identity cannot be recycled before execution.
#[cfg(any(unix, windows))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileIdentity(Arc<same_file::Handle>);

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity {
    len: u64,
}

pub(super) fn resolve_existing(
    requested_path: String,
    workspace_root: &Path,
) -> Result<ResolvedPath, WorkspacePathError> {
    let relative_path = Path::new(&requested_path);
    let has_forbidden_component = relative_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });

    if requested_path.trim().is_empty() || relative_path.is_absolute() || has_forbidden_component {
        return Err(WorkspacePathError::InvalidPath { requested_path });
    }

    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|source| WorkspacePathError::WorkspaceUnavailable { source })?;
    let canonical_path = canonical_root
        .join(relative_path)
        .canonicalize()
        .map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;

    if !canonical_path.starts_with(&canonical_root) {
        return Err(WorkspacePathError::OutsideWorkspace { requested_path });
    }

    let identity = file_identity(&canonical_path, &requested_path)?;
    Ok(ResolvedPath {
        requested_path,
        canonical_path,
        identity,
    })
}

pub(super) fn resolve_existing_for_read(
    requested_path: String,
    workspace_root: &Path,
) -> Result<(ResolvedPath, ResolvedPathLocation), WorkspacePathError> {
    let path = Path::new(&requested_path);
    if !path.is_absolute() {
        return resolve_existing(requested_path, workspace_root)
            .map(|resolved| (resolved, ResolvedPathLocation::Workspace));
    }
    if requested_path.trim().is_empty() {
        return Err(WorkspacePathError::InvalidPath { requested_path });
    }
    let canonical_root = workspace_root
        .canonicalize()
        .map_err(|source| WorkspacePathError::WorkspaceUnavailable { source })?;
    let canonical_path = path
        .canonicalize()
        .map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.clone(),
            source,
        })?;
    let location = if canonical_path.starts_with(canonical_root) {
        ResolvedPathLocation::Workspace
    } else {
        ResolvedPathLocation::External
    };
    let identity = file_identity(&canonical_path, &requested_path)?;
    Ok((
        ResolvedPath {
            requested_path,
            canonical_path,
            identity,
        },
        location,
    ))
}

pub(super) fn revalidate_path(
    requested_path: &str,
    canonical_path: &Path,
    expected_identity: &FileIdentity,
) -> Result<(), WorkspacePathError> {
    let current =
        canonical_path
            .canonicalize()
            .map_err(|source| WorkspacePathError::Unavailable {
                requested_path: requested_path.to_owned(),
                source,
            })?;
    let current_identity = file_identity(canonical_path, requested_path)?;
    if current != canonical_path || &current_identity != expected_identity {
        return Err(WorkspacePathError::ChangedSincePlanning {
            requested_path: requested_path.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn verify_open_file(
    requested_path: &str,
    file: &fs::File,
    expected_identity: &FileIdentity,
) -> Result<(), WorkspacePathError> {
    let current_identity = identity_from_file(file, requested_path)?;
    if &current_identity != expected_identity {
        return Err(WorkspacePathError::ChangedSincePlanning {
            requested_path: requested_path.to_owned(),
        });
    }
    Ok(())
}

fn file_identity(path: &Path, requested_path: &str) -> Result<FileIdentity, WorkspacePathError> {
    #[cfg(any(unix, windows))]
    {
        same_file::Handle::from_path(path)
            .map(|handle| FileIdentity(Arc::new(handle)))
            .map_err(|source| WorkspacePathError::Unavailable {
                requested_path: requested_path.to_owned(),
                source,
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = fs::metadata(path).map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.to_owned(),
            source,
        })?;
        Ok(FileIdentity {
            len: metadata.len(),
        })
    }
}

#[cfg(any(unix, windows))]
fn identity_from_file(
    file: &fs::File,
    requested_path: &str,
) -> Result<FileIdentity, WorkspacePathError> {
    let handle = file
        .try_clone()
        .map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.to_owned(),
            source,
        })?;
    same_file::Handle::from_file(handle)
        .map(|handle| FileIdentity(Arc::new(handle)))
        .map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.to_owned(),
            source,
        })
}

#[cfg(not(any(unix, windows)))]
fn identity_from_file(
    file: &fs::File,
    requested_path: &str,
) -> Result<FileIdentity, WorkspacePathError> {
    file.metadata()
        .map(|metadata| FileIdentity {
            len: metadata.len(),
        })
        .map_err(|source| WorkspacePathError::Unavailable {
            requested_path: requested_path.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolves_existing_nested_path() {
        let workspace = tempdir().expect("temporary workspace");
        let notes = workspace.path().join("notes");
        fs::create_dir(&notes).expect("notes directory");
        fs::write(notes.join("hello.txt"), "hello").expect("text fixture");

        let resolved = resolve_existing("notes/hello.txt".to_owned(), workspace.path())
            .expect("nested path should resolve");

        assert_eq!(resolved.requested_path, "notes/hello.txt");
        assert_eq!(
            resolved.canonical_path,
            notes
                .join("hello.txt")
                .canonicalize()
                .expect("canonical file")
        );
    }

    #[test]
    fn rejects_invalid_paths_before_workspace_io() {
        let parent = tempdir().expect("temporary parent");
        let unavailable_workspace = parent.path().join("missing-workspace");
        let cases = [
            "   ".to_owned(),
            parent
                .path()
                .join("absolute.txt")
                .to_string_lossy()
                .into_owned(),
            "../outside.txt".to_owned(),
        ];

        for requested_path in cases {
            let result = resolve_existing(requested_path.clone(), &unavailable_workspace);

            assert!(matches!(
                result,
                Err(WorkspacePathError::InvalidPath {
                    requested_path: actual,
                }) if actual == requested_path
            ));
        }
    }

    #[test]
    fn missing_path_retains_request_and_source() {
        let workspace = tempdir().expect("temporary workspace");

        let result = resolve_existing("missing.txt".to_owned(), workspace.path());

        match result {
            Err(WorkspacePathError::Unavailable {
                requested_path,
                source,
            }) => {
                assert_eq!(requested_path, "missing.txt");
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected unavailable path, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_that_resolves_outside_workspace() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("temporary workspace");
        let outside = tempdir().expect("temporary outside directory");
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "outside").expect("outside fixture");
        symlink(&outside_file, workspace.path().join("escape.txt")).expect("escape symlink");

        let result = resolve_existing("escape.txt".to_owned(), workspace.path());

        assert!(matches!(
            result,
            Err(WorkspacePathError::OutsideWorkspace { requested_path })
                if requested_path == "escape.txt"
        ));
    }
}
