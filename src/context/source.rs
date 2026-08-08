use super::ContextError;
#[cfg(test)]
use super::{
    ContextSource, SourceOrigin, SourceProvenance, TransientSourceId, TrustClass, canonical_text,
};
use crate::bounded_file;
#[cfg(test)]
use std::path::PathBuf;
use std::{fs, path::Path};

pub(crate) const MAX_PROJECT_SOURCE_BYTES: usize = 64 * 1024;
#[cfg(test)]
const PROJECT_SOURCE_TOKEN_BUDGET: usize = 1_024;
pub(crate) const PROJECT_INSTRUCTIONS: &str = "AGENTS.md";

#[cfg(test)]
pub(crate) fn load_project_sources(
    workspace_root: &Path,
) -> Result<Vec<ContextSource>, ContextError> {
    let Some(bytes) = read_project_instructions(workspace_root)? else {
        return Ok(Vec::new());
    };
    let path = workspace_root.join(PROJECT_INSTRUCTIONS);
    let content = String::from_utf8(bytes).map_err(|source| ContextError::InvalidUtf8 {
        path: path.clone(),
        source,
    })?;

    Ok(vec![ContextSource {
        id: TransientSourceId::new("project:AGENTS.md"),
        provenance: SourceProvenance {
            display_name: "root AGENTS.md".to_owned(),
            path: Some(PathBuf::from(PROJECT_INSTRUCTIONS)),
            origin: SourceOrigin::ProjectFile,
        },
        trust: TrustClass::Project,
        content: canonical_text(&content),
        max_tokens: PROJECT_SOURCE_TOKEN_BUDGET,
    }])
}

pub(crate) fn read_project_instructions(
    workspace_root: &Path,
) -> Result<Option<Vec<u8>>, ContextError> {
    let path = workspace_root.join(PROJECT_INSTRUCTIONS);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ContextError::SourceMetadata { path, source }),
    };

    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ContextError::InvalidSourceKind { path });
    }

    bounded_file::read(&path, MAX_PROJECT_SOURCE_BYTES)
        .map(Some)
        .map_err(|error| match error {
            bounded_file::BoundedReadError::TooLarge { .. } => ContextError::SourceTooLarge {
                path,
                limit: MAX_PROJECT_SOURCE_BYTES,
            },
            bounded_file::BoundedReadError::Io { path, source } => {
                ContextError::SourceRead { path, source }
            }
        })
}
