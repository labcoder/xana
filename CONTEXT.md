# Xana Documentation

This glossary defines the audiences and authority carried by Xana's product
documentation.

## Language

**Architecture**:
The descriptive engineering contract for behavior and boundaries that are
demonstrably present in the Xana repository now. Its documents contain only
facts about what exists and how it works, although they may link to related
future proposals. Observable code and tests are evidence of present reality; a
disagreement is a documentation defect rather than a reason to preserve the
description by changing code.
_Avoid_: Architecture snapshot, implemented design, architecture roadmap

**Design Principle**:
A durable, cross-cutting constraint that future Xana work follows unless the
principle is explicitly reconsidered. It constrains multiple features and
survives several implementations rather than specifying one particular change.
_Avoid_: Preference, aspiration

**Proposal**:
A document describing a particular change or future system shape. Its status
determines its authority: Proposed has none, Accepted is prescriptive, and
Implemented, Rejected, Withdrawn, or Superseded is historical.
_Avoid_: Architecture, Design Principle, roadmap

**Architecture Decision Record (ADR)**:
A sparse explanation of why Xana made a consequential architecture choice that
is costly to reverse, surprising without context, and involved a genuine
tradeoff. Architecture, a Design Principle, or an Accepted Proposal states the
resulting contract.
_Avoid_: Feature specification, changelog, architecture contract

**User Documentation**:
Task-oriented guidance and reference material for people installing,
configuring, or using behavior that Xana currently ships.
_Avoid_: External docs, product roadmap

**Engineering Documentation**:
Public architecture, principles, proposals, decisions, and development guidance
for Xana contributors and coding agents.
_Avoid_: Internal docs, user guide
