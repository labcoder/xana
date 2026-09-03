# M4 Desktop Conversation evidence

> Ticket: M4-17A
>
> Status: Implementation complete; owner verification pending

## Delivered contract

Xana Desktop now drives native and managed Codex Conversations through the
same bounded `xana::desktop` facade. Conversation, Message, and Activity are
independent Workbench panels over one runtime-owned Conversation; none owns an
agent loop, provider, tool, credential, or durable transcript.

- The retained virtual transcript consumes stable message and Operation IDs,
  streams replacement deltas, converges on authoritative finals, and leaves a
  failed response retryable only while its exact bounded request remains.
- The Message panel supports multiline IME input, multiple runtime-validated
  image attachments, picker, drag/drop, clipboard image, a bounded per-
  Conversation queue, cancellation, and distinct edit/regenerate recovery.
- A bounded retained `Chat`/`PromptBar` pair per recent Conversation preserves
  component-owned cursor/selection, focus, transcript scroll, and follow-tail
  state. Xana's separate bounded composer store isolates draft text,
  attachments, and queued turns by stable Conversation identity.
- Managed Codex model and reasoning changes are acknowledged by the managed
  actor before Xana publishes a later-turn receipt. The vendor thread and its
  context remain attached. Native model/Profile changes use Settings and an
  explicit fresh Conversation instead of translating history.
- Activity projects owner-qualified reasoning summaries, tool and integration
  work, approvals, execution facts, usage freshness, artifacts, completion
  receipts, and failures. Runtime-issued permission identity is required for
  allow-once, allow-session-scope, or deny.
- Neither current execution owner advertises same-turn steering. The composer
  therefore exposes an honest queued follow-up rather than mislabeling it as a
  steer.

## Automated evidence

Run from the Xana repository root:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --quiet
```

The focused Desktop suite covers stable managed-selection receipts, terminal
failure recovery identity, catalog metadata, composer isolation and bounds,
attachments, layouts, settings transitions, and runtime projection. The full
workspace suite covers the native and managed execution adapters, controller
leases, approvals, activity, streaming convergence, and protocol compatibility.

## Owner verification retained for M4-24

On each supported desktop platform, verify:

1. Multiline typing, IME composition, paste, picker, multi-image drag/drop, and
   clipboard image staging in Message.
2. Rapidly switch two Conversations after moving each transcript away from its
   tail and placing a non-terminal cursor/selection in each draft; verify every
   view returns to its own state.
3. Queue, reorder, edit, and remove follow-ups during a Run, then interrupt and
   verify exactly one intended follow-up submits.
4. Exercise allow once, allow exact session scope, and deny from Activity with
   keyboard and pointer; hide or move Activity and verify attention remains
   reachable.
5. Complete and fail one native and one managed Run; inspect owner, location,
   authority, usage freshness, artifacts/checks, receipt, retry, and recovery.
6. Change managed model and reasoning between turns and confirm the same Codex
   thread remains; change a native model/Profile and confirm Xana asks for and
   opens a fresh Conversation without rewriting the source.
7. Repeat at 200% text scale, high contrast, reduced motion, and with a screen
   reader. No action may depend only on color, pointer, or animation.

No live-provider credential, prompt text, output content, artifact bytes, or
filesystem path belongs in committed evidence.
