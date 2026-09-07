//! One supported text handoff in the real managed turn, never a bridge-agent call.
use crate::{identity::ConversationId, memory::MemoryOwner};

pub(super) type Maintenance = Option<tokio_util::task::AbortOnDropHandle<()>>;
pub(super) fn maintain(owner: Option<&MemoryOwner>, task: &mut Maintenance) {
    if task.as_ref().is_some_and(|task| !task.is_finished()) {
        return;
    }
    if let Some(worker) = owner.and_then(|owner| owner.learner.clone()) {
        *task = Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
            async move {
                // Coalesce short owner inputs without a helper request per message.
                // No background lane or usage reservation is held during this delay.
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let _ = worker
                    .process(false, &tokio_util::sync::CancellationToken::new())
                    .await;
            },
        )));
    }
}

pub(super) fn foreground(
    owner: Option<&MemoryOwner>,
) -> anyhow::Result<Option<crate::storage::ForegroundLease>> {
    owner
        .map(|owner| owner.store.foreground_lease())
        .transpose()
}

/// The launch snapshot's Project comes from explicit Conversation membership,
/// never merely from sharing a workspace. A new /clear Conversation is Ungrouped
/// until application composition resolves an explicit membership for it.
pub(super) fn for_conversation(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
) -> Option<MemoryOwner> {
    owner.map(|owner| {
        let mut owner = owner.clone();
        let conversation = conversation.as_uuid();
        if owner.context.conversation != Some(conversation) {
            owner.context.project = None;
        }
        owner.context.conversation = Some(conversation);
        owner
    })
}

#[cfg(test)]
pub(crate) async fn prepare(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
    input: &str,
) -> Result<String, String> {
    prepare_with_source(owner, conversation, input, uuid::Uuid::new_v4()).await
}

pub(super) async fn prepare_turn(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
    input: &crate::tool::OwnerTurnInput,
) -> Result<String, String> {
    prepare_with_source(owner, conversation, &input.text, input.source_id).await
}

async fn prepare_with_source(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
    input: &str,
    source_id: uuid::Uuid,
) -> Result<String, String> {
    let Some(owner) = for_conversation(owner, conversation) else {
        return Ok(with_readiness(
            crate::memory::MemoryReadiness::Unavailable,
            input,
        ));
    };
    let input = input.to_owned();
    tokio::task::spawn_blocking(move ||->anyhow::Result<String> {
        // Codex owns its actual context window. 16,384 is Xana's conservative
        // unknown-window allowance, not a claim about a selected vendor model.
        let selection=owner.select_for_turn(&input,16_384)?;
        let (text,ids)=selection.managed_text(&input,16_384)?;
        owner.record_selection(&selection,&ids)?;
        // Only this owner-input edge can enqueue; vendor output never does.
        if owner.enqueue_user_statement(source_id,&input).is_err() {
            owner.store.set_document("memory/learning-receipt",br#"{"state":"pending_attention","notice":"Learning admission failed; the user turn remains available and chat can continue. Inspect memory learning-status."}"#,4096)?;
        }
        Ok(with_readiness(if selection.use_enabled {
            crate::memory::MemoryReadiness::Enabled
        } else {
            crate::memory::MemoryReadiness::UseDisabled
        }, &text))
    }).await.map_err(|_|"memory selection worker stopped".to_owned())?.map_err(|error|format!("{error:#}"))
}

// Current readiness travels with the existing one-text handoff. This bounded
// runtime notice makes absence/disablement explicit without a bridge model or
// sending a memory inventory. Codex still owns the inner loop and its context.
fn with_readiness(readiness: crate::memory::MemoryReadiness, input: &str) -> String {
    format!(
        "Xana runtime context:\n{}\n{}\n\n{}",
        readiness.notice(),
        readiness.guidance(),
        input
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleared_managed_conversation_drops_prior_project_prompt_memory() {
        use crate::{
            memory::{MemoryContext, MemoryScope},
            storage::{ProtectedStore, RecoveryIdentity, TestCustody},
        };
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let previous = ConversationId::new();
        let project = uuid::Uuid::new_v4();
        let profile = uuid::Uuid::new_v4();
        let owner = MemoryOwner::new(
            store,
            MemoryContext {
                conversation: Some(previous.as_uuid()),
                project: Some(project),
                profile: Some(profile),
            },
        );
        for (scope, statement) in [
            (
                MemoryScope::Project(project),
                "PRIOR_PROJECT_CANARY prefer diagrams",
            ),
            (
                MemoryScope::Conversation(previous.as_uuid()),
                "PRIOR_CONVERSATION_CANARY prefer tables",
            ),
            (
                MemoryScope::Profile(profile),
                "CURRENT_PROFILE_CANARY prefer short replies",
            ),
            (MemoryScope::User, "GLOBAL_USER_CANARY prefer examples"),
        ] {
            owner.remember(scope, statement.into(), None).unwrap();
        }
        // /clear creates a new, ungrouped Conversation without replacing the
        // launch config's immutable owner snapshot.
        let cleared = ConversationId::new();
        // The bounded handoff need not fit every eligible record at once.
        // Target each record without echoing its canary in the owner input.
        for (query, canary, retained) in [
            ("diagrams", "PRIOR_PROJECT_CANARY", false),
            ("tables", "PRIOR_CONVERSATION_CANARY", false),
            ("short", "CURRENT_PROFILE_CANARY", true),
            ("examples", "GLOBAL_USER_CANARY", true),
        ] {
            let before = prepare(Some(&owner), previous, query).await.unwrap();
            assert!(
                before.contains(canary),
                "missing original scope for {query}"
            );
            let after = prepare(Some(&owner), cleared, query).await.unwrap();
            assert_eq!(after.contains(canary), retained, "wrong scope for {query}");
            assert!(!after.contains("PRIOR_PROJECT_CANARY"));
            assert!(!after.contains("PRIOR_CONVERSATION_CANARY"));
        }
        assert_eq!(owner.context.conversation, Some(previous.as_uuid()));
        assert_eq!(owner.context.project, Some(project));
    }

    #[tokio::test]
    async fn managed_memory_readiness_is_honest_even_without_selected_facts() {
        let conversation = ConversationId::new();
        let unavailable = prepare(None, conversation, "hello").await.unwrap();
        assert!(unavailable.contains(crate::memory::MemoryReadiness::Unavailable.notice()));
        assert!(unavailable.ends_with("hello"));
        assert!(!unavailable.contains(crate::memory::MEMORY_GUIDANCE));
        assert!(!unavailable.contains("memory_lookup"));
        let directory = tempfile::tempdir().unwrap();
        let store = crate::storage::ProtectedStore::initialize(
            directory.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let owner = MemoryOwner::new(store, crate::memory::MemoryContext::default());
        let enabled = prepare(Some(&owner), conversation, "hello").await.unwrap();
        assert!(enabled.contains(crate::memory::MemoryReadiness::Enabled.notice()));
        owner
            .controls(
                crate::memory::MemoryScope::Conversation(conversation.to_string().parse().unwrap()),
                crate::memory::MemoryControlEdit {
                    no_memory: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        let disabled = prepare(Some(&owner), conversation, "hello").await.unwrap();
        assert!(disabled.contains(crate::memory::MemoryReadiness::UseDisabled.notice()));
        assert!(!disabled.contains(crate::memory::MemoryReadiness::Enabled.notice()));
        assert!(!disabled.contains(crate::memory::MEMORY_GUIDANCE));
        assert!(crate::context::estimate_tokens(&disabled) < 512);
    }
}
