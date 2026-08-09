use crate::config::OrchestrationLimits;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrchestrationBudget {
    pub(crate) limits: OrchestrationLimits,
    pub(crate) max_tool_rounds: usize,
    pub(crate) max_total_tool_rounds: usize,
    pub(crate) max_total_context_tokens: usize,
    pub(crate) max_total_report_bytes: usize,
    pub(crate) max_total_artifact_bytes: usize,
}

impl OrchestrationBudget {
    pub(crate) fn new(limits: OrchestrationLimits, max_tool_rounds: usize) -> Self {
        let descendants = limits.max_descendants;
        Self {
            max_tool_rounds,
            max_total_tool_rounds: max_tool_rounds.saturating_mul(descendants),
            max_total_context_tokens: limits.max_context_tokens.saturating_mul(descendants),
            max_total_report_bytes: limits.max_report_bytes.saturating_mul(descendants),
            max_total_artifact_bytes: limits.max_artifact_bytes.saturating_mul(descendants),
            limits,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(limits: OrchestrationLimits, max_tool_rounds: usize) -> Self {
        Self::new(limits, max_tool_rounds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReservationRequest {
    pub(crate) tool_rounds: usize,
    pub(crate) context_tokens: usize,
    pub(crate) report_bytes: usize,
    pub(crate) artifact_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BudgetReservation {
    request: ReservationRequest,
}

#[derive(Debug)]
pub(crate) struct BudgetLedger {
    budget: OrchestrationBudget,
    active_children: usize,
    running_children: usize,
    tool_rounds: usize,
    context_tokens: usize,
    report_bytes: usize,
    artifact_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetError {
    EmptyBatch,
    FanOut { requested: usize, maximum: usize },
    Descendants { requested: usize, remaining: usize },
    ToolRounds { requested: usize, remaining: usize },
    ContextTokens { requested: usize, remaining: usize },
    ReportBytes { requested: usize, remaining: usize },
    ArtifactBytes { requested: usize, remaining: usize },
}

impl fmt::Display for BudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBatch => formatter.write_str("child admission batch must not be empty"),
            Self::FanOut { requested, maximum } => write!(
                formatter,
                "child batch requests fan-out {requested}, exceeding maximum {maximum}"
            ),
            Self::Descendants {
                requested,
                remaining,
            } => write!(
                formatter,
                "child batch requests {requested} descendants with {remaining} remaining"
            ),
            Self::ToolRounds {
                requested,
                remaining,
            } => write!(
                formatter,
                "child batch reserves {requested} tool rounds with {remaining} remaining"
            ),
            Self::ContextTokens {
                requested,
                remaining,
            } => write!(
                formatter,
                "child batch reserves {requested} context tokens with {remaining} remaining"
            ),
            Self::ReportBytes {
                requested,
                remaining,
            } => write!(
                formatter,
                "child batch reserves {requested} report bytes with {remaining} remaining"
            ),
            Self::ArtifactBytes {
                requested,
                remaining,
            } => write!(
                formatter,
                "child batch reserves {requested} artifact bytes with {remaining} remaining"
            ),
        }
    }
}

impl Error for BudgetError {}

impl BudgetLedger {
    pub(crate) fn new(budget: OrchestrationBudget) -> Self {
        Self {
            budget,
            active_children: 0,
            running_children: 0,
            tool_rounds: 0,
            context_tokens: 0,
            report_bytes: 0,
            artifact_bytes: 0,
        }
    }

    pub(crate) fn budget(&self) -> &OrchestrationBudget {
        &self.budget
    }

    pub(crate) fn reserve_batch(
        &mut self,
        requests: &[ReservationRequest],
    ) -> Result<Vec<BudgetReservation>, BudgetError> {
        if requests.is_empty() {
            return Err(BudgetError::EmptyBatch);
        }
        if requests.len() > self.budget.limits.max_fan_out {
            return Err(BudgetError::FanOut {
                requested: requests.len(),
                maximum: self.budget.limits.max_fan_out,
            });
        }
        let requested_children = requests.len();
        check_capacity(
            requested_children,
            self.budget
                .limits
                .max_descendants
                .saturating_sub(self.active_children),
            |requested, remaining| BudgetError::Descendants {
                requested,
                remaining,
            },
        )?;
        let tool_rounds = sum(requests, |request| request.tool_rounds);
        let context_tokens = sum(requests, |request| request.context_tokens);
        let report_bytes = sum(requests, |request| request.report_bytes);
        let artifact_bytes = sum(requests, |request| request.artifact_bytes);
        check_capacity(
            tool_rounds,
            self.budget
                .max_total_tool_rounds
                .saturating_sub(self.tool_rounds),
            |requested, remaining| BudgetError::ToolRounds {
                requested,
                remaining,
            },
        )?;
        check_capacity(
            context_tokens,
            self.budget
                .max_total_context_tokens
                .saturating_sub(self.context_tokens),
            |requested, remaining| BudgetError::ContextTokens {
                requested,
                remaining,
            },
        )?;
        check_capacity(
            report_bytes,
            self.budget
                .max_total_report_bytes
                .saturating_sub(self.report_bytes),
            |requested, remaining| BudgetError::ReportBytes {
                requested,
                remaining,
            },
        )?;
        check_capacity(
            artifact_bytes,
            self.budget
                .max_total_artifact_bytes
                .saturating_sub(self.artifact_bytes),
            |requested, remaining| BudgetError::ArtifactBytes {
                requested,
                remaining,
            },
        )?;

        self.active_children += requested_children;
        self.tool_rounds += tool_rounds;
        self.context_tokens += context_tokens;
        self.report_bytes += report_bytes;
        self.artifact_bytes += artifact_bytes;
        Ok(requests
            .iter()
            .cloned()
            .map(|request| BudgetReservation { request })
            .collect())
    }

    pub(crate) fn can_start(&self) -> bool {
        self.running_children < self.budget.limits.max_concurrency
    }

    pub(crate) fn mark_started(&mut self) {
        assert!(
            self.can_start(),
            "scheduler exceeded its concurrency budget"
        );
        self.running_children += 1;
    }

    pub(crate) fn mark_stopped(&mut self) {
        self.running_children = self
            .running_children
            .checked_sub(1)
            .expect("running reservation released exactly once");
    }

    pub(crate) fn release(&mut self, reservation: BudgetReservation) {
        self.active_children = subtract(self.active_children, 1, "child");
        self.tool_rounds = subtract(
            self.tool_rounds,
            reservation.request.tool_rounds,
            "tool round",
        );
        self.context_tokens = subtract(
            self.context_tokens,
            reservation.request.context_tokens,
            "context token",
        );
        self.report_bytes = subtract(
            self.report_bytes,
            reservation.request.report_bytes,
            "report byte",
        );
        self.artifact_bytes = subtract(
            self.artifact_bytes,
            reservation.request.artifact_bytes,
            "artifact byte",
        );
    }

    #[cfg(test)]
    pub(crate) fn active_counts(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.active_children,
            self.running_children,
            self.tool_rounds,
            self.context_tokens,
            self.report_bytes,
            self.artifact_bytes,
        )
    }
}

fn check_capacity<E>(requested: usize, remaining: usize, error: E) -> Result<(), BudgetError>
where
    E: FnOnce(usize, usize) -> BudgetError,
{
    if requested > remaining {
        Err(error(requested, remaining))
    } else {
        Ok(())
    }
}

fn sum(requests: &[ReservationRequest], field: impl Fn(&ReservationRequest) -> usize) -> usize {
    requests.iter().fold(0usize, |total, request| {
        total.saturating_add(field(request))
    })
}

fn subtract(current: usize, amount: usize, name: &str) -> usize {
    current
        .checked_sub(amount)
        .unwrap_or_else(|| panic!("{name} reservation released more than once"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> OrchestrationLimits {
        OrchestrationLimits {
            max_fan_out: 2,
            max_descendants: 2,
            max_concurrency: 1,
            deadline_seconds: 30,
            max_context_tokens: 10,
            max_report_bytes: 20,
            max_artifact_bytes: 30,
        }
    }

    fn request() -> ReservationRequest {
        ReservationRequest {
            tool_rounds: 2,
            context_tokens: 10,
            report_bytes: 20,
            artifact_bytes: 30,
        }
    }

    #[test]
    fn batch_reservation_is_atomic_at_every_exact_boundary() {
        let mut ledger = BudgetLedger::new(OrchestrationBudget::for_tests(limits(), 2));
        let reservations = ledger
            .reserve_batch(&[request(), request()])
            .expect("exact limits");
        assert_eq!(ledger.active_counts(), (2, 0, 4, 20, 40, 60));
        assert!(matches!(
            ledger.reserve_batch(&[request()]),
            Err(BudgetError::Descendants { .. })
        ));
        assert_eq!(ledger.active_counts(), (2, 0, 4, 20, 40, 60));

        for reservation in reservations {
            ledger.release(reservation);
        }
        assert_eq!(ledger.active_counts(), (0, 0, 0, 0, 0, 0));
    }

    #[test]
    fn running_capacity_is_separate_from_admission_capacity() {
        let mut ledger = BudgetLedger::new(OrchestrationBudget::for_tests(limits(), 2));
        let _ = ledger
            .reserve_batch(&[request(), request()])
            .expect("batch");
        assert!(ledger.can_start());
        ledger.mark_started();
        assert!(!ledger.can_start());
        ledger.mark_stopped();
        assert!(ledger.can_start());
    }

    #[test]
    fn invalid_fanout_changes_no_counter() {
        let mut ledger = BudgetLedger::new(OrchestrationBudget::for_tests(limits(), 2));
        assert!(matches!(
            ledger.reserve_batch(&[request(), request(), request()]),
            Err(BudgetError::FanOut { .. })
        ));
        assert_eq!(ledger.active_counts(), (0, 0, 0, 0, 0, 0));
    }

    #[test]
    fn every_aggregate_reservation_rejects_one_over_without_mutation() {
        let cases = [
            (
                ReservationRequest {
                    tool_rounds: 5,
                    ..request()
                },
                "tool rounds",
            ),
            (
                ReservationRequest {
                    context_tokens: 21,
                    ..request()
                },
                "context tokens",
            ),
            (
                ReservationRequest {
                    report_bytes: 41,
                    ..request()
                },
                "report bytes",
            ),
            (
                ReservationRequest {
                    artifact_bytes: 61,
                    ..request()
                },
                "artifact bytes",
            ),
        ];
        for (request, label) in cases {
            let mut ledger = BudgetLedger::new(OrchestrationBudget::for_tests(limits(), 2));
            assert!(ledger.reserve_batch(&[request]).is_err(), "{label}");
            assert_eq!(ledger.active_counts(), (0, 0, 0, 0, 0, 0), "{label}");
        }
    }

    #[test]
    fn descendant_capacity_is_shared_across_separate_admissions() {
        let mut ledger = BudgetLedger::new(OrchestrationBudget::for_tests(limits(), 2));
        let first = ledger.reserve_batch(&[request()]).expect("first child");
        assert!(matches!(
            ledger.reserve_batch(&[request(), request()]),
            Err(BudgetError::Descendants {
                requested: 2,
                remaining: 1
            })
        ));
        assert_eq!(ledger.active_counts(), (1, 0, 2, 10, 20, 30));
        ledger.release(first.into_iter().next().expect("reservation"));
        assert_eq!(ledger.active_counts(), (0, 0, 0, 0, 0, 0));
    }
}
