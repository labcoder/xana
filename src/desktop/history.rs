//! Read-only saved history, independent of live controller and draft state.

use super::{DesktopError, DesktopErrorCode, DesktopMessage, DesktopSnapshot, content};
use crate::{
    config::XanaConfig,
    identity::SessionId,
    session::{ConversationPage, SessionStore},
    storage::ProtectedStore,
};
use anyhow::{Result, ensure};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// A cursor is bound to a selected Conversation and an exact immutable prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHistoryCursor {
    session: SessionId,
    generation: u64,
    total: usize,
    tail: Option<String>,
    boundary: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopHistoryRequest {
    Latest { before: Option<usize> },
    Older(DesktopHistoryCursor),
    Newer(DesktopHistoryCursor),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHistoryPage {
    pub session_id: String,
    pub messages: Vec<DesktopMessage>,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub older: Option<DesktopHistoryCursor>,
    pub newer: Option<DesktopHistoryCursor>,
}

#[derive(Default)]
struct Selection {
    native_session: Option<SessionId>,
    generation: u64,
}

/// Blocking, bounded storage I/O. Call on a background executor, never render.
/// Clones share revocable custody and selected-Conversation fencing, not keys.
#[derive(Clone)]
pub struct DesktopHistoryReader {
    protected: Option<ProtectedStore>,
    data_dir: PathBuf,
    workspace: PathBuf,
    config_file: PathBuf,
    selection: Arc<Mutex<Selection>>,
}

impl DesktopHistoryReader {
    pub(super) fn new(
        protected: Option<ProtectedStore>,
        data_dir: PathBuf,
        workspace: PathBuf,
        config_file: PathBuf,
    ) -> Self {
        Self {
            protected,
            data_dir,
            workspace,
            config_file,
            selection: Arc::default(),
        }
    }

    pub(super) fn select(&self, snapshot: Option<&DesktopSnapshot>) {
        let native_session = snapshot
            .filter(|snapshot| snapshot.execution_owner == "native")
            .and_then(|snapshot| snapshot.session_id.parse().ok());
        if let Ok(mut selection) = self.selection.lock()
            && selection.native_session != native_session
        {
            selection.native_session = native_session;
            selection.generation = selection.generation.saturating_add(1);
        }
    }

    pub fn page(
        &self,
        session_id: &str,
        request: DesktopHistoryRequest,
    ) -> Result<DesktopHistoryPage, DesktopError> {
        let session = session_id.parse::<SessionId>().map_err(invalid)?;
        let generation = {
            let selection = self.selection.lock().map_err(invalid)?;
            if selection.native_session.is_none() {
                return Err(DesktopError::new(
                    DesktopErrorCode::UnsupportedExecutionOwner,
                    "saved native history is unavailable for this execution owner",
                ));
            }
            ensure_selected(&selection, session, None).map_err(invalid)?;
            selection.generation
        };
        let (anchor, before, from) = match request {
            DesktopHistoryRequest::Latest { before } => (None, before, None),
            DesktopHistoryRequest::Older(cursor) => {
                ensure_cursor(&cursor, session, generation).map_err(invalid)?;
                (
                    Some((cursor.total, cursor.tail)),
                    Some(cursor.boundary),
                    None,
                )
            }
            DesktopHistoryRequest::Newer(cursor) => {
                ensure_cursor(&cursor, session, generation).map_err(invalid)?;
                (
                    Some((cursor.total, cursor.tail)),
                    None,
                    Some(cursor.boundary),
                )
            }
        };
        let (page, tail) = if let Some(store) = &self.protected {
            store.history_page_anchored(session, &self.workspace, anchor, before, from)
        } else {
            self.legacy_page(session, anchor, before, from)
        }
        .map_err(invalid)?;
        let policy = XanaConfig::load_registry_from(&self.config_file)
            .map_err(invalid)?
            .resources;
        let end = page.start + page.messages.len();
        let cursor = |boundary| DesktopHistoryCursor {
            session,
            generation,
            total: page.total,
            tail: tail.clone(),
            boundary,
        };
        let messages = page
            .messages
            .iter()
            .enumerate()
            .map(|(offset, message)| {
                content::project_message(
                    format!("{session}:history:{}", page.start + offset),
                    message,
                    &policy,
                )
            })
            .collect();
        let selection = self.selection.lock().map_err(invalid)?;
        ensure_selected(&selection, session, Some(generation)).map_err(invalid)?;
        Ok(DesktopHistoryPage {
            session_id: session.to_string(),
            messages,
            start: page.start,
            end,
            total: page.total,
            older: (page.start > 0).then(|| cursor(page.start)),
            newer: (end < page.total).then(|| cursor(end)),
        })
    }

    // Legacy storage retains its existing 10k-record/16MiB full-inspection cap.
    // Protected history never falls back to this materializing compatibility path.
    fn legacy_page(
        &self,
        session: SessionId,
        anchor: Option<(usize, Option<String>)>,
        before: Option<usize>,
        from: Option<usize>,
    ) -> Result<(ConversationPage, Option<String>)> {
        let path = SessionStore::path_for(&self.data_dir.join("sessions"), session);
        let loaded = SessionStore::inspect(&path)?;
        let state = crate::session::reduce(&loaded.records)?;
        ensure!(
            state.session_id == session && state.workspace_root == self.workspace,
            "Conversation workspace or identity differs"
        );
        let entries = state.conversation_entry_path()?;
        let total = anchor.as_ref().map_or(entries.len(), |anchor| anchor.0);
        ensure!(total <= entries.len(), "saved history is no longer active");
        let tail = total
            .checked_sub(1)
            .map(|index| entries[index].id.to_string());
        ensure!(
            anchor.is_none_or(|anchor| anchor.1 == tail),
            "saved history changed; return to Live and reopen history"
        );
        let forward = from.is_some();
        let high = if let Some(from) = from {
            from.min(total).saturating_add(128).min(total)
        } else {
            before.unwrap_or(total).min(total)
        };
        let low = from.map_or_else(|| high.saturating_sub(128), |from| from.min(total));
        let mut messages = Vec::new();
        let mut bytes = 0usize;
        for offset in 0..high - low {
            let index = if forward {
                low + offset
            } else {
                high - offset - 1
            };
            let message = &entries[index].message;
            bytes = bytes.saturating_add(serde_json::to_vec(message)?.len());
            if bytes > 2 * 1024 * 1024 {
                break;
            }
            messages.push(message.clone());
        }
        ensure!(
            high == low || !messages.is_empty(),
            "saved entry exceeds the page byte limit"
        );
        let start = if forward { low } else { high - messages.len() };
        if !forward {
            messages.reverse();
        }
        Ok((
            ConversationPage {
                messages,
                start,
                total,
                has_older: start > 0,
            },
            tail,
        ))
    }
}

fn ensure_selected(
    selection: &Selection,
    session: SessionId,
    generation: Option<u64>,
) -> Result<()> {
    ensure!(
        selection.native_session == Some(session)
            && generation.is_none_or(|generation| generation == selection.generation),
        "Conversation selection changed; return to Live and reopen history"
    );
    Ok(())
}

fn ensure_cursor(cursor: &DesktopHistoryCursor, session: SessionId, generation: u64) -> Result<()> {
    ensure!(
        cursor.session == session && cursor.generation == generation,
        "saved history cursor belongs to another selection"
    );
    Ok(())
}

fn invalid(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(DesktopErrorCode::StateInvalid, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        message::{Message, Role},
        session::DurableSession,
        storage::{RecoveryIdentity, TestCustody},
    };

    struct Fixture {
        _temp: tempfile::TempDir,
        session: DurableSession,
        id: SessionId,
        reader: DesktopHistoryReader,
    }

    impl Fixture {
        fn new(protected: bool) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let data = temp.path().join("data");
            let workspace = temp.path().canonicalize().unwrap();
            let id = SessionId::new();
            let store = protected.then(|| {
                ProtectedStore::initialize(
                    &data,
                    &RecoveryIdentity::generate(),
                    &TestCustody::default(),
                )
                .unwrap()
            });
            let session = if let Some(store) = &store {
                DurableSession::create_protected(store.clone(), workspace.clone(), id).unwrap()
            } else {
                DurableSession::create_with_id(&data, workspace.clone(), id).unwrap()
            };
            let config_file = temp.path().join("config.toml");
            std::fs::write(
                &config_file,
                r#"
version = 5
default_profile = "default"
permission_mode = "ask"
[providers.local]
kind = "ollama"
[profiles.default]
connection = "local"
model = "fixture"
"#,
            )
            .unwrap();
            let reader = DesktopHistoryReader::new(store, data, workspace, config_file);
            *reader.selection.lock().unwrap() = Selection {
                native_session: Some(id),
                generation: 1,
            };
            Self {
                _temp: temp,
                session,
                id,
                reader,
            }
        }
        fn append(&mut self, index: usize) {
            self.session
                .append_message(Message::text(Role::User, format!("{index}: 日本語 🦀")))
                .unwrap();
        }
        fn page(&self, request: DesktopHistoryRequest) -> DesktopHistoryPage {
            self.reader.page(&self.id.to_string(), request).unwrap()
        }
    }

    #[test]
    fn saved_history_pages_are_exact_append_stable_and_clear_invalidates() {
        for protected in [true, false] {
            let mut fixture = Fixture::new(protected);
            for index in 0..300 {
                fixture.append(index);
            }
            let latest = fixture.page(DesktopHistoryRequest::Latest { before: None });
            assert_eq!((latest.start, latest.end, latest.total), (172, 300, 300));
            assert_eq!(latest.messages.len(), 128);
            let older = fixture.page(DesktopHistoryRequest::Older(latest.older.clone().unwrap()));
            assert_eq!((older.start, older.end), (44, 172));
            assert_eq!(older.messages[0].id, format!("{}:history:44", fixture.id));
            fixture.append(300);
            let newer = fixture.page(DesktopHistoryRequest::Newer(older.newer.unwrap()));
            assert_eq!((newer.start, newer.end, newer.total), (172, 300, 300));
            assert_eq!(newer.messages, latest.messages);
            let oldest = fixture.page(DesktopHistoryRequest::Older(older.older.unwrap()));
            assert_eq!((oldest.start, oldest.end), (0, 44));
            assert!(oldest.older.is_none());
            fixture.session.clear_conversation().unwrap();
            assert!(
                fixture
                    .reader
                    .page(
                        &fixture.id.to_string(),
                        DesktopHistoryRequest::Older(latest.older.unwrap())
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn history_reader_enforces_workspace_selection_and_vendor_boundary() {
        let mut fixture = Fixture::new(true);
        for index in 0..140 {
            fixture.append(index);
        }
        let saved = fixture.page(DesktopHistoryRequest::Latest { before: Some(128) });
        assert_eq!((saved.start, saved.end, saved.total), (0, 128, 140));
        assert!(
            fixture
                .reader
                .page(
                    &SessionId::new().to_string(),
                    DesktopHistoryRequest::Latest { before: None }
                )
                .is_err()
        );
        let mut wrong_workspace = fixture.reader.clone();
        wrong_workspace.workspace = fixture.reader.workspace.join("different");
        assert!(
            wrong_workspace
                .page(
                    &fixture.id.to_string(),
                    DesktopHistoryRequest::Latest { before: None }
                )
                .is_err()
        );
        fixture.reader.selection.lock().unwrap().generation += 1;
        assert!(
            fixture
                .reader
                .page(
                    &fixture.id.to_string(),
                    DesktopHistoryRequest::Newer(saved.newer.unwrap())
                )
                .is_err()
        );
        fixture.reader.select(None);
        assert_eq!(
            fixture
                .reader
                .page(
                    &fixture.id.to_string(),
                    DesktopHistoryRequest::Latest { before: None }
                )
                .unwrap_err()
                .code,
            DesktopErrorCode::UnsupportedExecutionOwner
        );
    }
}
