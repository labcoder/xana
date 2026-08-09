# Xana documentation

Xana keeps documentation for people using the program separate from the
engineering contracts used to change it.

## Using Xana

- [Source installation](user/installation.md) explains prerequisites,
  checkout and Git installation, platform `XANA_HOME` syntax, updates, and
  uninstall behavior.
- [Configuration](user/configuration.md) explains initialization, provider and
  profile settings, platform paths, `XANA_HOME`, and configuration diagnostics.
- [Project context and system prompt](user/project-context.md) explains the
  built-in prompt, root `AGENTS.md` discovery, input budgets, and instruction
  boundaries.
- [Permissions](user/permissions.md) explains deny/ask/allow policy, scoped
  session grants, controller decisions, and the lack of containment.
- [Sessions](user/sessions.md) explains durable history, explicit resume,
  immutable artifacts, inspection, corruption handling, and backup limits.
- [Plain and one-shot modes](user/automation.md) explains terminal surface
  selection, pipelines, JSON envelopes, continuation, and stable exit codes.
- [Terminal presentation](user/presentation.md) explains semantic styling,
  terminal fallbacks, `NO_COLOR`, reduced motion, and machine-local
  preferences.
- [Full-screen terminal UI](user/tui.md) explains composer presets, portable
  keys, safe paste, follow-ups, command/model controls, and owner-specific
  limitations.
- [Operation recovery](user/operations.md) explains read-only recovery plans,
  explicit reconciliation, replay safety, and unknown effect outcomes.
- [Child orchestration](user/orchestration.md) explains exact native routes,
  runtime-owned children, active/offline inspection, cancellation and timeout
  semantics, attributed lifecycle, bounded reports, and current limits.
- [Orchestration plans](user/orchestration-plans.md) explains the closed,
  versioned spawn/await/collect/cancel graph and its validation boundary.
- The repository [README](../README.md) is the user-first starting point for a
  source checkout and the shipped CLI.

User Documentation describes behavior available in the executable. It may
state a limitation, but it does not present a proposal as an upcoming feature.

## Engineering Xana

- [Architecture](architecture/README.md) describes what exists and how it
  works.
- [Connections, models, and managed runtimes](architecture/models-and-managed-runtimes.md)
  describes native providers, Codex delegation, catalogs, model/reasoning
  selection, typed activity, opaque managed-thread resumption, and credential
  ownership.
- [Design Principles](principles.md) defines the durable constraints future
  work follows unless they are explicitly reconsidered.
- [Proposals](proposals/) contains particular future designs and their
  lifecycle status.
- [Accepted Phase 5 local frontend and workspace host](proposals/0017-bounded-local-frontends-and-workspace-host.md)
  is the prescriptive contract for the bounded embedded client, TUI, loopback
  host, presentation preferences, attachment security, backpressure, and
  shutdown work that is not yet fully implemented.
- [Implemented Phase 4 orchestration](proposals/0016-bounded-child-orchestration-and-task-routes.md)
  records the historical bounded-child, exact-route, report, budget,
  native/managed owner, and orchestration-plan decision. Architecture and User
  Documentation now own the shipped contract.
- [Code organization](development/code-organization.md) defines the repository
  policy for modules, tests, comments, formatting, and tooling.
- [Xana Documentation](../CONTEXT.md) is the glossary for documentation
  authority and audience terms.
- [Architecture decisions](adr/README.md) contains sparse rationale for
  consequential choices.

Architecture Decision Records are created under `docs/adr/` only when a choice
is costly to reverse, surprising without context, and the result of a genuine
tradeoff. Architecture, a principle, or an accepted proposal states the
contract; an ADR explains why a consequential contract exists.

## Authority model

| Artifact | Audience | Authority |
|---|---|---|
| Architecture | Contributors and coding agents | Descriptive: de facto behavior and boundaries |
| Design Principles | Contributors and coding agents | Prescriptive: durable constraints and philosophies |
| Accepted Proposal | Contributors and coding agents | Prescriptive: an approved but unimplemented change |
| Other Proposal states | Contributors and coding agents | None; historical or exploratory |
| User Documentation | People installing, configuring, or using Xana | Shipped behavior only |
| Development Documentation | Contributors and coding agents | Repository policy |

Code and tests are evidence of what Xana does. If Architecture or User
Documentation disagrees with that evidence, the documentation is defective;
do not change behavior merely to preserve a descriptive claim.

## Proposal lifecycle

Proposal filenames use a stable four-digit identifier and a descriptive slug.
The sequence provides an address, not a priority or delivery order.

| Status | Meaning | Authority |
|---|---|---|
| Proposed | Under consideration | None |
| Accepted | Approved for future implementation | Prescriptive |
| Implemented | Reflected in code and Architecture | Historical |
| Rejected | Considered and declined | Historical |
| Withdrawn | Removed by its author before a decision | Historical |
| Superseded | Replaced by an identified proposal | Historical |

Accepting a proposal requires one repository change that marks it Accepted and
records the complete prescriptive design. Updating only a status label is not
enough. If the decision meets the ADR threshold, that change also records its
rationale in a numbered ADR.

Implementing a proposal requires the same change to update affected
Architecture and User Documentation and mark the proposal Implemented. Once
implemented, Architecture owns the resulting description and the proposal is
historical evidence.

## Keeping documentation accurate

Update documentation in the same change that alters what it describes:

- Update Architecture when responsibilities, dependencies, invariants, data
  flow, or externally meaningful limitations change.
- Update User Documentation when installation, CLI, configuration, paths,
  diagnostics, or visible behavior changes.
- Update a Proposal when its design or lifecycle status changes.
- Change a Design Principle only through an explicit architecture decision,
  not merely because implementation drifted away from it.
- Add or supersede an ADR only when the sparse-ADR threshold is met.

A refactor that preserves documented behavior and boundaries needs no
documentation change. Documents do not carry review dates or commit hashes;
same-change maintenance and Git history provide provenance.

Architecture documents and proposals begin with visible audience, authority,
and status lines as applicable. User documents begin with a visible audience
line. No additional metadata tooling is required.
