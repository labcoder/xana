![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust. It can
chat, inspect and edit a workspace, run commands with explicit permission,
read text and CSV documents, answer questions about its own bundled
documentation, and send local image attachments to capable models.

Xana currently supports:

- local Ollama and custom OpenAI-compatible servers;
- the OpenAI API and OpenRouter with API keys;
- Anthropic Messages with an API key; and
- ChatGPT Plus/Pro through a locally installed Codex app-server, with Codex
  owning login, token refresh, inference, tools, sandbox, and inner history.

Native connections run Xana's own agent loop. Codex is a managed runtime: Xana
provides the CLI and process/event/approval bridge but does not wrap the turn
in a second model call or copy Codex credentials. When Xana creates a managed
thread, it supplies its canonical built-in identity as a developer instruction,
so the assistant presents itself as Xana while Codex retains ownership of its
base instructions and inner loop.

## Install from source

Xana uses the pinned Rust toolchain:

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo install --path crates/xana-cli --locked
xana --version
xana init
xana config check
xana
```

Git installation is also supported:

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
```

Use `--rev COMMIT_SHA` for a repeatable build. Xana is not published to
crates.io and has no prebuilt installer or automatic updater. See
[Source installation](docs/user/installation.md).

## Choose a first connection

`xana init` offers local Ollama, a ChatGPT subscription through the managed
Codex runtime, or a custom OpenAI-compatible endpoint. It creates and validates
the selected connection without storing a plaintext secret. A minimal Ollama
document is:

```toml
version = 3
default_profile = "default"
default_child_route = "default"
permission_mode = "ask"

[shell]
kind = "platform"

[providers.ollama]
kind = "ollama"

[providers.ollama.models."qwen3:1.7b"]
input_modalities = ["text"]
tools = true

[profiles.default]
connection = "ollama"
model = "qwen3:1.7b"
max_tool_rounds = 8

[routes.default]
profile = "default"
```

## Add a remote API provider

Keys can be stored in the OS credential manager or referenced through one
named environment variable. Plaintext keys never belong in `config.toml`.

```bash
xana connection add openrouter --kind open-router --model openai/gpt-4.1
xana connection set-key openrouter
xana connection refresh openrouter
xana model use openrouter/openai/gpt-4.1
xana
```

Use `--kind open-ai` for the OpenAI API or `--kind anthropic` for Anthropic.
Anthropic is API-key-only; Xana does not offer Claude subscription OAuth.

## Use a ChatGPT subscription through Codex

On a fresh installation, install a compatible Codex CLI and choose
`ChatGPT subscription through Codex` in `xana init`. Xana creates the managed
connection and prints the status, login, catalog-refresh, and model-discovery
commands to run next.

To add Codex to an existing Xana configuration instead:

```bash
xana connection add codex --kind codex --model ADVERTISED_MODEL_ID
xana connection status codex
xana connection login codex
xana connection refresh codex
xana model
xana model use codex/ADVERTISED_MODEL_ID --effort high --summary auto
xana
```

The exact model names come from `codex app-server` and can change with account
access; replace `ADVERTISED_MODEL_ID` with one shown by `xana model list
--connection codex`. No static model example is authoritative. Login also
supports `--device-code`. Xana delegates the local OAuth completion to Codex;
it needs no hosted callback server and never reads Codex's auth file.

Xana launches the configured Codex CLI, not the Codex desktop process. The
desktop app and CLI binaries update separately even when they share account
state. Use `codex --version` and `xana connection status codex` to confirm the
runtime Xana is actually supervising; update or rebuild both sides when the
experimental app-server protocol changes.

The managed assistant identifies itself as Xana. Xana sends its canonical
built-in identity when it creates the Codex thread, but does not replace
Codex's base instructions, tools, sandbox, approvals, or project context
discovery. This is part of the same managed request, not an additional model
call. Codex fixes the effective identity when it creates a thread; it cannot
retrofit Xana's identity onto an older thread during resume. Xana detects
legacy local handles and tells you to enter `/clear` before the first prompt.
That starts a new Xana-identified thread without deleting the old Codex-owned
thread.

During a managed turn Xana projects the activity that Codex app-server emits:
reasoning summaries, plans, command and tool progress, file changes, context
compaction, Codex-owned subagent activity, model reroutes, and approval
requests. The default `normal` view is concise. Use
`/activity quiet|normal|verbose` to change the presentation for
the current CLI process and `/details` to replay the bounded details retained
for the last turn. `verbose` can show raw reasoning text only when Codex
actually emits it; Xana cannot expose private hidden chain-of-thought.

## Models and connections

The normal model UX is intentionally shallow:

```text
xana model
xana model list --connection CONNECTION
xana model refresh CONNECTION
xana model use CONNECTION/MODEL
xana model use codex/MODEL --effort auto|EFFORT --summary auto|concise|detailed|off
```

Inside chat, `/model` lists models and `/model CONNECTION/MODEL` selects one.
Switching between Xana's native loop and a managed runtime starts a new
conversation rather than silently translating history. Within managed Codex
chat, `/model codex/MODEL`, `/reasoning EFFORT`, and `/reasoning-summary MODE`
apply to subsequent turns without starting a new Codex thread or discarding
its context. `/reasoning auto` restores the selected model's advertised
default.

Use `xana connection list|add|status|set-key|delete-key|login|logout|refresh|remove`
for advanced connection and credential control. See
[Configuration](docs/user/configuration.md) for exact commands, provider kinds,
catalogs, OS credential storage, and `XANA_HOME`.

Named child task routes are separate from the interactive model selection.
Inspect their exact local resolution without starting a provider or managed
process:

```text
xana route list
xana route check default
```

The current route commands are read-only diagnostics; child execution is not
yet exposed by this release.

## Start first-run setup again

`xana reset` (alias: `xana clean`) previews the narrow setup state it will
remove and asks for confirmation. Use `--yes` for an explicit noninteractive
reset:

```bash
xana reset --yes
xana init
```

From this source checkout, use `cargo run -- reset --yes` followed by `cargo
run -- init`. Reset removes configuration, model selection/catalog caches, and
managed-thread handles. It preserves native sessions, artifacts, stored API
keys, Codex authentication, and Codex-owned conversations. `/clear` is
different: it clears only the current conversation.

## Chat, tools, and images

Bare `xana` starts terminal chat. Native conversations create a durable JSONL
session; resume one explicitly with `xana --resume SESSION_ID`. `/clear` moves
to a new empty native history or a new Codex thread. `/quit`, Ctrl-C, and EOF
shut down the foreground runtime.

For Codex, Xana stores only an opaque thread id keyed by the connection and
canonical workspace. The next Xana process resumes that Codex-owned thread on
its first turn. It does not copy the conversation, tool state, or credentials;
`/clear` replaces the saved handle with a new Codex thread.

The capability-resolved native tool snapshot contains:

- `read_file`: bounded UTF-8 file/range reads;
- `list_files`: bounded sorted non-recursive listings;
- `edit_file`: one exact replacement in an existing bounded UTF-8 file;
- `run_command`: configured-shell execution with independently bounded stdout
  and stderr;
- `read_document`: bounded UTF-8 or CSV-to-Markdown extraction; and
- `xana_docs`: bounded reads from Xana's curated, version-matched docs.

Every effect crosses Xana's permission broker. Permission is not containment:
allowed native tools use the process's ordinary host access. Codex-managed
turns use Codex's own tools/sandbox and Xana projects command/file approval
requests into the terminal.

Use `/attach WORKSPACE_RELATIVE_IMAGE` to stage PNG, JPEG, or GIF input. Xana
keeps immutable artifact references, enforces file/pixel/count/aggregate
budgets, preserves attachment order, and fails closed unless the selected
model advertises image input. OpenAI-compatible and Anthropic bytes are
resolved only at the provider wire edge; Codex receives checked workspace
paths.

## Prompt, context, and recovery

Each native root turn freezes one versioned system-prompt snapshot containing
Xana's built-in identity/guidelines, the actual tool catalog, runtime context,
a concise reference to `xana_docs`, and a bounded durable root `AGENTS.md` view
when present. Xana does not discover `XANA.md`, nested `AGENTS.md`, skills, or
plugins yet.

Native tool intents and results are durably bracketed. Session resume performs
no automatic recovery; use `xana operation plan` and explicit `xana operation
resume` for eligible safe reads. See [Project context](docs/user/project-context.md),
[Permissions](docs/user/permissions.md), [Sessions](docs/user/sessions.md), and
[Operation recovery](docs/user/operations.md).

## Documentation and development

- [Documentation index](docs/README.md)
- [Architecture](docs/architecture/README.md)
- [Connections, models, and managed runtimes](docs/architecture/models-and-managed-runtimes.md)
- [Design principles](docs/principles.md)

Required checks:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## License

MIT - see [LICENSE](./LICENSE).
