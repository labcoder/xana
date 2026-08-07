use super::{
    ContextError, ContextSource, SourceOrigin, SourceProvenance, TransientSourceId, TrustClass,
    canonical_text,
};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

pub(super) const MAX_PROJECT_SOURCE_BYTES: usize = 64 * 1024;
const PROJECT_SOURCE_TOKEN_BUDGET: usize = 1_024;
const PROJECT_INSTRUCTIONS: &str = "AGENTS.md";

pub(crate) fn load_project_sources(
    workspace_root: &Path,
) -> Result<Vec<ContextSource>, ContextError> {
    let path = workspace_root.join(PROJECT_INSTRUCTIONS);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(ContextError::SourceMetadata { path, source }),
    };

    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ContextError::InvalidSourceKind { path });
    }

    let file = File::open(&path).map_err(|source| ContextError::SourceRead {
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(MAX_PROJECT_SOURCE_BYTES.min(metadata.len() as usize));
    file.take((MAX_PROJECT_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ContextError::SourceRead {
            path: path.clone(),
            source,
        })?;

    if bytes.len() > MAX_PROJECT_SOURCE_BYTES {
        return Err(ContextError::SourceTooLarge {
            path,
            limit: MAX_PROJECT_SOURCE_BYTES,
        });
    }

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
