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

pub(crate) async fn prepare(
    owner: Option<&MemoryOwner>,
    conversation: ConversationId,
    input: &str,
) -> Result<String, String> {
    let Some(owner) = owner else {
        return Ok(input.to_owned());
    };
    let mut owner = owner.clone();
    owner.context.conversation = Some(
        conversation
            .to_string()
            .parse()
            .map_err(|_| "invalid managed Conversation identity")?,
    );
    let input = input.to_owned();
    tokio::task::spawn_blocking(move ||->anyhow::Result<String> {
        // Codex owns its actual context window. 16,384 is Xana's conservative
        // unknown-window allowance, not a claim about a selected vendor model.
        let selection=owner.select_for_turn(&input,16_384)?;
        let (text,ids)=selection.managed_text(&input,16_384)?;
        owner.record_selection(&selection,&ids)?;
        // Only this owner-input edge can enqueue; vendor output never does.
        if owner.enqueue_user_statement(uuid::Uuid::new_v4(),&input).is_err() {
            owner.store.set_document("memory/learning-receipt",br#"{"state":"pending_attention","notice":"Learning admission failed; the user turn remains available and chat can continue. Inspect memory learning-status."}"#,4096)?;
        }
        Ok(text)
    }).await.map_err(|_|"memory selection worker stopped".to_owned())?.map_err(|error|format!("{error:#}"))
}
