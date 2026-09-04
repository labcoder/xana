//! Redacted accounting for the exact frozen prefix and current request tail.

use super::{
    CacheObservation, PROMPT_LEDGER_VERSION, PromptLayerKind, PromptLedgerCategory,
    PromptLedgerCategoryKind as Category, PromptPlanLedger, PromptSnapshot,
    budget::TokenEstimateSource, estimate_message_tokens, render::estimate_image_tokens,
};
use crate::{
    context::estimate_tokens,
    message::{ContentBlock, Message},
};

impl PromptSnapshot {
    pub(crate) fn ledger(&self, history: &[Message]) -> Option<PromptPlanLedger> {
        let budget = self.budget_plan.clone()?;
        let mut categories = [
            Category::Instructions,
            Category::RuntimeFacts,
            Category::ParentHandoff,
            Category::PersonalMemory,
            Category::RetrievedEvidence,
            Category::ToolDefinitions,
            Category::CompactedHistory,
            Category::RecentHistory,
            Category::ToolEvidence,
            Category::Attachments,
        ]
        .map(|kind| PromptLedgerCategory {
            kind,
            estimated_tokens: 0,
        })
        .to_vec();
        // Per-layer rounding can exceed whole-prefix rounding by a few tokens.
        // Allocate that single counted prefix once, including wrapper overhead.
        let mut remaining = self.system_tokens;
        for layer in &self.layers {
            let kind = match layer.kind {
                PromptLayerKind::Environment | PromptLayerKind::Surface => Category::RuntimeFacts,
                PromptLayerKind::CompactedHistory => Category::CompactedHistory,
                PromptLayerKind::ParentHandoff => Category::ParentHandoff,
                _ => Category::Instructions,
            };
            let tokens = layer.estimated_tokens.min(remaining);
            charge(&mut categories, kind, tokens);
            remaining -= tokens;
        }
        charge(&mut categories, Category::Instructions, remaining);
        charge(
            &mut categories,
            Category::ToolDefinitions,
            self.tool_schema_tokens,
        );

        let mut attachment_count = 0;
        let mut attachment_bytes = 0_u64;
        let mut history_tokens = 0_usize;
        for message in history {
            let mut text_tokens = estimate_message_tokens(message);
            history_tokens = history_tokens.saturating_add(text_tokens);
            for block in &message.content {
                let (kind, tokens) = match block {
                    ContentBlock::Image(image) => {
                        attachment_count += 1;
                        attachment_bytes = attachment_bytes.saturating_add(image.byte_len);
                        (Category::Attachments, estimate_image_tokens(image))
                    }
                    ContentBlock::ToolResult(result) => {
                        (Category::ToolEvidence, estimate_tokens(&result.output))
                    }
                    _ => continue,
                };
                charge(&mut categories, kind, tokens);
                text_tokens = text_tokens.saturating_sub(tokens);
            }
            charge(&mut categories, Category::RecentHistory, text_tokens);
        }

        Some(PromptPlanLedger {
            version: PROMPT_LEDGER_VERSION,
            estimator: TokenEstimateSource::Utf8HeuristicV1,
            budget,
            categories,
            estimated_input_tokens: self
                .system_tokens
                .saturating_add(self.tool_schema_tokens)
                .saturating_add(history_tokens),
            attachment_count,
            attachment_bytes,
            omitted_source_ids: self
                .context_plan
                .omitted_sources
                .iter()
                .take(64)
                .map(|id| id.as_str().to_owned())
                .collect(),
            cache_read: CacheObservation::Unavailable,
            cache_write: CacheObservation::Unavailable,
        })
    }
}

fn charge(categories: &mut [PromptLedgerCategory], kind: Category, tokens: usize) {
    if let Some(category) = categories.iter_mut().find(|category| category.kind == kind) {
        category.estimated_tokens = category.estimated_tokens.saturating_add(tokens);
    }
}
