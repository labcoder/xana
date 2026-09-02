# M4 rich-content, preview, and disclosure evidence

> Scope: implementation evidence for course ticket M4-06
>
> Status: Complete locally; cross-platform CI remains the repository gate

M4-06 turns the M4 semantic vocabulary into bounded runtime projections and
reuses Xana's existing reviewed network and immutable-artifact authorities for
preview data. It does not make a frontend renderer, URL, or MIME declaration an
authority source.

## Implemented contracts

- Runtime assistant and tool-result messages gain deterministic inert semantic
  projections. Whole code/diff, table, display-math, and standalone safe-link
  blocks are recognized; ambiguous input remains Markdown or text.
- ANSI/control and bidirectional override characters are stripped. Tool-call
  arguments never enter projected content.
- Semantic content is bounded independently by part count, encoded retained
  bytes, and authoritative-final bytes. Conversation clear removes the derived
  projection as well as legacy visible history.
- Every content part projects to rich, text, metadata, or unsupported output
  with a bounded readable fallback. Link preview/open and artifact
  inspect/copy/save/reveal/open are distinct typed intents.
- `web_fetch` produces a typed generic link card after the existing exact
  outbound review, public-address resolution, redirect, scheme, header, MIME,
  encoding, response-byte, extraction-time, and cancellation checks. The card
  retains sanitized text, bounded site/title facts, provenance, digest,
  freshness, and `untrusted: true`; it contains no live remote document.
- Artifact preview transport is range-based. It accepts only a visible opaque
  artifact ID and offset, retains at most 64 KiB, streams and hashes the complete
  artifact, and rejects symlinks, non-regular files, replacement, length, and
  digest mismatches.
- Resource inspection applies aggregate and compiled source ceilings before
  artifact I/O, then retains a bounded signature probe while verifying the
  complete source. Declared and detected media types remain separate.
- Common raster, SVG, Lottie, audio, and video signatures produce typed
  metadata. SVG/Lottie stay pending for reviewed derivative adapters and
  unknown binaries fail closed.
- Acquisition, presentation, playback, external open, provider input, focused
  analysis, and transform project independently. Exact provider/model/route,
  effective source-byte limit, reason, source, and freshness facts are retained;
  a missing fact is never inferred.
- Existing provider/compaction summaries are attributed. Constructing a fresh
  recap records explicit connection/model intent and does not itself perform a
  model request.

## Automated evidence

Focused fixtures cover:

- rich-block recognition, hostile markup, terminal/bidi controls, unknown
  content, surface fallbacks, and tool-argument redaction;
- link-card URL/trust/digest bounds plus local-only HTML active-content,
  redirect, private-address, header, compression, MIME, encoding, body-size,
  timeout, cancellation, and zero-send policy failures;
- verified artifact ranges, corruption, symlinks on Unix, visible-reference
  authorization, range offsets, total length, and bounded retention;
- WebP animation/static signatures, declared/detected disagreement, SVG/Lottie
  pending state, metadata bombs, and zero-I/O aggregate rejection; and
- independent local presentation, permission-gated external open, exact
  provider input, and unsupported route facts.

The required repository gates are:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

## Commits

- `7c7b124 feat(frontend): project bounded rich content`
- `2e4e669 feat(resource): inspect verified artifact ranges`
- `8cfe2c3 feat(web): return safe link preview cards`

## Deferred ownership

M4-11 owns terminal cards and explicit interaction, while M4-19 and M4-21 own
Desktop renderers and reviewed media adapters. M4-23 owns cross-platform codec
and performance evidence. This ticket does not claim remote embeds, automatic
URL/media loading, native Lottie, arbitrary codecs, provider upload support,
speech, browser automation, or a public protocol/SDK.
