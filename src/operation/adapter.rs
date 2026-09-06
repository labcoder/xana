//! Durable adapter correlation is native admission metadata, not another journal.

use crate::{
    identity::{OperationId, SessionId},
    message::Message,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persist this opaque token before submission. It identifies one exact input;
/// it grants no authority and is not proof that admission or execution occurred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopCommandKey {
    version: u16,
    command: Uuid,
    conversation: Uuid,
    namespace: Uuid,
    store: Option<Uuid>,
    profile: String,
    payload: String,
}

impl DesktopCommandKey {
    pub fn command_id(&self) -> Uuid {
        self.command
    }
    pub fn conversation_id(&self) -> Uuid {
        self.conversation
    }
    pub(crate) fn operation(&self) -> OperationId {
        self.command.to_string().parse().expect("UUID operation")
    }
    pub(crate) fn session(&self) -> SessionId {
        self.conversation
            .to_string()
            .parse()
            .expect("UUID Conversation")
    }
    pub(crate) fn new(
        namespace: Uuid,
        session: SessionId,
        store: Option<Uuid>,
        profile: String,
        message: &Message,
    ) -> Result<Self> {
        let value = Self {
            version: 1,
            command: Uuid::new_v4(),
            conversation: session.to_string().parse()?,
            namespace,
            store,
            profile,
            payload: message_digest(message)?,
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported adapter command version");
        ensure!(
            !self.command.is_nil() && !self.conversation.is_nil() && !self.namespace.is_nil(),
            "adapter command identity is invalid"
        );
        ensure!(
            [&self.profile, &self.payload]
                .iter()
                .all(|hash| hash.len() == 64
                    && hash
                        .bytes()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
            "adapter command digest is invalid"
        );
        Ok(())
    }
    pub(crate) fn matches_scope(
        &self,
        namespace: Uuid,
        session: SessionId,
        store: Option<Uuid>,
        profile: &str,
    ) -> bool {
        self.namespace == namespace
            && self.session() == session
            && self.store == store
            && self.profile == profile
    }
    pub(crate) fn matches_owner(
        &self,
        session: SessionId,
        store: Option<Uuid>,
        profile: &str,
    ) -> bool {
        self.session() == session && self.store == store && self.profile == profile
    }
    pub(crate) fn matches_message(&self, message: &Message) -> Result<bool> {
        Ok(self.payload == message_digest(message)?)
    }
}

/// Stable committed message identity; repeated delivery can deduplicate by this
/// reference. Its digest covers the typed message, including artifact references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopCommandResultRef {
    pub entry_id: Uuid,
    pub message_digest: String,
}

pub(crate) fn message_digest(message: &Message) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(message)?)
        .to_hex()
        .to_string())
}
