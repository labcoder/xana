//! Read-only native command reconciliation; no provider, tool, or replay path.

use super::{
    DesktopAttachment, DesktopClient, DesktopError, DesktopErrorCode, DesktopLaunch,
    DesktopSnapshot,
};
use crate::{
    identity::{OperationId, SessionId},
    message::{Message, Role},
    paths::XanaPaths,
    session::{DurableSession, SessionStore},
    storage::ProtectedStore,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub use crate::operation::adapter::{DesktopCommandKey, DesktopCommandResultRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCommandState {
    Completed,
    Failed,
    Declined,
    Interrupted,
    Suspended,
    Pending,
    Unknown,
    NotFound,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopCommandUnavailable {
    ScopeChanged,
    Locked,
    CorruptOrIncompatible,
    HistoryMissing,
    UnsupportedOwner,
}

/// An outcome is an observation, never permission to replay. No delivery or
/// external-effect exactly-once promise follows from a committed native result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopCommandOutcome {
    pub state: DesktopCommandState,
    pub result: Option<DesktopCommandResultRef>,
    pub unavailable: Option<DesktopCommandUnavailable>,
}

impl DesktopCommandOutcome {
    fn state(state: DesktopCommandState) -> Self {
        Self {
            state,
            result: None,
            unavailable: None,
        }
    }
    fn unavailable(reason: DesktopCommandUnavailable) -> Self {
        Self {
            state: DesktopCommandState::Unavailable,
            result: None,
            unavailable: Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Selection {
    session: SessionId,
    native: bool,
    active: Option<OperationId>,
    revision: u64,
}

#[derive(Default)]
struct SelectionState {
    revision: u64,
    current: Option<Selection>,
    closed: bool,
}

impl SelectionState {
    fn select(&mut self, next: Option<(SessionId, bool, Option<OperationId>)>) {
        if self.closed {
            return;
        }
        let changed = match (&self.current, &next) {
            (Some(old), Some((session, native, _))) => {
                old.session != *session || old.native != *native
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        self.current = next.map(|(session, native, active)| Selection {
            session,
            native,
            active,
            revision: self.revision,
        });
    }
    fn revoke(&mut self) {
        self.select(None);
        self.closed = true;
    }
}

/// Clones share revocable selected-Conversation custody. Blocking bounded reads
/// belong on an adapter's background executor, not its render/audio callback.
#[derive(Clone)]
pub struct DesktopCommandOutcomes {
    paths: XanaPaths,
    workspace: PathBuf,
    protected: Option<ProtectedStore>,
    namespace: Uuid,
    selection: Arc<Mutex<SelectionState>>,
}

impl DesktopCommandOutcomes {
    pub(super) fn new(
        paths: XanaPaths,
        workspace: PathBuf,
        protected: Option<ProtectedStore>,
    ) -> Self {
        Self {
            paths,
            workspace,
            protected,
            namespace: Uuid::nil(),
            selection: Arc::default(),
        }
    }
    pub(super) fn select(&self, snapshot: Option<&DesktopSnapshot>) {
        if let Ok(mut selection) = self.selection.lock() {
            let next = snapshot.and_then(|snapshot| {
                snapshot.session_id.parse().ok().map(|session| {
                    (
                        session,
                        snapshot.execution_owner == "native",
                        snapshot.active_operation.map(|id| id.0),
                    )
                })
            });
            selection.select(next);
        }
    }
    pub(super) fn with_namespace(&self, namespace: Uuid) -> Self {
        Self {
            namespace,
            ..self.clone()
        }
    }
    pub(super) fn revoke(&self) {
        if let Ok(mut selection) = self.selection.lock() {
            selection.revoke();
        }
    }

    /// Creates only a token. Save it before calling `submit_correlated`; no
    /// durable admission or work occurs here. Namespace partitions correlation,
    /// not authority: access still belongs to the authenticated local owner.
    pub fn prepare(
        &self,
        input: &str,
        attachments: &[DesktopAttachment],
    ) -> Result<DesktopCommandKey, DesktopError> {
        let selected = self.selected().map_err(error)?;
        if !selected.native {
            return Err(error("managed command outcomes are unavailable"));
        }
        let message = input_message(input, attachments)?;
        let profile = DurableSession::adapter_profile_digest(
            self.paths.data_dir(),
            self.protected.as_ref(),
            selected.session,
        )
        .map_err(error)?;
        DesktopCommandKey::new(
            self.namespace,
            selected.session,
            self.protected.as_ref().map(ProtectedStore::id),
            profile,
            &message,
        )
        .map_err(error)
    }

    /// Exact lookup never opens a session writer or dispatches work. NotFound
    /// says only this retained history has no admission, not that retry is safe.
    pub fn lookup(&self, key: &DesktopCommandKey) -> DesktopCommandOutcome {
        let selected = match self.selected() {
            Ok(value) => value,
            Err(_) => {
                return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::ScopeChanged);
            }
        };
        if !selected.native {
            return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::UnsupportedOwner);
        }
        match ProtectedStore::status(self.paths.data_dir()) {
            Ok(crate::storage::StorageStatus::Protected { locked: true, .. }) => {
                return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::Locked);
            }
            Ok(crate::storage::StorageStatus::Protected { id, .. })
                if self.protected.as_ref().is_none_or(|home| home.id() != id) =>
            {
                return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::ScopeChanged);
            }
            Ok(_) => {}
            Err(_) => {
                return DesktopCommandOutcome::unavailable(
                    DesktopCommandUnavailable::CorruptOrIncompatible,
                );
            }
        }
        let profile = match DurableSession::adapter_profile_digest(
            self.paths.data_dir(),
            self.protected.as_ref(),
            selected.session,
        ) {
            Ok(value) => value,
            Err(_) => {
                return DesktopCommandOutcome::unavailable(
                    DesktopCommandUnavailable::CorruptOrIncompatible,
                );
            }
        };
        if key.validate().is_err() {
            return DesktopCommandOutcome::unavailable(
                DesktopCommandUnavailable::CorruptOrIncompatible,
            );
        }
        if !key.matches_scope(
            self.namespace,
            selected.session,
            self.protected.as_ref().map(ProtectedStore::id),
            &profile,
        ) {
            return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::ScopeChanged);
        }
        let found = self.lookup_inner(key, &selected);
        if self.selected().map_or(true, |current| {
            current.session != selected.session || current.revision != selected.revision
        }) {
            return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::ScopeChanged);
        }
        // The final held-capability read is the disclosure linearization point.
        // A revoked handle cannot be revived by opening a new unlocked owner.
        match DurableSession::adapter_profile_digest(
            self.paths.data_dir(),
            self.protected.as_ref(),
            selected.session,
        ) {
            Ok(current) if current == profile => {}
            Ok(_) => {
                return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::ScopeChanged);
            }
            Err(_) => return DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::Locked),
        }
        found.unwrap_or_else(|_| {
            DesktopCommandOutcome::unavailable(DesktopCommandUnavailable::CorruptOrIncompatible)
        })
    }

    /// Materialize only the exact committed result, never search transcript
    /// prose. The returned resource references retain ordinary Desktop policy.
    pub fn read_result(
        &self,
        key: &DesktopCommandKey,
    ) -> Result<super::DesktopMessage, DesktopError> {
        let outcome = self.lookup(key);
        let reference = outcome
            .result
            .ok_or_else(|| error("command has no available committed result"))?;
        let entry_id = reference.entry_id.to_string().parse().map_err(error)?;
        let message = if let Some(home) = &self.protected {
            let records = home
                .history_records_for(
                    key.session(),
                    crate::storage::HistorySubject::Entry(entry_id),
                )
                .map_err(error)?;
            let [
                crate::session::RecordEnvelope {
                    record: crate::session::SessionRecord::ConversationEntryAppended { entry },
                    ..
                },
            ] = records.as_slice()
            else {
                return Err(error("command result record is missing or duplicated"));
            };
            entry.message.clone()
        } else {
            let path =
                SessionStore::path_for(&self.paths.data_dir().join("sessions"), key.session());
            let loaded = SessionStore::inspect(&path).map_err(error)?;
            crate::session::reduce(&loaded.records)
                .map_err(error)?
                .entries
                .remove(&entry_id)
                .ok_or_else(|| error("command result entry is unavailable"))?
                .message
        };
        if crate::operation::adapter::message_digest(&message).map_err(error)?
            != reference.message_digest
        {
            return Err(error("command result digest differs"));
        }
        let policy = crate::config::XanaConfig::load_registry_from(self.paths.config_file())
            .map_err(error)?
            .resources;
        let result =
            super::content::project_message(reference.entry_id.to_string(), &message, &policy);
        if self.lookup(key).result.as_ref() != Some(&reference) {
            return Err(error("command result scope changed before disclosure"));
        }
        Ok(result)
    }

    fn lookup_inner(
        &self,
        key: &DesktopCommandKey,
        selected: &Selection,
    ) -> anyhow::Result<DesktopCommandOutcome> {
        let operation = if let Some(home) = &self.protected {
            if !home.history_exists(selected.session)? {
                return Ok(DesktopCommandOutcome::unavailable(
                    DesktopCommandUnavailable::HistoryMissing,
                ));
            }
            anyhow::ensure!(
                home.history_metadata(selected.session)?.workspace == self.workspace,
                "workspace differs"
            );
            DurableSession::inspect_operation_protected(home, selected.session, key.operation())?
        } else {
            let path =
                SessionStore::path_for(&self.paths.data_dir().join("sessions"), selected.session);
            if !path.exists() {
                return Ok(DesktopCommandOutcome::unavailable(
                    DesktopCommandUnavailable::HistoryMissing,
                ));
            }
            let loaded = SessionStore::inspect(&path)?;
            let mut state = crate::session::reduce(&loaded.records)?;
            anyhow::ensure!(state.workspace_root == self.workspace, "workspace differs");
            state.operation_details.remove(&key.operation())
        };
        let Some(operation) = operation else {
            return Ok(DesktopCommandOutcome::state(DesktopCommandState::NotFound));
        };
        anyhow::ensure!(
            operation.adapter.as_ref() == Some(key),
            "correlation identity collision"
        );
        use crate::native_runtime::OperationOutcome as O;
        let state = match operation.finished {
            Some(O::Completed) => DesktopCommandState::Completed,
            Some(O::Failed) => DesktopCommandState::Failed,
            Some(O::Declined) => DesktopCommandState::Declined,
            Some(O::Interrupted) => DesktopCommandState::Interrupted,
            None if operation
                .suspensions
                .last()
                .is_some_and(|suspension| match suspension {
                    crate::operation::SuspensionReason::RoundBudgetReached(suspended) => operation
                        .round_budget_decisions
                        .last()
                        .is_none_or(|decision| {
                            decision.suspension_id != suspended.id
                                || decision.action
                                    != crate::native_runtime::RoundBudgetAction::Continue
                        }),
                    crate::operation::SuspensionReason::Permission => {
                        selected.active == Some(key.operation())
                    }
                    crate::operation::SuspensionReason::ProcessInterrupted => false,
                }) =>
            {
                DesktopCommandState::Suspended
            }
            None if selected.active == Some(key.operation()) => DesktopCommandState::Pending,
            None => DesktopCommandState::Unknown,
        };
        Ok(DesktopCommandOutcome {
            state,
            result: operation.adapter_result,
            unavailable: None,
        })
    }
    fn selected(&self) -> anyhow::Result<Selection> {
        self.selection
            .lock()
            .map_err(|_| anyhow::anyhow!("selection unavailable"))?
            .current
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no selected Conversation"))
    }
}

impl DesktopClient {
    pub fn command_outcomes(&self, namespace: Uuid) -> DesktopCommandOutcomes {
        self.command_outcomes.with_namespace(namespace)
    }
    pub fn submit_correlated(
        &self,
        key: DesktopCommandKey,
        input: impl Into<String>,
        attachments: Vec<DesktopAttachment>,
    ) -> Result<super::DesktopCommandReceipt, DesktopError> {
        let input = input.into();
        key.validate().map_err(error)?;
        if !key
            .matches_message(&input_message(&input, &attachments)?)
            .map_err(error)?
        {
            return Err(error("adapter command payload collision"));
        }
        let operation_id = super::DesktopOperationId(key.operation());
        let command_id = self.enqueue(super::BridgeCommandValue::Submit {
            operation_id,
            input,
            attachments,
            acknowledge_workspace_write_collision: false,
            correlation: Some(key),
        })?;
        Ok(super::DesktopCommandReceipt {
            command_id,
            operation_id: Some(operation_id),
        })
    }
}

impl DesktopLaunch {
    /// Explicitly select one native Conversation for read-only crash recovery.
    /// This does not launch a client, acquire a writer, or resolve a provider.
    pub fn command_outcomes(
        &self,
        namespace: Uuid,
        conversation: Uuid,
    ) -> Result<DesktopCommandOutcomes, DesktopError> {
        let paths = XanaPaths::resolve(self.xana_home.clone()).map_err(error)?;
        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| error("an explicit workspace is required"))?
            .canonicalize()
            .map_err(error)?;
        let protected = ProtectedStore::configured_inspection(paths.data_dir()).map_err(error)?;
        let reader =
            DesktopCommandOutcomes::new(paths, workspace, protected).with_namespace(namespace);
        reader.selection.lock().map_err(error)?.select(Some((
            conversation.to_string().parse().map_err(error)?,
            true,
            None,
        )));
        Ok(reader)
    }
}

fn input_message(input: &str, attachments: &[DesktopAttachment]) -> Result<Message, DesktopError> {
    if input.trim().is_empty() || input.len() > super::MAX_PUBLIC_TEXT_BYTES {
        return Err(error("adapter input is blank or too large"));
    }
    if attachments.len() > crate::vision::MAX_IMAGES_PER_TURN {
        return Err(error("adapter attachment count exceeds the turn limit"));
    }
    let images = super::validate_desktop_attachments(attachments.to_vec())?;
    let mut message = Message::text(Role::User, input);
    message
        .content
        .extend(images.into_iter().map(crate::message::ContentBlock::Image));
    Ok(message)
}
fn error(value: impl std::fmt::Display) -> DesktopError {
    DesktopError::new(DesktopErrorCode::StateInvalid, value.to_string())
}

#[cfg(test)]
mod tests;
