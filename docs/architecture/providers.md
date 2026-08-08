# Provider contracts

Audience: contributors and coding agents. Authority: descriptive.

The runtime normalizes conversational providers behind a focused request:
borrowed model id, ordered provider-neutral messages, tool schemas, an output
token bound, and a streaming delta sink. Provider-specific request, response,
and event shapes stay private in each adapter.

OpenRouter uses the OpenAI-compatible transport at its `/api/v1` base URL. Its
bearer credential is supplied by the composition edge, never read during
configuration parsing. Optional `HTTP-Referer` and `X-Title` attribution
headers are explicit inputs. Model descriptors record text/image modalities,
tool support, reasoning support, and optional context limits; remote model
catalogues are not required during startup.

The Anthropic Messages adapter maps internal system content to the top-level
`system` field, preserves ordered user/assistant blocks, converts tool use and
tool results to their structured block forms, and sends the required
`anthropic-version` header. Its SSE decoder accumulates text deltas and split
tool JSON without exposing wire events to the runtime. Unsupported internal
blocks fail before HTTP I/O.

Optional subscription credentials use the runtime-only `CredentialId`,
redacted secret, secure-store, device-code, status, logout, and refresh
coordination contracts. The in-memory store exists for deterministic tests;
production storage must be an OS credential store and must never be redirected
into `config.toml`, sessions, artifacts, or `XANA_HOME` data. `xana auth
login|status|logout codex-oauth` is intentionally unavailable until an official
supported protocol authority supplies the endpoint, client identity, scopes,
audience, refresh, and revocation rules.
