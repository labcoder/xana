//! Content-free assertion locations and opt-in, single-case synthetic inspection.

use super::{CompactionSummary, Cycle, SUMMARY_MAX_BYTES};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AssertionId {
    Scope,
    CurrentTarget,
    UnresolvedWork,
    NoDeploy,
    OriginalTarget,
    SupersededTarget,
    SupersededUnresolvedWork,
    ToolScope,
    ToolCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActiveField {
    Goal,
    Constraints,
    Progress,
    Decisions,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AssertionObservation {
    pub(crate) id: AssertionId,
    pub(crate) active_fields: Vec<ActiveField>,
    pub(crate) references_hit: bool,
}

pub(super) fn assertion_observations(
    cycle: Cycle<'_>,
    summary: Option<&CompactionSummary>,
) -> (Vec<AssertionObservation>, Vec<AssertionObservation>) {
    use AssertionId::*;
    let required = [Scope, CurrentTarget, UnresolvedWork, NoDeploy];
    let canaries: &[_] = if cycle.forbidden.len() == 3 {
        &[OriginalTarget, ToolScope, ToolCompletion]
    } else {
        &[
            OriginalTarget,
            SupersededTarget,
            SupersededUnresolvedWork,
            ToolScope,
            ToolCompletion,
        ]
    };
    let observe = |ids: &[AssertionId], values: &[String]| {
        ids.iter()
            .zip(values)
            .map(|(&id, value)| {
                let mut active_fields = Vec::new();
                let mut references_hit = false;
                if let Some(summary) = summary {
                    use ActiveField::*;
                    if summary
                        .goal
                        .as_ref()
                        .is_some_and(|text| text.contains(value))
                    {
                        active_fields.push(Goal);
                    }
                    for (field, texts) in [
                        (Constraints, &summary.constraints),
                        (Progress, &summary.progress),
                        (Decisions, &summary.decisions),
                        (Unresolved, &summary.unresolved),
                    ] {
                        if texts.iter().any(|text| text.contains(value)) {
                            active_fields.push(field);
                        }
                    }
                    references_hit = summary.references.iter().any(|text| text.contains(value));
                }
                AssertionObservation {
                    id,
                    active_fields,
                    references_hit,
                }
            })
            .collect()
    };
    (
        observe(&required, cycle.required),
        observe(canaries, cycle.forbidden),
    )
}

/// Constructed only for a checked-in case. No provider response body is retained.
#[derive(Serialize)]
pub(crate) struct SyntheticInspection {
    case_id: String,
    summaries: Vec<InspectedSummary>,
}

#[derive(Serialize)]
struct InspectedSummary {
    cycle: usize,
    summary: CompactionSummary,
}

impl SyntheticInspection {
    pub(super) fn new(case_id: &str) -> Self {
        Self {
            case_id: case_id.into(),
            summaries: Vec::new(),
        }
    }

    pub(super) fn record(&mut self, cycle: usize, summary: &CompactionSummary) {
        // Preserve the generation validator's bounds even at this separate
        // output seam. The fixed corpus has at most two cycles per case.
        if (1..=2).contains(&cycle)
            && self.summaries.len() < 2
            && summary != &CompactionSummary::default()
            && super::super::validate_summary(summary, SUMMARY_MAX_BYTES)
        {
            self.summaries.push(InspectedSummary {
                cycle,
                summary: summary.clone(),
            });
        }
    }
}
