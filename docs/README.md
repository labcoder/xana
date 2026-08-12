# Xana documentation

Xana keeps documentation for people using the program separate from the
engineering contracts used to change it.

## Using Xana

- [Installation, updates, verification, and removal](user/installation.md)
  explains preview installers, exact targets and trust limits, manual
  verification, locked source alternatives, setup receipts, updates,
  troubleshooting, and state-preserving removal.
- [Configuration](user/configuration.md) explains setup, provider/profile
  settings, platform paths, `XANA_HOME`, doctor, validated editing, and scoped
  reset.
- [Project context and system prompt](user/project-context.md) explains the
  built-in prompt, root `AGENTS.md` discovery, input budgets, and instruction
  boundaries.
- [Permissions](user/permissions.md) explains deny/ask/allow policy, scoped
  session grants, controller decisions, and the lack of containment.
- [Sessions](user/sessions.md) explains durable history, explicit resume,
  immutable artifacts, inspection, corruption handling, and backup limits.
- [Projects](user/projects.md) explains optional local project identity,
  lifecycle, Ungrouped conversations, membership, and continuation review.
- [Agent Skills](user/skills.md) explains standards-compatible discovery,
  qualification, progressive activation, profile selection, provenance, and
  the no-authority boundary.
- [Agent Plugins](user/plugins.md) explains inert review, exact local/Git
  acquisition, content-addressed disabled installation, linked development
  mode, supply-chain bounds, and private recovery state.
- [Outbound data approvals and privacy](user/outbound-data.md) explains typed
  data classes, exact recipient approval, saved decisions, fail-closed
  noninteractive behavior, and content-free audit records.
- [Model Context Protocol catalog and compatibility](user/mcp.md) explains the
  pinned modern protocol version, qualified identities, profile allowlists,
  progressive discovery bounds, and the current no-transport limit.
- [Plain and one-shot modes](user/automation.md) explains terminal surface
  selection, pipelines, JSON envelopes, continuation, and stable exit codes.
- [Terminal presentation](user/presentation.md) explains semantic styling,
  terminal fallbacks, `NO_COLOR`, reduced motion, and machine-local
  preferences.
- [Full-screen terminal UI](user/tui.md) explains composer presets, portable
  keys, safe paste, follow-ups, command/model controls, and owner-specific
  limitations.
- [Local foreground host](user/local-host.md) explains loopback-only serving,
  capability discovery, passive observer attachment, sequence boundaries, and
  the repository-private transport.
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
- [Accepted projects, profiles, and portable configuration](proposals/0019-projects-profiles-and-portable-configuration.md)
  defines optional project ownership, frozen arbitrary profiles, portable
  non-secret project configuration, instruction precedence, and transactional
  migration for Milestone 3. It is prescriptive but not yet implemented.
- [Accepted standards interoperability and external-agent boundaries](proposals/0020-standards-interoperability-and-external-agents.md)
  defines Agent Skills and Agent Plugins v1, bounded MCP client/server roles,
  A2A delegation, outbound-data policy, and supervised lifecycle ownership. It
  is prescriptive but not yet implemented.
- [Accepted focused multimodal services and routing](proposals/0021-focused-multimodal-services-and-routing.md)
  defines exact named image-generation and specialist-vision routes separate
  from conversational providers. It is prescriptive but not yet implemented.
- [Implemented Release Preview](proposals/0018-release-preview-distribution.md)
  records the bounded four-target native preview, source-controlled installers,
  Xana-owned readiness handoff, attributable draft assembly, and explicit
  Product Distribution deferrals. Architecture and User Documentation own the
  shipped contract.
- [Implemented Phase 5 local frontend and workspace host](proposals/0017-bounded-local-frontends-and-workspace-host.md)
  records the historical bounded embedded-client, TUI, loopback-host,
  presentation, attachment-security, backpressure, and shutdown decision.
  Architecture and User Documentation now own the shipped contract.
- [Implemented Phase 4 orchestration](proposals/0016-bounded-child-orchestration-and-task-routes.md)
  records the historical bounded-child, exact-route, report, budget,
  native/managed owner, and orchestration-plan decision. Architecture and User
  Documentation now own the shipped contract.
- [Code organization](development/code-organization.md) defines the repository
  policy for modules, tests, comments, formatting, and tooling.
- [Xana 0.5.0 architecture closure evidence](development/architecture-closure-evidence-0.5.0.md)
  records the one-package boundary audit, canonical module vocabulary, local
  verification, and deliberately deferred public extraction.
- [Release Preview development](development/release-preview.md) documents the
  pinned native plan, verified installers, local audits, and draft-only release
  workflow without implying publication.
- [Release Preview draft review](development/release-review-checklist.md) is the
  owner checklist for an exact complete unpublished bundle.
- [Xana 0.5.0 Release Preview evidence](development/release-preview-evidence-0.5.0.md)
  separates passed local candidate gates from still-unexecuted remote,
  provenance, draft, and publication evidence.
- [Release notes](releases/) are versioned source inputs to release assembly,
  not evidence that the corresponding release has been published.
- [Xana 0.5.1 release notes](releases/0.5.1.md) describe the first published
  Release Preview and its unchanged unsigned-preview trust limits.
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
