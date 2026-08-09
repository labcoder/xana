# Orchestration plans

> Audience: People using Xana to run a fixed bounded child graph.

An `OrchestrationPlan` is versioned JSON for a closed set of child
`spawn`, `await`, `collect`, and `cancel` operations. It is distinct from
Xana's internal `ContextPlan`, which selects prompt context and does not start
agents.

The native root agent can call `validate_orchestration_plan` to check a plan
without admitting work, then `execute_orchestration_plan` to run it. You can
ask Xana to construct and validate one, or provide JSON such as:

```json
{
  "version": 1,
  "plan_id": "8cd197dd-78a7-49ed-929b-cd24c443f651",
  "steps": [
    {
      "operator": "spawn",
      "id": "reviews",
      "requests": [
        {"route": "worker", "task": "Review configuration safety."},
        {"route": "worker", "task": "Review orchestration tests."}
      ]
    },
    {
      "operator": "collect",
      "id": "results",
      "handles": [
        {"step": "reviews", "index": 0},
        {"step": "reviews", "index": 1}
      ],
      "failure_policy": "continue_on_error"
    }
  ]
}
```

Step ids are unique stable names. A handle reference can target only a prior
spawn step and a valid zero-based output index, making the graph acyclic by
construction. One spawn step is a fixed parallel group. Xana validates the
complete serialized plan, exact routes, result schemas, authority, references,
and aggregate runtime budget before its first durable effect. Validation is
repeatable and commits nothing.

Execution durably records the plan id and fingerprint, then atomically admits
the plan's complete static spawn set through the same supervisor and budget
ledger used by `spawn_many`. Later await, collect, and cancel steps use the
same direct operations. Every child handle retains its plan id, spawn step id,
and output index. Reusing a plan id that has started is rejected, including
after session restoration, rather than silently admitting duplicate work.

Plans are capped at 256 KiB, 64 steps, and 64 total children; the configured
root budget can be narrower. The returned model-facing result is independently
capped at 256 KiB. Xana charges each encoded step before retaining it, so a
later oversized await or collection cannot first accumulate a much larger
transient result. Plans have no loops, conditions, expressions, dynamic spawns,
recursion, general filter/reduce, arbitrary code, or evaluator state. Timeout
and cancellation remain explicit, and partial child evidence stays in the
durable session if a later plan step fails.
