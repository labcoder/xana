# Xana

This glossary defines Xana's shared domain terms and documentation authority.

## Language

### Conversation and execution

**Conversation**: A continuous exchange with a stable identity and retained history;
changing its available capabilities does not create a different Conversation.

**Execution configuration**: The resolved configuration revision governing an
operation, fixed for that operation even when later work uses newer settings.
_Avoid_: Permanently frozen Conversation settings

### Personal memory

**Personal memory**: Durable scoped knowledge about the user, separate from task
history, compaction, authored instructions and permission grants.
_Avoid_: History, user-prefs file, identity prompt

**Memory scope**: The boundary in which a fact may be used: User (all eligible
Conversations), Profile-private, Project, or one Conversation. Scope does not
describe how the fact was acquired.
_Avoid_: Separate global/learned stores

**Stated memory**: A fact attributed to owner-authored input; **inferred memory**
is a model's interpretation and must not silently become an owner statement.

**Learned memory**: Memory proposed by the separately authorized background
learner, with source evidence and review state. It is not synonymous with global
User scope or with an explicit foreground save.

**Memory receipt**: The result of the governed memory operation, distinguishing
a committed change from denied, unavailable or unchanged outcomes; model prose
alone is not evidence of persistence.

### Documentation

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
