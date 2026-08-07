# Project context and system prompt

> Audience: People using Xana in a project or inspecting what enters a model request.

Xana builds one provider-neutral system prompt when an agent starts. The
assembly contract is versioned as `xana-prompt-v1`, and the resulting bytes are
frozen for that agent. Every provider request—including later tool rounds—gets
the same system message followed by the current conversation history. Changes
on disk take effect only when a new agent starts.

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

The normal developer preview has no bundled product-documentation capability,
so layer 3 is omitted. Xana does not claim it can read its installed
documentation. A bounded `xana.docs.read` capability and its `xana_docs` tool
are not implemented yet.

Exact machine-readable tool schemas remain a separate provider request field.
The human-readable catalog and schemas both come from the same immutable tool
registry snapshot, so the prompt cannot advertise an unavailable tool.

Each rendered layer has a labeled boundary containing a transient source id,
display name, kind, origin, trust class, optional relative path, and truncation
flag. Dynamic attributes and bodies are XML-escaped so project text cannot
forge another source boundary. The labels make provenance legible; they are
not a security boundary.

## Root `AGENTS.md`

Xana checks only this path relative to the directory where it starts:

```text
AGENTS.md
```

Absence is normal. When present, the path must be a regular, non-symlink UTF-8
file no larger than 64 KiB. Xana reads at most 64 KiB plus one detection byte,
then automatically selects only a head preview capped at 1,024 estimated
tokens. Oversized, invalid UTF-8, symlink, and non-regular sources produce a
typed startup error rather than silently entering the prompt.

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

Startup prints a context-plan report with selected and omitted source ids,
estimated project tokens, and truncation flags. It never prints source content.

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

## Transient limitations

Source and preview ids in `xana-prompt-v1` last only for the active agent.
There is no artifact store, durable context reference, context service,
context compaction, or native context-plan executor. A later durable layer can
reuse the selection concepts without pretending these transient values already
provide persistence or recovery.
