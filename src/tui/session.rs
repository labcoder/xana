//! Bounded session projections and workspace-local rail preference.

use crate::{
    bounded_file,
    message::{ContentBlock, Message, Role},
    workspace_host::{
        ConversationProjection, ConversationRef, ConversationState, WorkspaceSnapshot,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    io,
    io::Write,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const MAX_VISIBLE_SESSIONS: usize = 512;
const MAX_SESSION_TEXT_BYTES: usize = 160;
const MAX_PREFERENCE_BYTES: usize = 32 * 1024;
const PREFERENCE_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionRow {
    pub(super) conversation: ConversationRef,
    pub(super) title: String,
    pub(super) execution_owner: &'static str,
    pub(super) connection: String,
    pub(super) model: String,
    pub(super) state: ConversationState,
    pub(super) modified_unix: Option<u64>,
    pub(super) record_count: Option<usize>,
    pub(super) selected: bool,
    pub(super) unread: bool,
    pub(super) error: bool,
}

impl SessionRow {
    fn from_projection(
        projection: ConversationProjection,
        runtime: &ConversationRef,
        current_connection: &str,
        current_model: &str,
    ) -> Self {
        let is_runtime = projection.conversation == *runtime;
        let (title, execution_owner, connection) = match &projection.conversation {
            ConversationRef::Native { session_id } => (
                format!("Native {}", short(&session_id.to_string())),
                "native",
                if is_runtime {
                    current_connection.to_owned()
                } else {
                    "native".to_owned()
                },
            ),
            ConversationRef::Managed {
                connection,
                thread_id,
            } => (
                format!("{} {}", connection, short(thread_id)),
                "managed",
                connection.clone(),
            ),
            ConversationRef::NewNative => (
                "New native conversation".to_owned(),
                "native",
                current_connection.to_owned(),
            ),
            ConversationRef::NewManaged { connection } => (
                format!("New {connection} conversation"),
                "managed",
                connection.clone(),
            ),
        };
        Self {
            conversation: projection.conversation,
            title: bounded(title),
            execution_owner,
            connection: bounded(connection),
            model: if is_runtime {
                bounded(current_model.to_owned())
            } else {
                "not retained".to_owned()
            },
            state: projection.state,
            modified_unix: projection
                .modified
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_secs()),
            record_count: projection.record_count,
            selected: projection.selected || is_runtime,
            unread: false,
            error: false,
        }
    }
}

pub(super) fn project(
    snapshot: WorkspaceSnapshot,
    runtime: &ConversationRef,
    current_connection: &str,
    current_model: &str,
) -> Vec<SessionRow> {
    snapshot
        .conversations
        .into_iter()
        .take(MAX_VISIBLE_SESSIONS)
        .map(|projection| {
            SessionRow::from_projection(projection, runtime, current_connection, current_model)
        })
        .collect()
}

pub(super) fn preview_title(messages: &[Message], fallback: &str) -> String {
    messages
        .iter()
        .find_map(|message| {
            (message.role == Role::User)
                .then(|| {
                    message.content.iter().find_map(|block| match block {
                        ContentBlock::Text(text) => Some(bounded(
                            text.split_whitespace().collect::<Vec<_>>().join(" "),
                        )),
                        _ => None,
                    })
                })
                .flatten()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| bounded(fallback.to_owned()))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPreferences {
    version: u16,
    rail_expanded: bool,
}

impl Default for SessionPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCE_VERSION,
            rail_expanded: true,
        }
    }
}

pub(super) struct SessionPreferenceStore {
    path: PathBuf,
    preferences: SessionPreferences,
}

impl SessionPreferenceStore {
    pub(super) fn load(frontend_dir: &Path, workspace: &Path) -> Self {
        let key = blake3::hash(workspace.as_os_str().as_encoded_bytes()).to_hex();
        let path = frontend_dir.join("workspaces").join(format!("{key}.toml"));
        let preferences = bounded_file::read(&path, MAX_PREFERENCE_BYTES)
            .ok()
            .and_then(|bytes| toml::from_slice::<SessionPreferences>(&bytes).ok())
            .filter(|preferences| preferences.version == PREFERENCE_VERSION)
            .unwrap_or_default();
        Self { path, preferences }
    }

    pub(super) fn rail_expanded(&self) -> bool {
        self.preferences.rail_expanded
    }

    pub(super) fn set_rail_expanded(&mut self, expanded: bool) -> io::Result<()> {
        self.preferences.rail_expanded = expanded;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rendered = toml::to_string_pretty(&self.preferences).map_err(io::Error::other)?;
        let mut file = atomic_write_file::AtomicWriteFile::open(&self.path)?;
        file.write_all(rendered.as_bytes())?;
        file.commit()
    }
}

fn short(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn bounded(mut value: String) -> String {
    if value.len() <= MAX_SESSION_TEXT_BYTES {
        return value;
    }
    let mut end = MAX_SESSION_TEXT_BYTES - 3;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("...");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workspace_preference_round_trips_without_runtime_truth() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut store = SessionPreferenceStore::load(directory.path(), &workspace);
        assert!(store.rail_expanded());
        store.set_rail_expanded(false).unwrap();
        assert!(!SessionPreferenceStore::load(directory.path(), &workspace).rail_expanded());
        let stored = std::fs::read_to_string(store.path).unwrap();
        assert!(!stored.contains("active"));
        assert!(!stored.contains("session"));
    }

    #[test]
    fn title_preview_is_bounded_and_uses_only_user_text() {
        let messages = vec![
            Message::text(Role::Assistant, "ignore"),
            Message::text(Role::User, "a ".repeat(200)),
        ];
        let title = preview_title(&messages, "fallback");
        assert!(title.len() <= MAX_SESSION_TEXT_BYTES);
        assert!(!title.contains("ignore"));
    }
}
