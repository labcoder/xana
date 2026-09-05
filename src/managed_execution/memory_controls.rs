//! Locally acknowledged owner controls never become a Codex model turn.
use crate::{identity::ConversationId, memory::MemoryOwner};

pub(super) async fn local_memory_reply(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
    input: &str,
) -> Result<String, String> {
    let mut owner = owner.cloned().ok_or_else(|| {
        "Personal memory requires an unlocked protected home; no plaintext memory was created"
            .to_owned()
    })?;
    owner.context.conversation = Some(
        conversation
            .to_string()
            .parse()
            .map_err(|_| "invalid Conversation identity")?,
    );
    let input = input.to_owned();
    tokio::task::spawn_blocking(move || {
        owner
            .respond(&input)
            .ok_or_else(|| "Unknown local memory control".to_owned())?
            .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|_| "Memory control stopped; inspect the record before retrying".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryContext, MemoryScope},
        storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    };
    #[tokio::test]
    async fn memory_managed_control_uses_current_conversation_and_no_vendor_runtime() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let owner = MemoryOwner::new(
            store,
            MemoryContext {
                conversation: Some(uuid::Uuid::new_v4()),
                ..Default::default()
            },
        );
        let conversation = ConversationId::new();
        let reply = local_memory_reply(
            Some(&owner),
            conversation,
            "remember that I prefer concise replies",
        )
        .await
        .unwrap();
        assert!(reply.contains("no model call"));
        let record = owner.page(None, None).unwrap().records.remove(0);
        assert_eq!(
            record.scope,
            MemoryScope::Conversation(conversation.to_string().parse().unwrap())
        );
        assert!(
            local_memory_reply(None, conversation, "show my memories")
                .await
                .is_err()
        );
        assert!(
            local_memory_reply(Some(&owner), conversation, "ordinary question")
                .await
                .is_err()
        );
    }
}
