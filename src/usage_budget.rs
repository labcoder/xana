//! Durable admission contracts, independent of vendor quota observations.

use crate::{
    identity::{OperationId, StepId},
    storage::ProtectedStore,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetPolicy {
    pub(crate) daily_requests: u64,
    pub(crate) root_requests: u64,
    pub(crate) foreground_request_reserve: u64,
    pub(crate) daily_tokens: Option<u64>,
    pub(crate) root_tokens: Option<u64>,
    pub(crate) background_daily_tokens: u64,
    pub(crate) background_job_tokens: u64,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            daily_requests: 10_000,
            root_requests: 10_000,
            foreground_request_reserve: 32,
            daily_tokens: None,
            root_tokens: None,
            background_daily_tokens: 32_768,
            background_job_tokens: 8_192,
        }
    }
}

impl BudgetPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100_000).contains(&self.daily_requests)
                && (1..=1_000_000).contains(&self.root_requests),
            "request allowance is outside its supported range"
        );
        ensure!(
            self.foreground_request_reserve <= self.daily_requests,
            "foreground reserve exceeds daily requests"
        );
        for limit in [
            self.daily_tokens,
            self.root_tokens,
            Some(self.background_daily_tokens),
            Some(self.background_job_tokens),
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                (1..=1_000_000_000).contains(&limit),
                "token allowance is outside its supported range"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkClass {
    Foreground,
    Background,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchFacts {
    pub(crate) project: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) connection: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Admission {
    pub(crate) facts: DispatchFacts,
    pub(crate) id: String,
    pub(crate) operation: String,
    pub(crate) root: String,
    pub(crate) job: String,
    pub(crate) route: String,
    pub(crate) class: WorkClass,
    pub(crate) reserved_tokens: u64,
}

impl Admission {
    pub(crate) fn validate(&self) -> Result<()> {
        for value in [
            &self.facts.project,
            &self.facts.profile,
            &self.facts.owner,
            &self.facts.connection,
            &self.facts.model,
            &self.facts.reasoning,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                value.len() <= 512 && !value.chars().any(char::is_control),
                "invalid usage dispatch facts"
            );
        }
        for value in [&self.id, &self.operation, &self.root, &self.job] {
            ensure!(
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
                "invalid usage attribution"
            );
        }
        ensure!(
            !self.route.is_empty()
                && self.route.len() <= 1024
                && !self.route.chars().any(char::is_control),
            "invalid usage route"
        );
        ensure!(
            (1..=1_000_000_000).contains(&self.reserved_tokens),
            "invalid usage reservation"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    /// Vendor cumulative counter observation; never added as per-request usage.
    pub(crate) cumulative: Option<CumulativeUsage>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) reported_cost_microunits: Option<u64>,
    pub(crate) outcome: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CumulativeUsage {
    pub(crate) counter: String,
    pub(crate) total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UsageRecord {
    pub(crate) sequence: u64,
    pub(crate) day: u64,
    pub(crate) admission: Admission,
    pub(crate) charged_tokens: u64,
    pub(crate) receipt: Option<Receipt>,
}

/// Remaining configured admission allowance, not a vendor wallet estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemainingAllowance {
    pub(crate) requests: u64,
    pub(crate) tokens: Option<u64>,
    pub(crate) exceeded: bool,
}

/// Composition supplies the owner/route once; every child shares the root's
/// durable allowance. Dropping a reservation deliberately never refunds it.
#[derive(Clone)]
pub(crate) struct UsageBudget {
    store: ProtectedStore,
    root: String,
    route: String,
    output_reserve: u64,
    job: Option<String>,
    class: WorkClass,
    facts: DispatchFacts,
}

impl UsageBudget {
    pub(crate) fn remaining(&self, operation: OperationId) -> Result<RemainingAllowance> {
        let day = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            / 86_400;
        self.store.usage_remaining(
            &self.root,
            self.job.as_deref().unwrap_or(&operation.to_string()),
            self.class,
            day,
        )
    }
    pub(crate) async fn foreground_helper_lease(
        &self,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<crate::storage::ForegroundJobLease> {
        self.store.foreground_job_lease(cancellation).await
    }
    pub(crate) fn new(
        store: ProtectedStore,
        root: String,
        route: String,
        output_reserve: u64,
    ) -> Self {
        Self {
            store,
            root,
            route,
            output_reserve,
            job: None,
            class: WorkClass::Foreground,
            facts: DispatchFacts::default(),
        }
    }

    pub(crate) fn reroute(&self, route: String, output_reserve: u64) -> Self {
        Self {
            route,
            output_reserve,
            ..self.clone()
        }
    }

    pub(crate) fn with_facts(mut self, facts: DispatchFacts) -> Self {
        self.facts = facts;
        self
    }

    /// Detached work keeps one durable occurrence budget across every request.
    pub(crate) fn background(mut self, job: String) -> Self {
        self.class = WorkClass::Background;
        self.job = Some(job);
        self
    }

    pub(crate) fn child_route(
        &self,
        route: &str,
        owner: &str,
        connection: &str,
        model: &str,
        profile: &str,
        reasoning: Option<String>,
    ) -> Self {
        self.reroute(format!("child/{route}"), self.output_reserve)
            .with_facts(DispatchFacts {
                owner: Some(owner.into()),
                connection: Some(connection.into()),
                model: Some(model.into()),
                profile: Some(profile.into()),
                reasoning,
                ..self.facts.clone()
            })
    }

    pub(crate) fn inherit_operation(mut self, parent: OperationId) -> Result<Self> {
        if let Some(parent) = self.store.usage_attribution(&parent.to_string())? {
            self.root = parent.root;
            self.job = Some(parent.job);
            self.class = parent.class;
            self.facts.project = parent.facts.project;
        }
        Ok(self)
    }

    pub(crate) fn rebind_root(&mut self, root: String) {
        self.root = root;
    }

    pub(crate) fn with_model(&self, model: &str) -> Self {
        let mut updated = self.clone();
        updated.facts.model = Some(model.into());
        updated
    }

    pub(crate) fn with_reasoning(mut self, reasoning: Option<String>) -> Self {
        self.facts.reasoning = reasoning;
        self
    }

    pub(crate) fn admit(
        &self,
        operation: OperationId,
        id: StepId,
        input_tokens: u64,
    ) -> Result<Reservation> {
        let request = Admission {
            facts: self.facts.clone(),
            id: id.to_string(),
            operation: operation.to_string(),
            root: self.root.clone(),
            job: self.job.clone().unwrap_or_else(|| operation.to_string()),
            route: self.route.clone(),
            class: self.class,
            reserved_tokens: input_tokens.saturating_add(self.output_reserve).max(1),
        };
        let day = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            / 86_400;
        self.store.reserve_usage(&request, day)?;
        Ok(Reservation {
            store: self.store.clone(),
            id: request.id,
        })
    }
}

pub(crate) struct Reservation {
    store: ProtectedStore,
    id: String,
}

impl Reservation {
    pub(crate) fn settle(&self, receipt: Receipt) -> Result<()> {
        self.store.settle_usage(&self.id, &receipt)
    }
}

#[cfg(test)]
mod tests;
