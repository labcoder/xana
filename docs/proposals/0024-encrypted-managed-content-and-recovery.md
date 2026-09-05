# Encrypted managed content and portable recovery

> Audience: Contributors and coding agents
> Authority: Prescriptive
> Status: Accepted

## Boundary

Encrypt Xana-managed personal/task memory, native Conversation content,
artifacts, content-bearing indexes and work payloads, recovery snapshots and
automatic backups. Include derived previews, titles/paths/source metadata,
journals/WAL, temporary files and diagnostic/crash paths in the spill inventory.
Document residual non-content metadata and access-pattern leakage.

User-owned source files and ordinary non-secret configuration remain ordinary.
Credentials retain their existing owner. Vendor Codex/provider stores, browser
profiles/caches, OS swap/dumps/filesystem snapshots and intentional plaintext
exports are separate disclosed boundaries. No automatic plaintext memory mirror.
At-rest encryption does not secure a compromised unlocked user session.

## Keys and recovery

Use OS-protected custody for normal unlock without per-turn or per-file
authentication. Provide manual unlock when custody is unavailable and portable
user-held recovery independent of the original machine. No vendor escrow,
cloud-account requirement or plaintext fallback. An unlocked store changes no
tool, client or source authority.

Locking stops protected admission, pauses queued protected jobs, requests bounded
safe cancellation/checkpoints, persists stopping/uncertain receipts while keys
remain usable, detaches sensitive projections, then releases usable keys and
reports Locked. Do not promise to erase every process copy or unsend context.
Restart reconciles state and key availability, never replays uncertain effects.
Startup before user login does not imply OS custody can unlock.

The library choice remains gated on a runnable feasibility proof: transactional
encrypted records, bounded lexical/FTS search, bounded artifact ranges, tamper/
truncation rejection, cancellation, journal/temp canary scans, interrupted-write
recovery, licenses, exact native dependencies, package/resource costs and native
Windows x64, macOS ARM64/Intel and Linux x64 glibc evidence. No custom crypto.
An isolated experiment is not production dependency acceptance. A failed gate
must be resolved or brought to the owner, not weakened silently.

## Migration and backup

Use explicit format generations and one writer. Preflight space, custody and
independent recovery; fence old writers; preserve IDs/artifact references;
verify the encrypted generation before atomic activation. Inject failures at
write/flush/activation boundaries. Every interruption leaves a resumable or
clearly recoverable state; older binaries must not silently write new formats.

Do not silently remove plaintext backups or claim secure SSD erasure. Normal
canonical history has no new automatic expiry. New rolling backups default to
at most seven days, seven snapshots and 1 GiB, with configurable count/bytes and
visible shortened coverage. If one snapshot exceeds the cap, report missing
fresh coverage and retain the last usable copy until replacement is verified.

Restore is reviewed. Reconcile forgetting exclusions, grants and jobs before
use. Unknown older backups restore with recall, learning and automation inactive
until reviewed. External copies cannot be erased remotely. Export of plaintext
requires deliberate user intent and a clear disclosed destination.

## Rationale and implementation

[ADR 0004](../adr/0004-protect-managed-content-with-independent-recovery.md)
records the privacy/recovery tradeoff. Current JSONL history, artifacts and
private records are not yet application-encrypted. This proposal does not
retroactively change that fact or authorize migration of an existing home.
