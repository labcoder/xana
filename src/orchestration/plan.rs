use super::{
    AgentHandleSnapshot, AwaitAgentOutcome, ChildCancellationReceipt, CollectAgentsOptions,
    CollectionFailurePolicy, CollectionResult, SpawnAgentRequest,
};
use crate::identity::{AgentId, OrchestrationPlanId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, time::Duration};

pub(crate) const ORCHESTRATION_PLAN_VERSION: u32 = 1;
pub(crate) const MAX_PLAN_BYTES: usize = 256 * 1024;
pub(crate) const MAX_PLAN_STEPS: usize = 64;
pub(crate) const MAX_PLAN_CHILDREN: usize = 64;
pub(crate) const MAX_PLAN_RESULT_BYTES: usize = 256 * 1024;
const MAX_STEP_ID_BYTES: usize = 64;
const MAX_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrchestrationPlan {
    pub(crate) version: u32,
    pub(crate) plan_id: OrchestrationPlanId,
    pub(crate) steps: Vec<OrchestrationPlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operator", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OrchestrationPlanStep {
    Spawn {
        id: String,
        requests: Vec<SpawnAgentRequest>,
    },
    Await {
        id: String,
        handle: PlanHandleRef,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        cancel_on_timeout: bool,
    },
    Collect {
        id: String,
        handles: Vec<PlanHandleRef>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        failure_policy: CollectionFailurePolicy,
        #[serde(default)]
        cancel_remaining: bool,
        #[serde(default)]
        cancel_on_timeout: bool,
    },
    Cancel {
        id: String,
        handle: PlanHandleRef,
    },
}

impl OrchestrationPlanStep {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Spawn { id, .. }
            | Self::Await { id, .. }
            | Self::Collect { id, .. }
            | Self::Cancel { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanHandleRef {
    pub(crate) step: String,
    #[serde(default)]
    pub(crate) index: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedOrchestrationPlan {
    pub(crate) plan: OrchestrationPlan,
    pub(crate) fingerprint: String,
    pub(crate) spawn_requests: Vec<SpawnAgentRequest>,
    pub(crate) spawn_ranges: BTreeMap<String, std::ops::Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanValidationDiagnostic {
    pub(crate) version: u32,
    pub(crate) plan_id: OrchestrationPlanId,
    pub(crate) fingerprint: String,
    pub(crate) step_count: usize,
    pub(crate) child_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrchestrationPlanStart {
    pub(crate) plan_id: OrchestrationPlanId,
    pub(crate) operation_id: crate::identity::OperationId,
    pub(crate) fingerprint: String,
    pub(crate) step_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrchestrationPlanResult {
    pub(crate) version: u32,
    pub(crate) plan_id: OrchestrationPlanId,
    pub(crate) fingerprint: String,
    pub(crate) steps: Vec<OrchestrationPlanStepResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operator", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OrchestrationPlanStepResult {
    Spawn {
        id: String,
        handles: Vec<AgentHandleSnapshot>,
    },
    Await {
        id: String,
        outcome: AwaitAgentOutcome,
    },
    Collect {
        id: String,
        result: CollectionResult,
    },
    Cancel {
        id: String,
        receipt: Box<ChildCancellationReceipt>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanValidationError {
    TooLarge {
        actual: usize,
        maximum: usize,
    },
    UnsupportedVersion(u32),
    Empty,
    TooManySteps {
        requested: usize,
        maximum: usize,
    },
    InvalidStepId(String),
    DuplicateStepId(String),
    EmptySpawn(String),
    TooManyChildren {
        requested: usize,
        maximum: usize,
    },
    InvalidTimeout {
        step: String,
        timeout_ms: u64,
    },
    EmptyCollection(String),
    DuplicateReference {
        step: String,
        reference: PlanHandleRef,
    },
    UnknownOrForwardReference {
        step: String,
        reference: PlanHandleRef,
    },
}

impl fmt::Display for PlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, maximum } => write!(
                formatter,
                "plan contains {actual} bytes, exceeding maximum {maximum}"
            ),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "unsupported orchestration plan version {version}"
            ),
            Self::Empty => formatter.write_str("orchestration plan must contain at least one step"),
            Self::TooManySteps { requested, maximum } => write!(
                formatter,
                "plan requests {requested} steps, exceeding maximum {maximum}"
            ),
            Self::InvalidStepId(id) => write!(formatter, "plan step id {id:?} is invalid"),
            Self::DuplicateStepId(id) => write!(formatter, "plan step id {id:?} is duplicated"),
            Self::EmptySpawn(id) => write!(formatter, "spawn step {id:?} has no requests"),
            Self::TooManyChildren { requested, maximum } => write!(
                formatter,
                "plan requests {requested} children, exceeding maximum {maximum}"
            ),
            Self::InvalidTimeout { step, timeout_ms } => write!(
                formatter,
                "plan step {step:?} has timeout {timeout_ms}ms outside 1..={MAX_TIMEOUT_MS}"
            ),
            Self::EmptyCollection(id) => write!(formatter, "collect step {id:?} has no handles"),
            Self::DuplicateReference { step, reference } => write!(
                formatter,
                "plan step {step:?} repeats handle {}[{}]",
                reference.step, reference.index
            ),
            Self::UnknownOrForwardReference { step, reference } => write!(
                formatter,
                "plan step {step:?} references unknown or forward handle {}[{}]",
                reference.step, reference.index
            ),
        }
    }
}

impl std::error::Error for PlanValidationError {}

pub(crate) fn validate_plan_structure(
    plan: OrchestrationPlan,
) -> Result<ValidatedOrchestrationPlan, PlanValidationError> {
    let encoded = serde_json::to_vec(&plan).expect("plan serialization is infallible");
    if encoded.len() > MAX_PLAN_BYTES {
        return Err(PlanValidationError::TooLarge {
            actual: encoded.len(),
            maximum: MAX_PLAN_BYTES,
        });
    }
    if plan.version != ORCHESTRATION_PLAN_VERSION {
        return Err(PlanValidationError::UnsupportedVersion(plan.version));
    }
    if plan.steps.is_empty() {
        return Err(PlanValidationError::Empty);
    }
    if plan.steps.len() > MAX_PLAN_STEPS {
        return Err(PlanValidationError::TooManySteps {
            requested: plan.steps.len(),
            maximum: MAX_PLAN_STEPS,
        });
    }

    let mut seen = BTreeMap::<String, Option<std::ops::Range<usize>>>::new();
    let mut spawn_requests = Vec::new();
    let mut spawn_ranges = BTreeMap::new();
    for step in &plan.steps {
        let id = step.id();
        if id.is_empty()
            || id.len() > MAX_STEP_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(PlanValidationError::InvalidStepId(id.to_owned()));
        }
        if seen.contains_key(id) {
            return Err(PlanValidationError::DuplicateStepId(id.to_owned()));
        }
        match step {
            OrchestrationPlanStep::Spawn { requests, .. } => {
                if requests.is_empty() {
                    return Err(PlanValidationError::EmptySpawn(id.to_owned()));
                }
                let start = spawn_requests.len();
                spawn_requests.extend(requests.iter().cloned());
                if spawn_requests.len() > MAX_PLAN_CHILDREN {
                    return Err(PlanValidationError::TooManyChildren {
                        requested: spawn_requests.len(),
                        maximum: MAX_PLAN_CHILDREN,
                    });
                }
                let range = start..spawn_requests.len();
                spawn_ranges.insert(id.to_owned(), range.clone());
                seen.insert(id.to_owned(), Some(range));
            }
            OrchestrationPlanStep::Await {
                handle, timeout_ms, ..
            } => {
                validate_timeout(id, *timeout_ms)?;
                validate_reference(id, handle, &seen)?;
                seen.insert(id.to_owned(), None);
            }
            OrchestrationPlanStep::Collect {
                handles,
                timeout_ms,
                ..
            } => {
                validate_timeout(id, *timeout_ms)?;
                if handles.is_empty() {
                    return Err(PlanValidationError::EmptyCollection(id.to_owned()));
                }
                let mut unique = std::collections::BTreeSet::new();
                for handle in handles {
                    validate_reference(id, handle, &seen)?;
                    if !unique.insert((handle.step.as_str(), handle.index)) {
                        return Err(PlanValidationError::DuplicateReference {
                            step: id.to_owned(),
                            reference: handle.clone(),
                        });
                    }
                }
                seen.insert(id.to_owned(), None);
            }
            OrchestrationPlanStep::Cancel { handle, .. } => {
                validate_reference(id, handle, &seen)?;
                seen.insert(id.to_owned(), None);
            }
        }
    }
    let fingerprint = blake3::hash(&encoded).to_hex().to_string();
    Ok(ValidatedOrchestrationPlan {
        plan,
        fingerprint,
        spawn_requests,
        spawn_ranges,
    })
}

fn validate_timeout(step: &str, timeout_ms: Option<u64>) -> Result<(), PlanValidationError> {
    if let Some(timeout_ms) = timeout_ms
        && !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms)
    {
        return Err(PlanValidationError::InvalidTimeout {
            step: step.to_owned(),
            timeout_ms,
        });
    }
    Ok(())
}

fn validate_reference(
    step: &str,
    reference: &PlanHandleRef,
    seen: &BTreeMap<String, Option<std::ops::Range<usize>>>,
) -> Result<(), PlanValidationError> {
    let Some(Some(range)) = seen.get(&reference.step) else {
        return Err(PlanValidationError::UnknownOrForwardReference {
            step: step.to_owned(),
            reference: reference.clone(),
        });
    };
    if reference.index >= range.len() {
        return Err(PlanValidationError::UnknownOrForwardReference {
            step: step.to_owned(),
            reference: reference.clone(),
        });
    }
    Ok(())
}

pub(crate) fn resolve_handle(
    reference: &PlanHandleRef,
    ranges: &BTreeMap<String, std::ops::Range<usize>>,
    handles: &[AgentHandleSnapshot],
) -> AgentId {
    let range = &ranges[&reference.step];
    handles[range.start + reference.index]
        .admission
        .attribution
        .agent_id
}

pub(crate) fn collect_options(
    timeout_ms: Option<u64>,
    failure_policy: CollectionFailurePolicy,
    cancel_remaining: bool,
    cancel_on_timeout: bool,
) -> CollectAgentsOptions {
    CollectAgentsOptions {
        timeout: timeout_ms.map(Duration::from_millis),
        failure_policy,
        cancel_remaining,
        cancel_on_timeout,
    }
}

pub(crate) fn await_options(
    timeout_ms: Option<u64>,
    cancel_on_timeout: bool,
) -> super::AwaitAgentOptions {
    super::AwaitAgentOptions {
        timeout: timeout_ms.map(Duration::from_millis),
        cancel_on_timeout,
    }
}

pub(crate) fn validation_diagnostic(
    validated: &ValidatedOrchestrationPlan,
) -> PlanValidationDiagnostic {
    PlanValidationDiagnostic {
        version: ORCHESTRATION_PLAN_VERSION,
        plan_id: validated.plan.plan_id,
        fingerprint: validated.fingerprint.clone(),
        step_count: validated.plan.steps.len(),
        child_count: validated.spawn_requests.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{ChildRestrictions, ChildResultSchema};

    fn request(task: &str) -> SpawnAgentRequest {
        SpawnAgentRequest {
            route: Some("worker".to_owned()),
            task: task.to_owned(),
            result_schema: ChildResultSchema::Summary,
            restrictions: ChildRestrictions::default(),
            handoff: Default::default(),
        }
    }

    fn plan(steps: Vec<OrchestrationPlanStep>) -> OrchestrationPlan {
        OrchestrationPlan {
            version: ORCHESTRATION_PLAN_VERSION,
            plan_id: OrchestrationPlanId::new(),
            steps,
        }
    }

    #[test]
    fn versioned_plan_round_trips_and_resolves_only_prior_spawn_handles() {
        let plan = plan(vec![
            OrchestrationPlanStep::Spawn {
                id: "workers".to_owned(),
                requests: vec![request("one"), request("two")],
            },
            OrchestrationPlanStep::Collect {
                id: "results".to_owned(),
                handles: vec![
                    PlanHandleRef {
                        step: "workers".to_owned(),
                        index: 1,
                    },
                    PlanHandleRef {
                        step: "workers".to_owned(),
                        index: 0,
                    },
                ],
                timeout_ms: Some(1_000),
                failure_policy: CollectionFailurePolicy::ContinueOnError,
                cancel_remaining: false,
                cancel_on_timeout: false,
            },
        ]);
        let encoded = serde_json::to_vec(&plan).expect("encode plan");
        let decoded: OrchestrationPlan = serde_json::from_slice(&encoded).expect("decode plan");
        assert_eq!(decoded, plan);
        let validated = validate_plan_structure(plan).expect("valid plan");
        assert_eq!(validated.spawn_requests.len(), 2);
        assert_eq!(validated.spawn_ranges["workers"], 0..2);
    }

    #[test]
    fn validator_rejects_forward_duplicate_and_unknown_operator_shapes() {
        let forward = plan(vec![
            OrchestrationPlanStep::Await {
                id: "early".to_owned(),
                handle: PlanHandleRef {
                    step: "later".to_owned(),
                    index: 0,
                },
                timeout_ms: None,
                cancel_on_timeout: false,
            },
            OrchestrationPlanStep::Spawn {
                id: "later".to_owned(),
                requests: vec![request("work")],
            },
        ]);
        assert!(matches!(
            validate_plan_structure(forward),
            Err(PlanValidationError::UnknownOrForwardReference { .. })
        ));

        let duplicate = plan(vec![
            OrchestrationPlanStep::Spawn {
                id: "same".to_owned(),
                requests: vec![request("one")],
            },
            OrchestrationPlanStep::Spawn {
                id: "same".to_owned(),
                requests: vec![request("two")],
            },
        ]);
        assert!(matches!(
            validate_plan_structure(duplicate),
            Err(PlanValidationError::DuplicateStepId(_))
        ));

        let unknown = serde_json::json!({
            "version": 1,
            "plan_id": OrchestrationPlanId::new(),
            "steps": [{"operator":"loop", "id":"never"}]
        });
        assert!(serde_json::from_value::<OrchestrationPlan>(unknown).is_err());
    }
}
