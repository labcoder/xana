use super::record::{RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord};
use crate::identity::SessionId;
use serde_json::Value;
use std::{
    collections::HashSet,
    error::Error,
    fmt, fs,
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_RECORD_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SESSION_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_SESSION_RECORDS: usize = 10_000;

pub(crate) struct SessionStore {
    session_id: SessionId,
    path: PathBuf,
    file: fs::File,
}

#[derive(Debug)]
pub(crate) struct LoadedSession {
    pub(crate) records: Vec<RecordEnvelope>,
    pub(crate) repair: Option<TornTailRepair>,
    pub(crate) inspected_len: u64,
    pub(crate) inspected_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TornTailRepair {
    pub(crate) truncate_to: u64,
}

impl SessionStore {
    pub(crate) fn path_for(sessions_dir: &Path, session_id: SessionId) -> PathBuf {
        sessions_dir.join(format!("{session_id}.jsonl"))
    }

    pub(crate) fn create(
        sessions_dir: &Path,
        created: RecordEnvelope,
    ) -> Result<Self, SessionError> {
        if !matches!(created.record, SessionRecord::SessionCreated { .. }) {
            return Err(SessionError::InvalidCreationRecord);
        }
        fs::create_dir_all(sessions_dir).map_err(|source| SessionError::Io {
            path: sessions_dir.to_owned(),
            source,
        })?;
        let path = Self::path_for(sessions_dir, created.session_id);
        let file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| SessionError::Io {
                path: path.clone(),
                source,
            })?;
        let mut store = Self {
            session_id: created.session_id,
            path,
            file,
        };
        store.append(&created)?;
        Ok(store)
    }

    pub(crate) fn inspect(path: &Path) -> Result<LoadedSession, SessionError> {
        let metadata = fs::metadata(path).map_err(|source| SessionError::Io {
            path: path.to_owned(),
            source,
        })?;
        let inspected_len = metadata.len();
        if inspected_len > MAX_SESSION_BYTES as u64 {
            return Err(SessionError::SessionTooLarge {
                actual: inspected_len,
                limit: MAX_SESSION_BYTES,
            });
        }
        let bytes = fs::read(path).map_err(|source| SessionError::Io {
            path: path.to_owned(),
            source,
        })?;
        let inspected_hash = blake3::hash(&bytes).to_hex().to_string();
        let mut records = Vec::new();
        let mut record_ids = HashSet::new();
        let mut offset = 0usize;

        while let Some(relative_end) = bytes[offset..].iter().position(|byte| *byte == b'\n') {
            let end = offset + relative_end;
            let line = &bytes[offset..end];
            if line.is_empty() {
                return Err(SessionError::CorruptRecord {
                    offset: offset as u64,
                    reason: "empty JSONL record".to_owned(),
                });
            }
            let envelope = decode_record(line, offset as u64)?;
            if !record_ids.insert(envelope.record_id) {
                return Err(SessionError::DuplicateRecordId {
                    record_id: envelope.record_id.to_string(),
                });
            }
            records.push(envelope);
            if records.len() > MAX_SESSION_RECORDS {
                return Err(SessionError::TooManyRecords {
                    limit: MAX_SESSION_RECORDS,
                });
            }
            offset = end + 1;
        }

        let repair = if offset < bytes.len() {
            if bytes.len() - offset > MAX_RECORD_BYTES {
                return Err(SessionError::RecordTooLarge {
                    offset: offset as u64,
                    actual: bytes.len() - offset,
                    limit: MAX_RECORD_BYTES,
                });
            }
            Some(TornTailRepair {
                truncate_to: offset as u64,
            })
        } else {
            None
        };

        if records.is_empty() {
            return Err(SessionError::MissingCreationRecord);
        }

        Ok(LoadedSession {
            records,
            repair,
            inspected_len,
            inspected_hash,
        })
    }

    pub(crate) fn open_for_resume(
        path: &Path,
        loaded: LoadedSession,
    ) -> Result<Self, SessionError> {
        let bytes = fs::read(path).map_err(|source| SessionError::Io {
            path: path.to_owned(),
            source,
        })?;
        let actual_hash = blake3::hash(&bytes).to_hex().to_string();
        if bytes.len() as u64 != loaded.inspected_len || actual_hash != loaded.inspected_hash {
            return Err(SessionError::ChangedAfterInspection {
                path: path.to_owned(),
            });
        }
        let session_id = loaded
            .records
            .first()
            .map(|record| record.session_id)
            .ok_or(SessionError::MissingCreationRecord)?;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(path)
            .map_err(|source| SessionError::Io {
                path: path.to_owned(),
                source,
            })?;
        if let Some(repair) = loaded.repair {
            file.set_len(repair.truncate_to)
                .map_err(|source| SessionError::Io {
                    path: path.to_owned(),
                    source,
                })?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|source| SessionError::Io {
                path: path.to_owned(),
                source,
            })?;
        Ok(Self {
            session_id,
            path: path.to_owned(),
            file,
        })
    }

    pub(crate) fn append(&mut self, record: &RecordEnvelope) -> Result<(), SessionError> {
        if record.version != SESSION_RECORD_VERSION {
            return Err(SessionError::UnsupportedVersion {
                version: record.version,
            });
        }
        if record.session_id != self.session_id {
            return Err(SessionError::WrongSessionId);
        }
        let mut encoded = serde_json::to_vec(record).map_err(SessionError::Encode)?;
        if encoded.len() > MAX_RECORD_BYTES {
            return Err(SessionError::RecordTooLarge {
                offset: self.file.metadata().map_or(0, |metadata| metadata.len()),
                actual: encoded.len(),
                limit: MAX_RECORD_BYTES,
            });
        }
        encoded.push(b'\n');
        self.file
            .write_all(&encoded)
            .and_then(|()| self.file.flush())
            .map_err(|source| SessionError::Io {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn decode_record(bytes: &[u8], offset: u64) -> Result<RecordEnvelope, SessionError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(SessionError::RecordTooLarge {
            offset,
            actual: bytes.len(),
            limit: MAX_RECORD_BYTES,
        });
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| SessionError::CorruptRecord {
            offset,
            reason: error.to_string(),
        })?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| SessionError::CorruptRecord {
            offset,
            reason: "record version is missing or invalid".to_owned(),
        })?;
    if version != u64::from(SESSION_RECORD_VERSION) {
        return Err(SessionError::UnsupportedVersion {
            version: u32::try_from(version).unwrap_or(u32::MAX),
        });
    }
    serde_json::from_value(value).map_err(|error| SessionError::CorruptRecord {
        offset,
        reason: error.to_string(),
    })
}

#[derive(Debug)]
pub(crate) enum SessionError {
    InvalidCreationRecord,
    MissingCreationRecord,
    UnsupportedVersion {
        version: u32,
    },
    WrongSessionId,
    DuplicateRecordId {
        record_id: String,
    },
    CorruptRecord {
        offset: u64,
        reason: String,
    },
    RecordTooLarge {
        offset: u64,
        actual: usize,
        limit: usize,
    },
    SessionTooLarge {
        actual: u64,
        limit: usize,
    },
    TooManyRecords {
        limit: usize,
    },
    ChangedAfterInspection {
        path: PathBuf,
    },
    Encode(serde_json::Error),
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCreationRecord => {
                write!(formatter, "first session record must create the session")
            }
            Self::MissingCreationRecord => {
                write!(formatter, "session has no committed creation record")
            }
            Self::UnsupportedVersion { version } => write!(
                formatter,
                "session record version {version} is not supported"
            ),
            Self::WrongSessionId => write!(formatter, "record belongs to a different session"),
            Self::DuplicateRecordId { record_id } => {
                write!(formatter, "session record id {record_id} is duplicated")
            }
            Self::CorruptRecord { offset, reason } => write!(
                formatter,
                "session record at byte {offset} is corrupt: {reason}"
            ),
            Self::RecordTooLarge {
                offset,
                actual,
                limit,
            } => write!(
                formatter,
                "session record at byte {offset} contains {actual} bytes, exceeding the {limit}-byte limit"
            ),
            Self::SessionTooLarge { actual, limit } => write!(
                formatter,
                "session contains {actual} bytes, exceeding the {limit}-byte load limit"
            ),
            Self::TooManyRecords { limit } => {
                write!(formatter, "session exceeds the {limit}-record load limit")
            }
            Self::ChangedAfterInspection { path } => write!(
                formatter,
                "session {} changed after read-only inspection",
                path.display()
            ),
            Self::Encode(source) => write!(formatter, "could not encode session record: {source}"),
            Self::Io { path, source } => write!(
                formatter,
                "session I/O at {} failed: {source}",
                path.display()
            ),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
