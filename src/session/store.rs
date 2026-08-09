use super::record::{RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord};
use crate::{
    bounded_file,
    identity::{ConversationEntryId, SessionId, ThreadId},
    message::Message,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, fs,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(crate) const MAX_RECORD_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SESSION_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_SESSION_RECORDS: usize = 10_000;

pub(crate) struct SessionStore {
    session_id: SessionId,
    path: PathBuf,
    file: fs::File,
    _writer_lock: fs::File,
    committed_bytes: u64,
    committed_records: usize,
    writable: bool,
}

#[derive(Debug)]
pub(crate) struct LoadedSession {
    pub(crate) records: Vec<RecordEnvelope>,
    pub(crate) repair: Option<TornTailRepair>,
    pub(crate) inspected_len: u64,
    pub(crate) inspected_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConversationPage {
    pub(crate) messages: Vec<Message>,
    pub(crate) start: usize,
    pub(crate) total: usize,
    pub(crate) has_older: bool,
}

#[derive(Debug, Clone, Copy)]
struct ConversationEntryIndex {
    parent: Option<ConversationEntryId>,
    offset: u64,
    line_len: usize,
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
        let writer_lock = acquire_writer_lock(&path)?;
        let mut store = Self {
            session_id: created.session_id,
            path,
            file,
            _writer_lock: writer_lock,
            committed_bytes: 0,
            committed_records: 0,
            writable: true,
        };
        store.append(&created)?;
        Ok(store)
    }

    pub(crate) fn inspect(path: &Path) -> Result<LoadedSession, SessionError> {
        let bytes = read_session(path)?;
        let inspected_len = bytes.len() as u64;
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

    pub(crate) fn conversation_page(
        path: &Path,
        before: Option<usize>,
        limit: usize,
    ) -> Result<ConversationPage, SessionError> {
        const MAX_PAGE_MESSAGES: usize = 128;
        let limit = limit.clamp(1, MAX_PAGE_MESSAGES);
        let file = fs::File::open(path).map_err(|source| SessionError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut entries = HashMap::<ConversationEntryId, ConversationEntryIndex>::new();
        let mut record_ids = HashSet::new();
        let mut root_thread = None::<ThreadId>;
        let mut root_head = None::<ConversationEntryId>;
        let mut session_id = None::<SessionId>;
        let mut offset = 0_u64;
        let mut records = 0_usize;
        loop {
            let mut line = Vec::new();
            let line_len =
                reader
                    .read_until(b'\n', &mut line)
                    .map_err(|source| SessionError::Io {
                        path: path.to_owned(),
                        source,
                    })?;
            if line_len == 0 {
                break;
            }
            if line_len > MAX_RECORD_BYTES.saturating_add(1) {
                return Err(SessionError::RecordTooLarge {
                    offset,
                    actual: line_len.saturating_sub(1),
                    limit: MAX_RECORD_BYTES,
                });
            }
            let next_offset = offset.saturating_add(line_len as u64);
            if next_offset > MAX_SESSION_BYTES as u64 {
                return Err(SessionError::SessionTooLarge {
                    actual: next_offset,
                    limit: MAX_SESSION_BYTES,
                });
            }
            if line.last() != Some(&b'\n') {
                break;
            }
            line.pop();
            let envelope = decode_record(&line, offset)?;
            if !record_ids.insert(envelope.record_id) {
                return Err(SessionError::DuplicateRecordId {
                    record_id: envelope.record_id.to_string(),
                });
            }
            records = records.saturating_add(1);
            if records > MAX_SESSION_RECORDS {
                return Err(SessionError::TooManyRecords {
                    limit: MAX_SESSION_RECORDS,
                });
            }
            if records == 1 && !matches!(&envelope.record, SessionRecord::SessionCreated { .. }) {
                return Err(SessionError::InvalidCreationRecord);
            }
            if let Some(expected) = session_id {
                if envelope.session_id != expected {
                    return Err(SessionError::WrongSessionId);
                }
            } else {
                session_id = Some(envelope.session_id);
            }
            match envelope.record {
                SessionRecord::SessionCreated { thread_id, .. } => {
                    root_thread.get_or_insert(thread_id);
                }
                SessionRecord::ConversationEntryAppended { entry } => {
                    entries.insert(
                        entry.id,
                        ConversationEntryIndex {
                            parent: entry.parent,
                            offset,
                            line_len,
                        },
                    );
                }
                SessionRecord::ThreadHeadMoved { thread_id, head }
                    if Some(thread_id) == root_thread =>
                {
                    root_head = head;
                }
                _ => {}
            }
            offset = next_offset;
        }
        if root_thread.is_none() {
            return Err(SessionError::MissingCreationRecord);
        }
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = root_head;
        while let Some(entry_id) = cursor {
            if !seen.insert(entry_id) {
                return Err(SessionError::CorruptRecord {
                    offset: 0,
                    reason: "conversation ancestry contains a cycle".to_owned(),
                });
            }
            let entry =
                entries
                    .get(&entry_id)
                    .copied()
                    .ok_or_else(|| SessionError::CorruptRecord {
                        offset: 0,
                        reason: format!("conversation head references missing entry {entry_id}"),
                    })?;
            chain.push(entry);
            cursor = entry.parent;
        }
        chain.reverse();
        let total = chain.len();
        let end = before.unwrap_or(total).min(total);
        let start = end.saturating_sub(limit);
        let mut file = reader.into_inner();
        let mut messages = Vec::with_capacity(end - start);
        for entry in &chain[start..end] {
            file.seek(SeekFrom::Start(entry.offset))
                .map_err(|source| SessionError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            let mut line = vec![0_u8; entry.line_len];
            file.read_exact(&mut line)
                .map_err(|source| SessionError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            let envelope = decode_record(&line, entry.offset)?;
            let SessionRecord::ConversationEntryAppended { entry } = envelope.record else {
                return Err(SessionError::CorruptRecord {
                    offset: entry.offset,
                    reason: "indexed conversation record changed kind".to_owned(),
                });
            };
            messages.push(entry.message);
        }
        Ok(ConversationPage {
            messages,
            start,
            total,
            has_older: start > 0,
        })
    }

    pub(crate) fn open_for_resume(
        path: &Path,
        loaded: LoadedSession,
    ) -> Result<Self, SessionError> {
        // Revalidation and torn-tail repair must be serialized with writers;
        // otherwise recovery could truncate a concurrently committed append.
        let writer_lock = acquire_writer_lock(path)?;
        let bytes = read_session(path)?;
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
        let committed_bytes = if let Some(repair) = loaded.repair {
            file.set_len(repair.truncate_to)
                .map_err(|source| SessionError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            repair.truncate_to
        } else {
            loaded.inspected_len
        };
        file.seek(SeekFrom::End(0))
            .map_err(|source| SessionError::Io {
                path: path.to_owned(),
                source,
            })?;
        Ok(Self {
            session_id,
            path: path.to_owned(),
            file,
            _writer_lock: writer_lock,
            committed_bytes,
            committed_records: loaded.records.len(),
            writable: true,
        })
    }

    pub(crate) fn append(&mut self, record: &RecordEnvelope) -> Result<(), SessionError> {
        if !self.writable {
            return Err(SessionError::WriterPoisoned {
                path: self.path.clone(),
            });
        }
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
        let next_records = self.committed_records.saturating_add(1);
        if next_records > MAX_SESSION_RECORDS {
            return Err(SessionError::TooManyRecords {
                limit: MAX_SESSION_RECORDS,
            });
        }
        let next_bytes = self.committed_bytes.saturating_add(encoded.len() as u64);
        if next_bytes > MAX_SESSION_BYTES as u64 {
            return Err(SessionError::SessionTooLarge {
                actual: next_bytes,
                limit: MAX_SESSION_BYTES,
            });
        }
        if let Err(source) = self
            .file
            .write_all(&encoded)
            .and_then(|()| self.file.flush())
        {
            // A failed write may have left an uncommitted physical tail. Do
            // not append behind it and turn recoverable tail damage into
            // interior corruption.
            self.writable = false;
            return Err(SessionError::Io {
                path: self.path.clone(),
                source,
            });
        }
        self.committed_bytes = next_bytes;
        self.committed_records = next_records;
        Ok(())
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn read_session(path: &Path) -> Result<Vec<u8>, SessionError> {
    bounded_file::read(path, MAX_SESSION_BYTES).map_err(|error| match error {
        bounded_file::BoundedReadError::TooLarge { actual, limit, .. } => {
            SessionError::SessionTooLarge { actual, limit }
        }
        bounded_file::BoundedReadError::Io { path, source } => SessionError::Io { path, source },
    })
}

fn acquire_writer_lock(session_path: &Path) -> Result<fs::File, SessionError> {
    let lock_path = session_path.with_extension("jsonl.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| SessionError::Io {
            path: lock_path.clone(),
            source,
        })?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(fs::TryLockError::WouldBlock) => Err(SessionError::WriterBusy {
            path: session_path.to_owned(),
        }),
        Err(fs::TryLockError::Error(source)) => Err(SessionError::Io {
            path: lock_path,
            source,
        }),
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
    WriterBusy {
        path: PathBuf,
    },
    WriterPoisoned {
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
            Self::WriterBusy { path } => write!(
                formatter,
                "session {} already has an active writer or recovery controller",
                path.display()
            ),
            Self::WriterPoisoned { path } => write!(
                formatter,
                "session {} cannot accept another append after a prior I/O failure",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{AgentId, ConversationEntryId, ThreadId},
        message::{ContentBlock, Message, Role},
        session::ConversationEntry,
    };

    fn creation(session_id: SessionId) -> RecordEnvelope {
        RecordEnvelope::new(
            session_id,
            SessionRecord::SessionCreated {
                thread_id: ThreadId::new(),
                workspace_root: PathBuf::from("/workspace"),
            },
        )
    }

    #[test]
    fn append_enforces_active_session_record_and_byte_limits() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let mut store =
            SessionStore::create(directory.path(), creation(session_id)).expect("session store");
        let before = fs::read(store.path()).expect("committed bytes");

        store.committed_records = MAX_SESSION_RECORDS;
        assert!(matches!(
            store.append(&creation(session_id)),
            Err(SessionError::TooManyRecords { .. })
        ));

        store.committed_records = 1;
        store.committed_bytes = MAX_SESSION_BYTES as u64;
        assert!(matches!(
            store.append(&creation(session_id)),
            Err(SessionError::SessionTooLarge { .. })
        ));
        assert_eq!(fs::read(store.path()).expect("unchanged bytes"), before);
    }

    #[test]
    fn conversation_pages_index_metadata_then_read_only_requested_messages() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let session_id = SessionId::new();
        let thread_id = ThreadId::new();
        let created = RecordEnvelope::new(
            session_id,
            SessionRecord::SessionCreated {
                thread_id,
                workspace_root: PathBuf::from("/workspace"),
            },
        );
        let mut store = SessionStore::create(directory.path(), created).expect("session store");
        let agent_id = AgentId::new();
        let mut parent = None;
        for index in 0..300 {
            let id = ConversationEntryId::new();
            store
                .append(&RecordEnvelope::new(
                    session_id,
                    SessionRecord::ConversationEntryAppended {
                        entry: ConversationEntry {
                            id,
                            parent,
                            agent_id,
                            message: Message::text(Role::User, format!("message {index}")),
                        },
                    },
                ))
                .expect("append conversation entry");
            store
                .append(&RecordEnvelope::new(
                    session_id,
                    SessionRecord::ThreadHeadMoved {
                        thread_id,
                        head: Some(id),
                    },
                ))
                .expect("move thread head");
            parent = Some(id);
        }

        let latest = SessionStore::conversation_page(store.path(), None, 128).expect("latest page");
        assert_eq!(latest.total, 300);
        assert_eq!(latest.start, 172);
        assert_eq!(latest.messages.len(), 128);
        assert!(latest.has_older);
        assert!(matches!(
            latest.messages[0].content.as_slice(),
            [ContentBlock::Text(text)] if text == "message 172"
        ));

        let older = SessionStore::conversation_page(store.path(), Some(latest.start), 128)
            .expect("older page");
        assert_eq!(older.start, 44);
        assert_eq!(older.messages.len(), 128);
        assert!(matches!(
            older.messages[0].content.as_slice(),
            [ContentBlock::Text(text)] if text == "message 44"
        ));
    }
}
