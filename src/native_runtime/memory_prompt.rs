//! Revalidate protected personal data before each native provider request.
use super::*;

pub(super) struct MemoryPromptRefresh {
    pub(super) owner: crate::memory::MemoryOwner,
    pub(super) query: Arc<str>,
}

impl crate::agent::RequestPromptRefresh for MemoryPromptRefresh {
    fn refresh(&self, prompt: &PromptSnapshot) -> anyhow::Result<PromptSnapshot> {
        let snapshot = prompt.clone().without_personal_memory();
        let selection = self
            .owner
            .select_for_turn(&self.query, snapshot.budget.total_tokens)?;
        let snapshot = snapshot.with_memory_readiness(if selection.use_enabled {
            crate::memory::MemoryReadiness::Enabled
        } else {
            crate::memory::MemoryReadiness::UseDisabled
        })?;
        let (snapshot, ids) = snapshot.with_personal_memory(&selection);
        self.owner.record_selection(&selection, &ids)?;
        Ok(snapshot)
    }
}
