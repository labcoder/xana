use std::{
    error::Error,
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Reads a regular file without ever allocating beyond `limit + 1` bytes.
///
/// The metadata check rejects known-large files early. The limited reader also
/// catches files that grow between inspection and reading.
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>, BoundedReadError> {
    let file = fs::File::open(path).map_err(|source| BoundedReadError::Io {
        path: path.to_owned(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| BoundedReadError::Io {
        path: path.to_owned(),
        source,
    })?;
    if metadata.len() > limit as u64 {
        return Err(BoundedReadError::TooLarge {
            path: path.to_owned(),
            actual: metadata.len(),
            limit,
        });
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| BoundedReadError::Io {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(BoundedReadError::TooLarge {
            path: path.to_owned(),
            actual: bytes.len() as u64,
            limit,
        });
    }
    Ok(bytes)
}

pub(crate) fn read_to_string(path: &Path, limit: usize) -> Result<String, BoundedReadError> {
    String::from_utf8(read(path, limit)?).map_err(|error| BoundedReadError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, error),
    })
}

#[derive(Debug)]
pub(crate) enum BoundedReadError {
    TooLarge {
        path: PathBuf,
        actual: u64,
        limit: usize,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for BoundedReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                path,
                actual,
                limit,
            } => write!(
                formatter,
                "{} contains {actual} bytes, exceeding the {limit}-byte limit",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
        }
    }
}

impl Error for BoundedReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::TooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_files_larger_than_the_limit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("large");
        fs::write(&path, b"12345").expect("write fixture");

        let error = read(&path, 4).expect_err("oversized file should fail");
        assert!(matches!(
            error,
            BoundedReadError::TooLarge {
                actual: 5,
                limit: 4,
                ..
            }
        ));
    }
}
