//! Optional personal context is added after mandatory instructions and charged
//! against both the whole prompt and the dedicated personal-memory allowance.
use super::*;

impl PromptSnapshot {
    /// Replaceable personal data must not accumulate across provider rounds.
    pub(crate) fn without_personal_memory(mut self) -> Self {
        self.layers
            .retain(|layer| layer.kind != PromptLayerKind::PersonalMemory);
        let rendered = render_layers(&self.layers);
        self.system_tokens = estimate_tokens(&rendered);
        self.system_message = Message::text(Role::System, rendered);
        refresh_layer_costs(&mut self.layers);
        self
    }

    /// A runtime fact, not optional retrieved data; account for it before selecting memory.
    pub(crate) fn with_memory_readiness(
        mut self,
        readiness: crate::memory::MemoryReadiness,
    ) -> Result<Self, PromptError> {
        self.layers
            .retain(|layer| layer.source_id != "runtime:personal-memory");
        self.layers.push(layer(
            PromptLayerKind::Environment,
            "runtime:personal-memory",
            SourceProvenance {
                display_name: "Personal memory readiness".into(),
                path: None,
                origin: SourceOrigin::RuntimeEnvironment,
            },
            TrustClass::Runtime,
            &format!("{}\n{}", readiness.notice(), readiness.guidance()),
            false,
        ));
        let rendered = render_layers(&self.layers);
        let required = estimate_tokens(&rendered)
            .saturating_add(self.tool_schema_tokens)
            .saturating_add(self.budget.conversation_reserve_tokens);
        if required > self.budget.total_tokens {
            return Err(PromptError::RequiredLayersExceedBudget {
                required_tokens: required,
                total_tokens: self.budget.total_tokens,
            });
        }
        self.system_tokens = estimate_tokens(&rendered);
        self.system_message = Message::text(Role::System, rendered);
        refresh_layer_costs(&mut self.layers);
        Ok(self)
    }

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
