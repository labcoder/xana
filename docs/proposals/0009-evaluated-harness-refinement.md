# Evaluated harness refinement

> Audience: Contributors and coding agents  
> Authority: None  
> Status: Proposed

## Context

Prompts, memories, skills, routes, hooks, and child definitions can improve as
Xana observes real work, but model-generated rationale is not validation. A
runtime-owned refinement process must preserve versions and provenance,
evaluate changes against held-out work, prevent stale promotion, and support
rollback.

This proposal does not authorize automatic project/global self-modification.
It separates proposal, observed evidence, promotion, activation, and rollback
so each target can apply an appropriate policy.

## Proposed records

Conceptually:

```rust
struct HarnessPatchProposal {
    proposal_id: ProposalId,
    base_revision: Revision,
    target: HarnessTarget,
    scope: RefinementScope,
    source_trajectory: Vec<ArtifactRef>,
    changes: Vec<HarnessChange>,
    rationale: String,
    risk: RiskClass,
    declared_validation: ValidationPlan,
}

struct ValidationResult {
    proposal_id: ProposalId,
    evaluator_revision: Revision,
    task_set: ArtifactRef,
    observations: Vec<MetricObservation>,
    regressions: Vec<Regression>,
    verdict: ValidationVerdict,
}
```

The exact types wait for artifact, operation, and evaluation formats. Required
semantics are:

- the immutable base and proposed patch have explicit revisions;
- source trajectories and task sets are artifact references with provenance;
- declared validation is distinct from an observed result;
- results record evaluator/harness revisions, repeated observations, failures,
  and regressions rather than only a summary verdict;
- promotion checks the base revision with compare-and-swap through one runtime
  owner; and
- every active change has a rollback record.

## Proposed lifecycle

```text
proposed -> validated -> promoted -> active
     |          |           |          |
     v          v           v          v
 rejected    rejected    rejected   rolled_back
```

Proposal-only mode records suggestions without applying them. Shadow mode runs
the candidate against held-out work without changing live behavior. Promotion
is a separate decision after validation; activation records the effective
revision. A changed base, stale evaluator, contaminated task set, or missing
result blocks promotion.

## Target policies

| Target | Default policy |
|---|---|
| Session-local, non-executable memory | Proposal or shadow mode; limited auto-promotion only after an accepted validation policy exists |
| Project prompt, memory, or route | Explicit promotion after held-out regression checks |
| User/global configuration | Explicit promotion with a visible rollback path |
| Executable skill, tool, hook, or code | Never auto-promote; require authorization and contained validation |
| Security or permission policy | Outside agent self-refinement authority |

Inputs retain provenance through refinement. A web page, repository
instruction, tool result, or extracted document cannot silently promote itself
into governing policy. A model or extension cannot widen the authority of the
target it proposes to change.

## Evaluation contract

Evaluation uses held-out tasks, fixed total budgets, exact model/provider and
harness revisions, repeated trials for stochastic systems, failure disclosure,
and distribution-shift/regression cases. Metrics may include task quality,
cost, latency, policy violations, acceptance rate, held-out delta, rollback
rate, and variance across runs.

Reported results identify the task-set policy, selection rule, prompts, depth,
fan-out, token/time limits, and unsuccessful trajectories. Public benchmark
wins alone do not justify a default product change.

Evaluation artifacts and revision state use the ownership model in
[Proposal 0007](0007-state-ownership-and-configuration.md). Any executable
validation crosses the authority and containment boundaries in
[Proposal 0003](0003-tool-authority-and-execution.md).

## Open questions

- Which session-local memory changes could ever qualify for automatic
  promotion?
- How are evaluator and task-set revisions pinned and reproduced?
- What held-out Xana task suite resists leakage and reward shaping?
- Which conflicts require manual merge rather than patch rejection?
- How long are inactive, rejected, and rolled-back revisions retained?

## Research basis

- [Continual Learning of Agentic Harnesses paper](https://arxiv.org/abs/2605.09998)
- [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent)
