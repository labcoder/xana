//! Canonical, cross-platform classification for user-supplied local resources.

use std::{error::Error, fmt, fs, path::Path, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalResourcePath {
    Workspace { relative: String },
    External { canonical: PathBuf },
}

#[derive(Debug)]
pub(crate) enum LocalResourcePathError {
    Invalid,
    NotRegular,
    Io(std::io::Error),
}

impl fmt::Display for LocalResourcePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str("resource path is invalid"),
            Self::NotRegular => formatter.write_str("resource path is not a regular file"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for LocalResourcePathError {}

impl From<std::io::Error> for LocalResourcePathError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn classify_local_path(
    workspace_root: &Path,
    source_path: &str,
) -> Result<LocalResourcePath, LocalResourcePathError> {
    let source = normalize_path(source_path)?;
    let root = workspace_root.canonicalize()?;
    let path = if source.is_absolute() {
        source
    } else {
        root.join(source)
    };
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        return Err(LocalResourcePathError::NotRegular);
    }
    let canonical = path.canonicalize()?;
    if let Ok(relative) = canonical.strip_prefix(&root) {
        Ok(LocalResourcePath::Workspace {
            relative: relative.to_string_lossy().into_owned(),
        })
    } else {
        Ok(LocalResourcePath::External { canonical })
    }
}

fn normalize_path(source: &str) -> Result<PathBuf, LocalResourcePathError> {
    let source = source.trim();
    if source.is_empty() || source.contains(['\r', '\n']) {
        return Err(LocalResourcePathError::Invalid);
    }
    let source = if source.len() >= 2
        && ((source.starts_with('"') && source.ends_with('"'))
            || (source.starts_with('\'') && source.ends_with('\'')))
    {
        &source[1..source.len() - 1]
    } else {
        source
    };
    let source = if let Some(path) = source.strip_prefix("file://") {
        if !path.starts_with('/') {
            return Err(LocalResourcePathError::Invalid);
        }
        let path = percent_decode_path(path)?;
        #[cfg(windows)]
        {
            let mut path = path;
            if path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
                && path.as_bytes().get(2) == Some(&b':')
            {
                path.remove(0);
            }
            path
        }
        #[cfg(not(windows))]
        path
    } else {
        source.to_owned()
    };
    #[cfg(windows)]
    let source = msys_windows_path(&source);
    Ok(PathBuf::from(source))
}

fn percent_decode_path(source: &str) -> Result<String, LocalResourcePathError> {
    let bytes = source.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .ok_or(LocalResourcePathError::Invalid)?;
        let high = hex_digit(encoded[0]).ok_or(LocalResourcePathError::Invalid)?;
        let low = hex_digit(encoded[1]).ok_or(LocalResourcePathError::Invalid)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    if decoded.contains(&0) {
        return Err(LocalResourcePathError::Invalid);
    }
    String::from_utf8(decoded).map_err(|_| LocalResourcePathError::Invalid)
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(windows)]
fn msys_windows_path(source: &str) -> String {
    let bytes = source.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/' {
        format!(
            "{}:/{}",
            (bytes[1] as char).to_ascii_uppercase(),
            &source[3..]
        )
    } else {
        source.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_multiline_paths_are_rejected_before_io() {
        assert!(matches!(
            normalize_path(""),
            Err(LocalResourcePathError::Invalid)
        ));
        assert!(matches!(
            normalize_path("one\ntwo"),
            Err(LocalResourcePathError::Invalid)
        ));
    }

    #[test]
    fn file_urls_are_percent_decoded_without_accepting_nul() {
        let path = normalize_path("file:///tmp/a%20b.png").unwrap();
        assert!(path.to_string_lossy().contains("a b.png"));
        assert!(normalize_path("file:///tmp/a%00b.png").is_err());
    }
}
