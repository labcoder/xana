# Connections, models, and managed runtimes

> Audience: Contributors and coding agents
> Authority: Historical implemented scope
> Status: Implemented

## Result

Xana now distinguishes named connections, connection-owned model descriptors,
persisted model selection, native conversational providers, and foreign
managed-agent runtimes.

Implemented scope:

- native Ollama, custom OpenAI-compatible, OpenAI API, OpenRouter, and
  Anthropic API-key connections;
- tagged environment or operating-system credential references with no
  plaintext fallback;
- explicit bounded catalog refresh and non-secret caches;
- first-run Ollama, custom OpenAI-compatible, or managed Codex connection
  initialization;
- unified `xana model`, advanced `xana connection`, and `/model` selection;
- a Codex app-server managed runtime using Codex-owned ChatGPT login, account,
  refresh, model, turn, tool, sandbox, and approval behavior;
- canonical Xana identity handoff at Codex thread creation without replacing
  Codex base instructions or adding an outer model call, plus versioned local
  detection of legacy handles whose creation-time identity cannot be replaced;
- typed Codex activity for assistant text, reasoning summaries and exposed
  reasoning blocks, plans, commands/tools, file changes and diffs, compaction,
  collaboration, reroutes, approvals, warnings, and completion;
- advertised reasoning-effort/default metadata, persisted effort and summary
  selection, and same-thread model-option changes;
- quiet, normal, and verbose append-only terminal views plus bounded
  last-turn detail replay;
- atomically persisted opaque Codex thread handles keyed by connection and
  canonical workspace, with delegated resume and explicit clear;
- explicit native/managed conversation boundaries; and
- artifact-backed OpenAI-compatible, OpenRouter, Anthropic, and Codex image
  input with capability and aggregate limits.

Not included:

- direct ChatGPT/Codex OAuth or direct ChatGPT backend transport;
- Claude subscription OAuth;
- automatic native/background catalog refresh or automatic provider routing
  (managed Codex still performs its required live compatibility negotiation);
- history translation between native and managed runtimes;
- hidden chain-of-thought or an extra model call to explain Codex activity;
- A2A, hosted OAuth callbacks, or a remote managed-runtime daemon; or
- URL image fetching, OCR, image generation, Files API upload, or an
  interactive collapse/expand TUI.

The resulting implementation is described by
[Connections, models, and managed runtimes](../architecture/models-and-managed-runtimes.md)
and [ADR 0001](../adr/0001-delegate-codex-subscription-access.md).
