//! Model-aware prompt budgets and bounded, redacted prompt-plan facts.
//!
//! Providers do not expose one portable tokenizer or cache contract. Xana
//! therefore records conservative estimates and their provenance instead of
//! presenting them as provider-exact usage.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

pub(crate) const PROMPT_LEDGER_VERSION: u16 = 1;
pub(crate) const HARD_CONTEXT_CEILING_TOKENS: usize = 1_000_000;
const MIN_USABLE_CONTEXT_TOKENS: usize = 2_048;

fn default_fallback_context_tokens() -> usize {
    32_768
}

fn default_output_reserve_tokens() -> usize {
    4_096
}

fn default_reasoning_reserve_tokens() -> usize {
    2_048
}

fn default_tool_reserve_tokens() -> usize {
    4_096
}

fn default_compaction_threshold_percent() -> u8 {
    80
}

fn default_retained_tail_tokens() -> usize {
    8_192
}

fn default_summary_max_bytes() -> usize {
    16 * 1024
}

/// User policy may narrow a known model limit but never enlarge it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PromptBudgetPolicy {
    pub(crate) max_context_tokens: Option<usize>,
    pub(crate) fallback_context_tokens: usize,
    pub(crate) output_reserve_tokens: usize,
    pub(crate) reasoning_reserve_tokens: usize,
    pub(crate) tool_reserve_tokens: usize,
    pub(crate) compaction_threshold_percent: u8,
    pub(crate) retained_tail_tokens: usize,
    pub(crate) summary_max_bytes: usize,
}

impl Default for PromptBudgetPolicy {
    fn default() -> Self {
        Self {
            max_context_tokens: None,
            fallback_context_tokens: default_fallback_context_tokens(),
            output_reserve_tokens: default_output_reserve_tokens(),
            reasoning_reserve_tokens: default_reasoning_reserve_tokens(),
            tool_reserve_tokens: default_tool_reserve_tokens(),
            compaction_threshold_percent: default_compaction_threshold_percent(),
            retained_tail_tokens: default_retained_tail_tokens(),
            summary_max_bytes: default_summary_max_bytes(),
        }
    }
}

impl PromptBudgetPolicy {
    pub(crate) fn validate(&self) -> Result<(), PromptBudgetError> {
        if self.max_context_tokens.is_some_and(|tokens| {
            !(MIN_USABLE_CONTEXT_TOKENS..=HARD_CONTEXT_CEILING_TOKENS).contains(&tokens)
        }) {
            return Err(PromptBudgetError::InvalidPolicy(
                "max_context_tokens must be in 2048..=1000000",
            ));
        }
        if !(MIN_USABLE_CONTEXT_TOKENS..=HARD_CONTEXT_CEILING_TOKENS)
            .contains(&self.fallback_context_tokens)
        {
            return Err(PromptBudgetError::InvalidPolicy(
                "fallback_context_tokens must be in 2048..=1000000",
            ));
        }
        if self.output_reserve_tokens == 0
            || self.output_reserve_tokens > HARD_CONTEXT_CEILING_TOKENS
            || self.reasoning_reserve_tokens > HARD_CONTEXT_CEILING_TOKENS
            || self.tool_reserve_tokens == 0
            || self.tool_reserve_tokens > HARD_CONTEXT_CEILING_TOKENS
            || self.retained_tail_tokens == 0
            || self.retained_tail_tokens > HARD_CONTEXT_CEILING_TOKENS
        {
            return Err(PromptBudgetError::InvalidPolicy(
                "prompt reserves must be positive and bounded",
            ));
        }
        if !(50..=95).contains(&self.compaction_threshold_percent) {
            return Err(PromptBudgetError::InvalidPolicy(
                "compaction_threshold_percent must be in 50..=95",
            ));
        }
        if !(1_024..=256 * 1024).contains(&self.summary_max_bytes) {
            return Err(PromptBudgetError::InvalidPolicy(
                "summary_max_bytes must be in 1024..=262144",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelBudgetFacts {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) context_tokens: Option<usize>,
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) reasoning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextWindowSource {
    ModelCatalog,
    ConservativeFallback,
    ContradictoryCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptBudgetPlan {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) context_window_tokens: usize,
    pub(crate) context_window_source: ContextWindowSource,
    pub(crate) route_ceiling_tokens: Option<usize>,
    pub(crate) input_budget_tokens: usize,
    pub(crate) output_reserve_tokens: usize,
    pub(crate) reasoning_reserve_tokens: usize,
    pub(crate) tool_reserve_tokens: usize,
    pub(crate) conversation_reserve_tokens: usize,
    pub(crate) compaction_threshold_tokens: usize,
    pub(crate) retained_tail_tokens: usize,
    pub(crate) summary_max_bytes: usize,
}

impl PromptBudgetPlan {
    pub(crate) fn derive(
        policy: &PromptBudgetPolicy,
        facts: ModelBudgetFacts,
    ) -> Result<Self, PromptBudgetError> {
        policy.validate()?;

        let (advertised_context, source) = match facts.context_tokens {
            Some(tokens) if tokens >= MIN_USABLE_CONTEXT_TOKENS => {
                (tokens, ContextWindowSource::ModelCatalog)
            }
            Some(tokens) => (tokens, ContextWindowSource::ContradictoryCatalog),
            None => (
                policy.fallback_context_tokens,
                ContextWindowSource::ConservativeFallback,
            ),
        };
        let context_window_tokens = advertised_context
            .min(
                policy
                    .max_context_tokens
                    .unwrap_or(HARD_CONTEXT_CEILING_TOKENS),
            )
            .min(HARD_CONTEXT_CEILING_TOKENS);
        if context_window_tokens < MIN_USABLE_CONTEXT_TOKENS {
            return Err(PromptBudgetError::UnusableModelLimit {
                context_tokens: context_window_tokens,
            });
        }

        // An advertised maximum larger than the entire window is contradictory.
        // Fall back to the smaller configured reserve without pretending the
        // provider supports a larger input than it advertised.
        let output_reserve_tokens = facts
            .max_output_tokens
            .filter(|tokens| *tokens > 0 && *tokens < context_window_tokens)
            .unwrap_or(policy.output_reserve_tokens)
            .min(context_window_tokens / 2)
            .max(1);
        let reasoning_reserve_tokens = if facts.reasoning {
            policy
                .reasoning_reserve_tokens
                .min(context_window_tokens / 4)
        } else {
            0
        };
        let input_budget_tokens = context_window_tokens
            .saturating_sub(output_reserve_tokens)
            .saturating_sub(reasoning_reserve_tokens);
        if input_budget_tokens < MIN_USABLE_CONTEXT_TOKENS {
            return Err(PromptBudgetError::ReservesExhaustWindow {
                context_tokens: context_window_tokens,
                output_tokens: output_reserve_tokens,
                reasoning_tokens: reasoning_reserve_tokens,
            });
        }

        let threshold_before_tools = input_budget_tokens
            .saturating_mul(usize::from(policy.compaction_threshold_percent))
            / 100;
        let compaction_threshold_tokens = threshold_before_tools
            .saturating_sub(policy.tool_reserve_tokens)
            .max(MIN_USABLE_CONTEXT_TOKENS);
        let retained_tail_tokens = policy
            .retained_tail_tokens
            .min(compaction_threshold_tokens / 2)
            .max(1);
        let conversation_reserve_tokens = retained_tail_tokens.min(input_budget_tokens / 2).max(1);

        Ok(Self {
            connection: facts.connection,
            model: facts.model,
            context_window_tokens,
            context_window_source: source,
            route_ceiling_tokens: policy.max_context_tokens,
            input_budget_tokens,
            output_reserve_tokens,
            reasoning_reserve_tokens,
            tool_reserve_tokens: policy.tool_reserve_tokens,
            conversation_reserve_tokens,
            compaction_threshold_tokens,
            retained_tail_tokens,
            summary_max_bytes: policy.summary_max_bytes,
        })
    }

    /// Validate persisted budget provenance without trusting journal bytes.
    pub(crate) fn is_valid_checkpoint_plan(&self) -> bool {
        let route_is_valid = self.route_ceiling_tokens.is_none_or(|ceiling| {
            (MIN_USABLE_CONTEXT_TOKENS..=HARD_CONTEXT_CEILING_TOKENS).contains(&ceiling)
                && self.context_window_tokens <= ceiling
        });
        !self.connection.trim().is_empty()
            && self.connection.len() <= 256
            && !self.model.trim().is_empty()
            && self.model.len() <= 256
            && (MIN_USABLE_CONTEXT_TOKENS..=HARD_CONTEXT_CEILING_TOKENS)
                .contains(&self.context_window_tokens)
            && route_is_valid
            && self.output_reserve_tokens > 0
            && self.output_reserve_tokens <= self.context_window_tokens / 2
            && self.reasoning_reserve_tokens <= self.context_window_tokens / 4
            && self.input_budget_tokens
                == self
                    .context_window_tokens
                    .saturating_sub(self.output_reserve_tokens)
                    .saturating_sub(self.reasoning_reserve_tokens)
            && self.input_budget_tokens >= MIN_USABLE_CONTEXT_TOKENS
            && self.tool_reserve_tokens > 0
            && self.tool_reserve_tokens <= HARD_CONTEXT_CEILING_TOKENS
            && (MIN_USABLE_CONTEXT_TOKENS..=self.input_budget_tokens)
                .contains(&self.compaction_threshold_tokens)
            && self.retained_tail_tokens > 0
            && self.retained_tail_tokens <= self.compaction_threshold_tokens / 2
            && self.conversation_reserve_tokens
                == self
                    .retained_tail_tokens
                    .min(self.input_budget_tokens / 2)
                    .max(1)
            && (1_024..=256 * 1_024).contains(&self.summary_max_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptLedgerCategoryKind {
    Instructions,
    ToolDefinitions,
    CompactedHistory,
    RecentHistory,
    Attachments,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptLedgerCategory {
    pub(crate) kind: PromptLedgerCategoryKind,
    pub(crate) estimated_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheObservation {
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PromptPlanLedger {
    pub(crate) version: u16,
    pub(crate) budget: PromptBudgetPlan,
    pub(crate) categories: Vec<PromptLedgerCategory>,
    pub(crate) estimated_input_tokens: usize,
    pub(crate) attachment_count: usize,
    pub(crate) attachment_bytes: u64,
    pub(crate) omitted_source_ids: Vec<String>,
    pub(crate) cache_read: CacheObservation,
    pub(crate) cache_write: CacheObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptBudgetError {
    InvalidPolicy(&'static str),
    UnusableModelLimit {
        context_tokens: usize,
    },
    ReservesExhaustWindow {
        context_tokens: usize,
        output_tokens: usize,
        reasoning_tokens: usize,
    },
}

impl fmt::Display for PromptBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy(reason) => {
                write!(formatter, "invalid prompt budget policy: {reason}")
            }
            Self::UnusableModelLimit { context_tokens } => write!(
                formatter,
                "model metadata leaves only {context_tokens} context tokens; configure a valid model limit or select another model"
            ),
            Self::ReservesExhaustWindow {
                context_tokens,
                output_tokens,
                reasoning_tokens,
            } => write!(
                formatter,
                "model context window {context_tokens} is exhausted by output ({output_tokens}) and reasoning ({reasoning_tokens}) reserves"
            ),
        }
    }
}

impl Error for PromptBudgetError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(context_tokens: Option<usize>) -> ModelBudgetFacts {
        ModelBudgetFacts {
            connection: "test".into(),
            model: "model".into(),
            context_tokens,
            max_output_tokens: Some(4_096),
            reasoning: false,
        }
    }

    #[test]
    fn known_model_windows_produce_different_bounded_input_plans() {
        let policy = PromptBudgetPolicy::default();
        let small = PromptBudgetPlan::derive(&policy, facts(Some(16_384))).unwrap();
        let large = PromptBudgetPlan::derive(&policy, facts(Some(128_000))).unwrap();

        assert!(small.input_budget_tokens < large.input_budget_tokens);
        assert_eq!(
            small.context_window_source,
            ContextWindowSource::ModelCatalog
        );
        assert!(large.compaction_threshold_tokens < large.input_budget_tokens);
    }

    #[test]
    fn missing_metadata_uses_documented_conservative_fallback() {
        let plan = PromptBudgetPlan::derive(&PromptBudgetPolicy::default(), facts(None)).unwrap();

        assert_eq!(plan.context_window_tokens, 32_768);
        assert_eq!(
            plan.context_window_source,
            ContextWindowSource::ConservativeFallback
        );
    }

    #[test]
    fn configured_ceiling_can_only_narrow_a_known_limit() {
        let policy = PromptBudgetPolicy {
            max_context_tokens: Some(24_000),
            ..PromptBudgetPolicy::default()
        };
        let plan = PromptBudgetPlan::derive(&policy, facts(Some(128_000))).unwrap();

        assert_eq!(plan.context_window_tokens, 24_000);
        assert_eq!(plan.route_ceiling_tokens, Some(24_000));
    }

    #[test]
    fn contradictory_tiny_catalog_limit_is_not_overclaimed() {
        let error = PromptBudgetPlan::derive(&PromptBudgetPolicy::default(), facts(Some(1_024)))
            .unwrap_err();

        assert_eq!(
            error,
            PromptBudgetError::UnusableModelLimit {
                context_tokens: 1_024
            }
        );
    }
}
