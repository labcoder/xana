use super::{
    AvailabilityV1, FactAuthorityV1, FactSourceV1, FreshnessV1, SemanticError, validate_code,
    validate_text,
};
use crate::identity::{ConversationId, OperationId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const MAX_USAGE_OBSERVATIONS: usize = 4_096;
const MAX_ACCOUNTING_PERIOD_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum UsageScopeV1 {
    Request { run_id: OperationId },
    Run { run_id: OperationId },
    Conversation { conversation_id: ConversationId },
    Connection { connection: String },
    Account { connection: String, account: String },
    RateLimitBucket { connection: String, bucket: String },
}

impl UsageScopeV1 {
    fn validate(&self) -> Result<(), SemanticError> {
        match self {
            Self::Connection { connection } => {
                validate_text("usage connection", connection, 256)?;
            }
            Self::Account {
                connection,
                account,
            } => {
                validate_text("usage connection", connection, 256)?;
                validate_text("usage account", account, 256)?;
            }
            Self::RateLimitBucket { connection, bucket } => {
                validate_text("usage connection", connection, 256)?;
                validate_code("rate-limit bucket", bucket, 128)?;
            }
            Self::Request { .. } | Self::Run { .. } | Self::Conversation { .. } => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UsageAmountsV1 {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) reasoning_tokens: Option<u64>,
    pub(crate) tool_tokens: Option<u64>,
    pub(crate) cost_microunits: Option<u64>,
}

impl UsageAmountsV1 {
    fn is_complete(&self) -> bool {
        self.input_tokens.is_some()
            && self.cached_input_tokens.is_some()
            && self.output_tokens.is_some()
            && self.reasoning_tokens.is_some()
            && self.tool_tokens.is_some()
            && self.cost_microunits.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextOccupancyV1 {
    pub(crate) input_tokens: u64,
    pub(crate) capacity_tokens: Option<u64>,
    pub(crate) compacted_tokens: Option<u64>,
    pub(crate) derived_summary_tokens: Option<u64>,
    pub(crate) retrieval_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LimitObservationV1 {
    pub(crate) remaining: Option<u64>,
    pub(crate) limit: Option<u64>,
    pub(crate) reset_at_unix_millis: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageAccountingV1 {
    Delta,
    CumulativeSnapshot { sequence: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageObservationV1 {
    pub(crate) id: Uuid,
    pub(crate) scope: UsageScopeV1,
    /// Stable provider/request/window identifier. A reset starts a new period.
    pub(crate) period: String,
    pub(crate) accounting: UsageAccountingV1,
    pub(crate) amounts: UsageAmountsV1,
    pub(crate) context: Option<ContextOccupancyV1>,
    pub(crate) rate_limit: Option<LimitObservationV1>,
    pub(crate) quota: Option<LimitObservationV1>,
    pub(crate) credits_microunits: Option<u64>,
    pub(crate) availability: AvailabilityV1,
    pub(crate) source: FactSourceV1,
    pub(crate) authority: FactAuthorityV1,
    pub(crate) freshness: FreshnessV1,
}

impl UsageObservationV1 {
    pub(crate) fn validate(&self) -> Result<(), SemanticError> {
        self.scope.validate()?;
        validate_code(
            "usage accounting period",
            &self.period,
            MAX_ACCOUNTING_PERIOD_BYTES,
        )?;
        for (field, limit) in [
            ("rate limit", self.rate_limit.as_ref()),
            ("quota", self.quota.as_ref()),
        ] {
            if let Some(limit) = limit
                && limit
                    .remaining
                    .zip(limit.limit)
                    .is_some_and(|(remaining, maximum)| remaining > maximum)
            {
                return Err(SemanticError::InvalidStructure {
                    field,
                    reason: "remaining must not exceed limit",
                });
            }
        }
        if let Some(context) = &self.context
            && context
                .capacity_tokens
                .is_some_and(|capacity| context.input_tokens > capacity)
        {
            return Err(SemanticError::InvalidStructure {
                field: "context occupancy",
                reason: "input tokens must not exceed a known capacity",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UsageAggregateV1 {
    pub(crate) amounts: UsageAmountsV1,
    pub(crate) observation_count: usize,
    pub(crate) incomplete: bool,
    pub(crate) estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct UsageBucketKey {
    scope: UsageScopeV1,
    period: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UsageLedgerV1 {
    deltas: BTreeMap<UsageBucketKey, BTreeMap<Uuid, UsageObservationV1>>,
    cumulative: BTreeMap<UsageBucketKey, BTreeMap<FactSourceV1, UsageObservationV1>>,
    seen: BTreeSet<Uuid>,
}

impl UsageLedgerV1 {
    pub(crate) fn observe(
        &mut self,
        observation: UsageObservationV1,
    ) -> Result<bool, SemanticError> {
        observation.validate()?;
        if self.seen.contains(&observation.id) {
            return Ok(false);
        }
        if self.seen.len() >= MAX_USAGE_OBSERVATIONS {
            return Err(SemanticError::TooManyValues {
                field: "usage observations",
                actual: self.seen.len().saturating_add(1),
                limit: MAX_USAGE_OBSERVATIONS,
            });
        }
        let key = UsageBucketKey {
            scope: observation.scope.clone(),
            period: observation.period.clone(),
        };
        match observation.accounting {
            UsageAccountingV1::Delta => {
                self.deltas
                    .entry(key)
                    .or_default()
                    .insert(observation.id, observation.clone());
            }
            UsageAccountingV1::CumulativeSnapshot { sequence } => {
                let by_source = self.cumulative.entry(key).or_default();
                if by_source.get(&observation.source).is_some_and(|current| {
                    matches!(
                        current.accounting,
                        UsageAccountingV1::CumulativeSnapshot {
                            sequence: current_sequence
                        } if current_sequence >= sequence
                    )
                }) {
                    self.seen.insert(observation.id);
                    return Ok(false);
                }
                by_source.insert(observation.source, observation.clone());
            }
        }
        self.seen.insert(observation.id);
        Ok(true)
    }

    pub(crate) fn aggregate(
        &self,
        scope: &UsageScopeV1,
        period: &str,
    ) -> Result<UsageAggregateV1, SemanticError> {
        let key = UsageBucketKey {
            scope: scope.clone(),
            period: period.to_owned(),
        };
        let observations = self
            .deltas
            .get(&key)
            .into_iter()
            .flat_map(BTreeMap::values)
            .chain(
                self.cumulative
                    .get(&key)
                    .into_iter()
                    .flat_map(BTreeMap::values),
            );
        let mut aggregate = UsageAggregateV1::default();
        for observation in observations {
            aggregate.observation_count = aggregate.observation_count.saturating_add(1);
            aggregate.estimated |= observation.authority == FactAuthorityV1::Estimated
                || observation.source == FactSourceV1::Estimated;
            if !matches!(
                observation.availability,
                AvailabilityV1::Available | AvailabilityV1::Stale
            ) {
                aggregate.incomplete = true;
                continue;
            }
            aggregate.incomplete |= observation.availability == AvailabilityV1::Stale
                || !observation.amounts.is_complete();
            add_amounts(&mut aggregate.amounts, &observation.amounts)?;
        }
        if aggregate.observation_count == 0 {
            aggregate.incomplete = true;
        }
        Ok(aggregate)
    }
}

fn add_amounts(target: &mut UsageAmountsV1, value: &UsageAmountsV1) -> Result<(), SemanticError> {
    macro_rules! add {
        ($field:ident) => {
            if let Some(value) = value.$field {
                target.$field = Some(
                    target
                        .$field
                        .unwrap_or_default()
                        .checked_add(value)
                        .ok_or(SemanticError::ArithmeticOverflow(stringify!($field)))?,
                );
            }
        };
    }
    add!(input_tokens);
    add!(cached_input_tokens);
    add!(output_tokens);
    add!(reasoning_tokens);
    add!(tool_tokens);
    add!(cost_microunits);
    Ok(())
}
