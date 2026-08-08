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
- unified `xana model`, advanced `xana connection`, and `/model` selection;
- a Codex app-server managed runtime using Codex-owned ChatGPT login, account,
  refresh, model, turn, tool, sandbox, and approval behavior;
- explicit native/managed conversation boundaries; and
- artifact-backed OpenAI-compatible, OpenRouter, Anthropic, and Codex image
  input with capability and aggregate limits.

Not included:

- direct ChatGPT/Codex OAuth or direct ChatGPT backend transport;
- Claude subscription OAuth;
- automatic startup catalog traffic or automatic provider routing;
- history translation between native and managed runtimes;
- A2A, hosted OAuth callbacks, or a remote managed-runtime daemon; or
- URL image fetching, OCR, image generation, or Files API upload.

The resulting implementation is described by
[Connections, models, and managed runtimes](../architecture/models-and-managed-runtimes.md)
and [ADR 0001](../adr/0001-delegate-codex-subscription-access.md).
