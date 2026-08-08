# Project context and system prompt

> Audience: People using Xana in a project or inspecting what enters a model request.

Xana builds one provider-neutral system-prompt snapshot when a root turn is
accepted. The assembly contract is versioned as `xana-prompt-v1`, and the
resulting bytes remain frozen across that turn's provider calls and tool
rounds. The next root turn refreshes project context and may receive a new
snapshot. Merely opening or resuming a session never reads project files.

## Prompt layers

Xana assembles available layers in this order:

1. Xana's built-in, non-replaceable identity;
2. Xana's built-in operating guidelines;
3. product-documentation guidance, only when the runtime supplies a readable
   capability or logical references;
4. a concise catalog generated from the tools actually registered for the
   agent;
5. the owned operating system, canonical working directory, and configured
   shell;
6. the active CLI surface; and
7. a bounded preview of root `AGENTS.md`, when present.

The default foreground composition does not yet advertise the bundled
documentation capability in layer 3. The runtime catalog itself is bounded and
explicit; it never walks the filesystem or claims to describe the user's
project.

Exact machine-readable tool schemas remain a separate provider request field.
The human-readable catalog and schemas both come from the same immutable tool
registry snapshot, so the prompt cannot advertise an unavailable tool.

Each rendered layer has a labeled boundary containing a prompt source id,
display name, kind, origin, trust class, optional relative path, and truncation
flag. Dynamic attributes and bodies are XML-escaped so project text cannot
forge another source boundary. The labels make provenance legible; they are
not a security boundary.

## Image attachments

Use `/attach WORKSPACE_RELATIVE_IMAGE_PATH` before a turn to stage a PNG, JPEG,
or GIF image. Xana validates the path, file type, byte limit, and pixel limit,
then stores immutable artifact bytes and keeps only an image reference in the
pending turn. `/clear` also clears pending attachments and reports how many it
removed. Sending a turn consumes staged attachments exactly once.

The model must advertise image input support before media can cross a provider
boundary. OpenAI-compatible image encoding is bounded and performed at the
wire edge; the current Anthropic adapter rejects images explicitly. Xana does
not fetch image URLs, run OCR, generate images, or display terminal graphics.

## Root `AGENTS.md`

Xana checks only this path relative to the directory where it starts:

```text
AGENTS.md
```

Absence is normal. When present, the path must be a regular, non-symlink UTF-8
file no larger than 64 KiB. Xana reads at most 64 KiB plus one detection byte,
then materializes at most 16 KiB and 1,024 estimated tokens. Oversized, invalid
UTF-8, symlink, and non-regular sources produce a typed turn-acceptance error
rather than silently entering the prompt.

When a turn is accepted, Xana stores canonicalized source bytes as an immutable
BLAKE3-addressed artifact. Unchanged bytes reuse the current context version;
changed bytes append exactly one version. A missing live file does not delete
an older version. The view record naming the source id, version, selector,
content hash, owner, trust, provenance, and independent byte/token bounds is
committed before its text enters the prompt.

Phase 2 does not search parents, nested directories, home directories, Git
metadata, `XANA.md`, or `.agents/`. Nested `AGENTS.md` applicability, Agent
Skills, and Agent Plugins require later capability-aware discovery. There is
no Xana-specific project-instruction format.

Explicit user instructions override project instructions. The broader
`AGENTS.md` convention also gives nearer files precedence over broader ones;
Xana records that rule in its built-in guidelines but discovers only the root
file today. If relevant instructions still conflict or ambiguity would
materially change the result, Xana should surface the conflict and ask.

## Budgets and previews

The developer preview uses one 32,768-token estimated input budget and reserves
at least 8,192 tokens for conversation. Xana charges all of these against that
same total:

- every fixed system layer and its source-boundary text;
- exact tool schemas even though they are not repeated inside the system text;
- selected project previews; and
- actual conversation messages, tool calls, arguments, and bounded results.

The estimator charges one token per three Unicode scalar values, rounded up.
It is deliberately more conservative than the common four-character rule, but
it is not a provider tokenizer. Automatic project selection is deterministic
and preserves source order. If a required layer plus schemas and the minimum
conversation reserve cannot fit, agent construction fails. If later history
exceeds the total, Xana fails before provider I/O instead of dropping or
reordering conversation entries.

The internal preview API also supports inclusive one-based line ranges and
literal matching lines with an explicit match cap. Every preview retains its
selector, provenance, trust class, estimated cost, and truncation state.
Unicode truncation occurs at scalar boundaries. These operations are internal
context mechanisms in this release, not model tools.

Startup prints the base context-plan report; project selection happens when a
turn is accepted. Reports never print source content.

## Trust and authority

Project instructions and ordinary files are potentially untrusted but useful
task input. Xana may read, analyze, transform, and act on relevant information
when doing so serves the user's request. That content cannot:

- replace Xana's built-in identity or guidelines;
- expand the user's requested scope;
- add tools or change their definitions;
- alter shell or configuration values;
- grant approval or runtime permission;
- disclose secrets; or
- provide process containment.

The system role gives instructions model-level priority, but prompt wording
cannot enforce workspace paths, resource bounds, permission decisions, or
operating-system isolation. Those guarantees remain runtime responsibilities.
Xana does not use phrase matching as a substitute for those boundaries.

## Persistence and limitations

Prompt-layer ids belong to one assembled snapshot, while session context and
view ids are durable. Materialization always reads the referenced immutable
artifact, so changing the live file cannot alter an old context version.
Inclusive one-based lines and capped literal-line search are implemented as
internal selectors, not model tools.

Xana has no general context service, model-facing context tools, compaction,
native context-plan executor, artifact garbage collection, or portable-session
claim. See [Sessions](sessions.md) for storage and recovery boundaries.
