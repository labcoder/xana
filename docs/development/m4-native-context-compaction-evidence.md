# M4 native context and compaction evidence

This document records the implementation evidence for the model-aware prompt
budget and durable native compaction boundary introduced in M4-03A. It is a
development baseline, not a provider tokenization or cache claim.

## Shipped boundary

- A native turn derives one immutable prompt budget from the selected catalog
  descriptor, an optional narrowing route ceiling, output/reasoning/tool
  reserves, and the configured compaction policy.
- Missing model metadata uses a conservative 32,768-token context fallback.
  Metadata below Xana's 2,048-token minimum fails before provider I/O.
- Every prompt-plan ledger is categorical and redacted. It exposes estimated
  sizes, reserves, omissions, and unavailable cache observations without
  copying hidden instructions, message bodies, tool results, or attachment
  bytes.
- Manual `/compact` and automatic threshold compaction append a versioned
  checkpoint before a provider request. The checkpoint retains exact source
  entry ids, a BLAKE3 source digest, an operation id, predecessor, reason,
  recent-tail boundary, structured summary, and producing model/budget facts.
- The raw journal remains canonical and unchanged. Repeated compaction extends
  one validated chain; branch/head changes select only a checkpoint on the
  active path. Managed runtimes retain ownership of their own context.

## Budget fixtures

The default policy reserves 4,096 output tokens, 4,096 tool tokens, and no
reasoning reserve for the non-reasoning fixture. Its 80% threshold is applied
to the effective input window before subtracting the tool reserve.

| Catalog fact | Effective input | Compact threshold | Verbatim tail | Source |
|---|---:|---:|---:|---|
| 16,384 known tokens | 12,288 | 5,734 | 2,867 | model catalog |
| 128,000 known tokens | 123,904 | 95,027 | 8,192 | model catalog |
| missing metadata | 28,672 | 18,841 | 8,192 | 32,768 fallback |
| 128,000 with a 24,000 route ceiling | 19,904 | 11,827 | 5,913 | narrowed route |

These values are conservative Xana estimates. Provider-reported usage remains
separate, and cache reads/writes remain `unavailable` when the provider does not
advertise them.

## Resource baseline

The deterministic M4 fixtures produced the following Windows development-build
measurements on 2026-09-01:

| Fixture | Measurement |
|---|---:|
| Rendered system prompt | 4,851 bytes / 1,616 estimated tokens |
| Built-in tool schemas | 3,003 bytes / 1,002 estimated tokens |
| Synthetic raw message bodies | 118,820 bytes |
| Derived durable checkpoint | 17,054 bytes |
| Structured summary within checkpoint | 16,214 bytes |
| Source entries retired by checkpoint | 118 |

The long-history fixture asserts that the checkpoint remains below 32 KiB and
less than one third of the raw transcript fixture. The raw messages remain in
the append-only session journal; the checkpoint does not rewrite or duplicate
the complete transcript.

## Adversarial and recovery coverage

Automated coverage verifies known, unknown, narrowed, and contradictory model
limits; redacted ledger bounds; manual and automatic compaction; provider
admission after compaction; exact restart/replay; repeated compaction;
operation-id reuse; branch/head movement; corrupt digests; malformed checkpoint
plans; active-turn rejection; and managed/transient unavailability. Generic
journal-tail recovery continues to cover an interrupted append before a
checkpoint becomes authoritative.

The automatic-runtime fixture also proves that retired marker text is absent
from the provider request while still present in the reduced raw journal.
Session inspection reports only bounded checkpoint provenance and never prints
the summary or source messages.

## Verification

The implementation commit is `b92d9ed`.

```text
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features --no-fail-fast -- --test-threads=4
```

Results: formatting and strict Clippy passed; 852 library tests passed with six
explicitly ignored stress/manual probes, 24 CLI/settings integration tests
passed, and three Desktop projection tests passed.

## Deliberate limits

The summary is a bounded deterministic continuation aid, not semantic memory.
M4-03A does not add retrieval, embeddings, document ingestion, RLM, provider-
wide cache optimization, or full-history frontend paging. Those remain owned by
later milestones, with this byte and retention baseline available for
comparison.
