# Protect managed content with independent recovery

> Audience: Contributors and coding agents
> Authority: Historical rationale
> Status: Accepted

## Decision

Protect all Xana-managed content at rest, not just a personal-memory table.
Normal access uses OS key custody; recovery must also work with a portable
user-held secret independent of the original machine. No automatic plaintext
mirror or insecure fallback. The exact library remains an evidence-gated choice.

## Tradeoff

Selective memory encryption is simpler, but the same private facts remain in
history, artifacts, search indexes and backups. A live plaintext Markdown mirror
would similarly defeat the boundary. Full managed-content protection costs
migration, native key-store integration, recovery UX and fault testing, and
cannot defend against a compromised unlocked session. That cost is accepted
because an agent retaining personal knowledge must not offer misleading privacy.

OS-only custody makes normal use easy but turns machine loss into data loss.
User-held recovery adds setup responsibility while avoiding vendor escrow.
User-owned source files and non-secret configuration remain ordinary, so this
choice does not require authentication to edit files in another application.

## Consequences

A library experiment and four-target evidence precede dependency selection.
Reviewed migration fences old writers, verifies activation and preserves
recoverability. Locking must settle or record uncertain work before dropping
usable keys. Backups and restore obey forgetting exclusions; external copies
and OS/vendor stores remain explicit exceptions, not erased by policy wording.

See the [accepted storage contract](../proposals/0024-encrypted-managed-content-and-recovery.md).

