# Phase 3 implemented subset

> Audience: Contributors and coding agents  
> Authority: Historical implemented scope  
> Status: Implemented

This record names the Phase 3 contracts that are present in the 0.2.0
workspace without silently accepting the broader Proposed designs 0001, 0002,
0006, 0007, or 0008.

Implemented:

- the `xana-core` → `xana-runtime` → `xana-cli` workspace dependency direction;
- deterministic capability ids, dependency resolution, conflict diagnostics,
  and immutable core snapshots;
- an explicit bounded self-documentation catalog and bytes-based text/CSV
  document extraction contract;
- focused conversational-provider descriptors, OpenRouter's
  OpenAI-compatible bearer/attribution seam, and a private Anthropic Messages
  converter/stream accumulator;
- redacted credential, device-code polling, token-store, status/logout, and
  per-credential refresh-coordination primitives with deterministic fakes;
- artifact-backed image references, `/attach` staging, one-shot turn
  consumption, `/clear` cleanup, format/pixel/byte limits, model capability
  checks, and a bounded wire-edge data-URL resolver.

Not implemented by this record:

- a vendor-specific `codex-oauth` protocol. The CLI reports it unavailable
  until an official authority supplies endpoints, client identity, scopes,
  audience, refresh, and revocation semantics;
- enabling the default foreground provider composition to inject image bytes,
  automatic model routing, Anthropic image blocks, URL fetching, OCR, image
  generation, or terminal graphics;
- the full plugin installer, language kernel, native context-plan system, or
  any mid-turn dependency installation.

These non-goals are deliberate safety and authority boundaries, not hidden
fallback behavior.
