//! Optional personal context is added after mandatory instructions and charged
//! against both the whole prompt and the dedicated personal-memory allowance.
use super::*;

impl PromptSnapshot {
    pub(crate) fn with_personal_memory(
        mut self,
        selection: &crate::memory::MemorySelection,
    ) -> (Self, Vec<uuid::Uuid>) {
        let cap = 2048.min(self.budget.total_tokens / 20);
        let baseline = self.system_tokens;
        let mut ids = Vec::new();
        for record in &selection.records {
            let text = format!(
                "{}\n{}",
                selection.notice,
                serde_json::to_string(record).expect("validated memory serializes")
            );
            self.layers.push(layer(
                PromptLayerKind::PersonalMemory,
                format!("memory:{}:{}", record.id, record.revision),
                SourceProvenance {
                    display_name: format!(
                        "{} · revision {} · {}",
                        record.id, record.revision, record.scope
                    ),
                    path: None,
                    origin: SourceOrigin::PersonalMemory,
                },
                TrustClass::Data,
                &text,
                false,
            ));
            let rendered = render_layers(&self.layers);
            let tokens = estimate_tokens(&rendered);
            if tokens.saturating_sub(baseline) > cap
                || tokens
                    .saturating_add(self.tool_schema_tokens)
                    .saturating_add(self.budget.conversation_reserve_tokens)
                    > self.budget.total_tokens
            {
                self.layers.pop();
                continue;
            }
            self.system_tokens = tokens;
            self.system_message = Message::text(Role::System, rendered);
            ids.push(record.id);
        }
        refresh_layer_costs(&mut self.layers);
        (self, ids)
    }
}
