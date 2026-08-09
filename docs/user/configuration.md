# Xana configuration

> Audience: People installing, configuring, or using Xana.

Xana loads one strict, versioned `config.toml`, with a 1 MiB file limit.
Human-authored configuration declares connections and profiles; static
secrets, cached catalogs, model selection, sessions, and artifacts live
elsewhere.

## Quick Setup

```bash
xana setup
xana config path
xana config check
```

Quick Setup offers every supported connection kind without recommending or
preselecting one. It validates endpoint or executable first, then credential
or Codex-owned account, then fetches the live catalog before model and
reasoning selection. The platform shell and bounded native defaults are used;
full profile, route, and shell editing remains an advanced configuration task.

`xana setup` is safe to rerun. It stages ordinary configuration in memory,
shows a bounded redacted review, and replaces `config.toml` atomically only
after confirmation. An exact prior config is retained as `config.toml.bak`.
Cancellation and connection failures write nothing. If a newly staged OS-store
key cannot be followed by a config commit, the prior key is restored. Codex
OAuth remains vendor-owned and is never represented as part of that rollback.
`--dry-run` establishes and validates without committing.

```bash
xana setup --non-interactive \
  --kind ollama \
  --connection ollama \
  --base-url http://localhost:11434/v1 \
  --model qwen3:1.7b \
  --permission-mode ask \
  --yes
```

API keys are either referenced by name or read from the explicit stdin secret
channel; they never appear in argv or TOML:

```bash
xana setup --non-interactive --kind open-router --connection openrouter \
  --credential-env OPENROUTER_API_KEY --model PROVIDER/MODEL \
  --permission-mode ask --yes
printf '%s' "$OPENAI_API_KEY" | xana setup --non-interactive --kind open-ai \
  --connection openai --key-from-stdin --model MODEL \
  --permission-mode ask --yes
```

Bare interactive `xana` offers Quick Setup when configuration is missing or
invalid. Non-TTY startup fails promptly with the exact noninteractive command
shape. Inside plain or full-screen chat, `/setup` closes the current foreground
owner, restores the terminal, runs the same setup transaction, and starts a
new conversation using the installed choice.

For a managed Codex first connection, no HTTP base URL or Xana credential is
accepted:

```bash
xana setup --non-interactive \
  --kind codex \
  --connection codex \
  --model ADVERTISED_MODEL_ID \
  --reasoning-effort high \
  --permission-mode ask \
  --yes
```

`--codex-program PROGRAM` overrides the executable; `--codex-home PATH`
selects an absolute isolated `CODEX_HOME`. Otherwise Xana launches `codex` and
shares the normal Codex home. Setup requires the Codex-owned account to be
available and verifies the exact model and optional reasoning effort against
the live app-server catalog. Login remains an explicit vendor-owned action;
run `codex login` or `xana connection login codex` before retrying setup.

## Reset first-run setup

Use `reset` (or its `clean` alias) when you want to discard the current setup
and run Quick Setup again:

```bash
xana reset
# Noninteractive confirmation:
xana reset --yes
xana setup
```

From a source checkout, the equivalents are `cargo run -- reset` and `cargo
run -- setup`. Without `--yes`, an interactive terminal previews every target
and asks for confirmation; redirected input fails closed.

Reset removes only:

- `config.toml`;
- `data/selection.toml`;
- cached model catalogs beneath `cache/models/`; and
- Xana's managed-thread handles beneath `data/managed-threads/`.

It preserves native session journals, artifacts, OS credential-manager API
keys, Codex authentication, and Codex-owned conversations. Removing a managed
thread handle means Xana will not automatically resume that external thread,
but it does not delete the thread from Codex. Use `/clear` to clear only the
active conversation, `connection delete-key` to remove an API key, and
`connection logout` for an explicit managed-account logout.

## Connections and models

`xana model` is the normal discovery and selection entry point:

```text
xana model
xana model list --connection openrouter
xana model refresh openrouter
xana model use openrouter/openai/gpt-4.1
xana model use codex/ADVERTISED_MODEL_ID --effort high --summary auto
```

Catalog refresh is explicit and caches only bounded non-secret metadata.
Native startup never refreshes a catalog. Starting managed Codex chat performs
a bounded live `model/list` compatibility check before the first prompt.
`/model` lists models during chat;
`/model CONNECTION/MODEL` persists a selection and starts a new conversation
when runtime ownership changes.

Advanced connection commands are:

```text
xana connection list
xana connection add ID --kind KIND --model MODEL [options]
xana connection status ID
xana connection set-key ID
xana connection delete-key ID
xana connection login ID [--device-code]
xana connection logout ID --yes
xana connection refresh ID
xana connection remove ID --yes
```

`remove` rejects the selected connection and any connection referenced by a
profile. Select another model and remove profile references first.

### Child task routes

Task routes are exact names for reusable child profiles. They are independent
of the model selected for the interactive foreground conversation. Inspect
them without starting a provider, opening a network connection, or launching
Codex:

```text
xana route list
xana route check worker
```

`route list` is bounded and marks the configured default with `*`. It shows
locally unavailable routes inline. `route check` exits nonzero for an unknown
route, missing credential, absent configured/cached model, unsupported model
option, unknown capability, or a model that explicitly rejects the selected
tools. Diagnostics identify the route, profile, connection, and model without
printing secret values. The route commands themselves are read-only. A Codex
route requires a model already present in the local catalog cache and an empty
native capability set because Codex owns its own tools:

```toml
[profiles.codex-review]
connection = "codex"
model = "ADVERTISED_MODEL_ID"
capabilities = []
permission_mode = "ask"

[routes.codex-review]
profile = "codex-review"
```

Run `xana connection login codex` and `xana model refresh codex` before using
that route. Native chat uses the same exact resolver when an orchestration tool
admits one supervised native or managed child; see
[Child orchestration](orchestration.md).

### API-key providers

Add a connection, store its key in the operating-system credential store, and
refresh models:

```bash
xana connection add openai --kind open-ai --model gpt-4.1
xana connection set-key openai
xana connection refresh openai

xana connection add openrouter --kind open-router --model openai/gpt-4.1
xana connection set-key openrouter
xana connection refresh openrouter

xana connection add anthropic --kind anthropic --model claude-sonnet-4-5
xana connection set-key anthropic
xana connection refresh anthropic
```

Use `--from-stdin` for a bounded noninteractive key read. Avoid putting a key
directly in a command argument or shell history. To use a named environment
variable instead of the OS store, add `--env VARIABLE`; Xana resolves exactly
that source and never falls back to another credential.

OpenAI and OpenRouter use bearer authentication. Anthropic uses `x-api-key`
and supports API keys only; Xana does not offer Claude subscription OAuth.
OpenRouter is an API/credit connection even if its key was created through an
external OAuth flow.

The default endpoints are:

| Kind | Default | Catalog |
|---|---|---|
| `ollama` | `http://localhost:11434/v1` | `/api/tags` |
| `open-ai` | `https://api.openai.com/v1` | `/v1/models` |
| `open-router` | `https://openrouter.ai/api/v1` | `/api/v1/models/user` |
| `anthropic` | `https://api.anthropic.com` | `/v1/models` |
| `open-ai-compat` | requires `--base-url` | `/v1/models` |

Custom OpenAI-compatible and Ollama connections may also declare a stored or
environment bearer credential.

### ChatGPT subscription through Codex

Install the Codex CLI and log it in normally, or let Xana delegate login:

```bash
xana connection add codex --kind codex --model ADVERTISED_MODEL_ID
xana connection status codex
xana connection login codex
# Headless alternative:
xana connection login codex --device-code
xana connection refresh codex
xana model use codex/ADVERTISED_MODEL_ID
xana
```

Replace `ADVERTISED_MODEL_ID` with an exact ID advertised by the installed
Codex app-server; use `xana model list --connection codex` after refresh. No
model name in this document is an availability promise. Xana launches `codex
app-server --stdio`. Codex owns browser/device login, token storage and
refresh, inference, tools, sandbox, approvals, and inner thread history. Xana
never reads or copies Codex's auth file and needs no hosted OAuth callback
server.

When Xana creates a managed thread, it supplies its canonical built-in identity
as a developer instruction. The assistant should therefore identify itself as
Xana, while Codex continues to own its base instructions, tools,
project-context discovery, and inner loop. Xana does not add an outer model
call or duplicate its native prompt layers for this handoff.

Codex cannot retrofit a new identity onto an older persisted thread during
resume. Xana records the creating identity version in its non-secret local
thread handle and warns when that marker is missing or old. Enter `/clear`
before the first prompt to create a new Xana-identified thread. Clearing the
local handle does not delete the old Codex-owned conversation.

Xana launches the configured Codex CLI executable; it does not send managed
turns through the running Codex desktop application. The two programs may use
the same Codex home, but their binaries update separately. If desktop has a
model or protocol behavior that Xana does not, compare the exact executable
and version with `Get-Command codex` plus `codex --version` on PowerShell (or
`command -v codex` plus `codex --version` on macOS/Linux), then run `xana
connection status codex`. Update the CLI selected by `codex_program`, not only
the desktop application. An app-server error that rejects the request value
`workspaceWrite` indicates an older Xana binary; update or rebuild Xana so it
sends the current `workspace-write` request enum.

Codex model discovery retains the reasoning efforts and default advertised by
each model. Use `--effort auto` or a value listed by `xana model`; Xana does
not hard-code a vendor effort enum. `--summary` accepts `auto`, `concise`,
`detailed`, or `off`. The equivalent in-chat controls are:

```text
/model codex/MODEL
/reasoning
/reasoning auto
/reasoning ADVERTISED_EFFORT
/reasoning-summary auto|concise|detailed|off
/activity quiet|normal|verbose
/details
```

Model, reasoning-effort, and summary changes apply to later turns while
retaining the same Codex thread and context. `/activity` is session-only and
changes presentation, not model compute. The normal view shows concise
reasoning summaries, plans, and work progress. Verbose also shows command
output, full diffs, and raw reasoning text when Codex emits it. `/details`
replays bounded verbose activity from the last turn. Xana cannot display
private hidden chain-of-thought, and these views do not make another model
request.

By default Xana shares the installed Codex home. `xana connection logout codex
--yes` can therefore affect other Codex clients using that home. For an
isolated account, set an absolute `--codex-home` when adding the connection.
`--codex-program` selects a compatible executable. Codex connections do not
accept `base_url` or a Xana credential reference.

## Version 3 example

```toml
version = 3
default_profile = "default"
default_child_route = "worker"
permission_mode = "ask"

[shell]
kind = "platform"

[providers.ollama]
kind = "ollama"

[providers.ollama.models."qwen3:1.7b"]
input_modalities = ["text"]
tools = true

[providers.openrouter]
kind = "openrouter"
credential = { source = "environment", variable = "OPENROUTER_API_KEY" }

[providers.openrouter.models."openai/gpt-4.1"]
input_modalities = ["text", "image"]
tools = true

[providers.codex]
kind = "codex"
codex_program = "codex"
# codex_home = "C:/absolute/isolated/codex-home"

[providers.codex.models."ADVERTISED_MODEL_ID"]

[profiles.default]
connection = "ollama"
model = "qwen3:1.7b"
max_tool_rounds = 8

[profiles.worker]
connection = "openrouter"
model = "openai/gpt-4.1"
capabilities = ["fs.read", "fs.list", "xana.docs.read"]
permission_mode = "ask"
max_tool_rounds = 4

[profiles.worker.orchestration]
max_fan_out = 4
max_descendants = 8
max_concurrency = 2
deadline_seconds = 300
max_context_tokens = 8192
max_report_bytes = 32768
max_artifact_bytes = 8388608

[routes.worker]
profile = "worker"
```

Version 1 and 2 documents remain readable. Their legacy
`profiles.<id>.provider` input maps to `connection`; specifying both is an
error. The first structured connection edit writes version 3 and the canonical
key while preserving TOML comments. Model selection is stored separately in
`data/selection.toml`, so choosing a model does not rewrite this file. The
selection document is version 2 and may include non-secret `reasoning_effort`
and `reasoning_summary` fields for Codex. Legacy version 1 selections remain
readable.

### Field reference

| Field | Meaning |
|---|---|
| `version` | This build accepts schema 1, 2, or 3 and writes 3 |
| `default_profile` | Profile used when no separate selection exists |
| `default_child_route` | Optional exact route used when child work omits a route name |
| `permission_mode` | `deny`, `ask`, or `allow` |
| `permission_rules` | Optional scoped user authority rules |
| `shell.kind` / `shell.program` | Platform shell policy for `run_command` |
| `providers.<id>.kind` | `ollama`, `openai_compat`, `openai`, `openrouter`, `anthropic`, or `codex` |
| `base_url` | Optional/defaulted HTTP(S) API base; required for custom compatible providers and forbidden for Codex |
| `credential` | Tagged `environment` or `stored` reference; never the secret |
| `models.<id>` | Optional capability/limit overrides for a connection-owned model |
| `codex_program` / `codex_home` | Codex-only executable and absolute account-home override |
| `profiles.<id>.connection` | Exact connection used by the profile; legacy `provider` remains read-only migration input |
| `profiles.<id>.model` | Exact configured/cached model id |
| `reasoning_effort` / `reasoning_summary` | Optional managed Codex model options validated against local catalog metadata |
| `capabilities` | Optional exact built-in logical capability ids; omitted means all built-ins and `[]` means none |
| `profiles.<id>.permission_mode` | Optional ceiling that can narrow but never widen the global policy |
| `max_tool_rounds` | Native loop limit, `1..=64`, default 8 |
| `profiles.<id>.orchestration` | Bounded fan-out, descendants, concurrency, deadline, context, report, and artifact defaults |
| `routes.<id>.profile` | Exact profile selected by a child task route; no fallback |

Model overrides accept `input_modalities = ["text", "image"]`, `tools`,
`reasoning`, `context_tokens`, and `max_output_tokens`. Unknown modalities and
unknown TOML fields are errors. Discovered capability metadata is cached and
merged with explicit fields; unknown capabilities fail closed.

## Secret storage

For `{ source = "stored", id = "openrouter" }`, Xana uses service
`dev.xana.credentials` in Windows Credential Manager, macOS Keychain, or Linux
Secret Service. There is no plaintext fallback. `{ source = "environment",
variable = "OPENROUTER_API_KEY" }` reads only that process variable.

`config.toml`, model caches, selections, sessions, artifacts, logs, and
`XANA_HOME` never store static key bytes. Deleting a Xana key is separate from
logging out of Codex because the credential owners differ.

## Paths and `XANA_HOME`

An explicitly non-empty `XANA_HOME` must be an absolute path. It maps
`config.toml`, `data/`, `cache/`, and `run/` beneath one root, but does not
redirect the operating-system credential store or Codex unless `codex_home` is
also set.

macOS/Linux:

```bash
export XANA_HOME="$HOME/.xana"
```

Windows PowerShell:

```powershell
$env:XANA_HOME = "$HOME\.xana"
```

Windows Git Bash must translate the POSIX-looking path into a Windows absolute
path before starting a Windows executable:

```bash
export XANA_HOME="$(cygpath -m "$HOME/.xana")"
```

`/c/Users/name/.xana` is absolute to Git Bash but not to Rust's Windows path
parser, because `xana.exe` receives it as a Windows path. `~` is expanded only
when unquoted by a shell; Xana does not perform tilde expansion.

Without the override, Xana uses platform application directories:

| Platform | Configuration |
|---|---|
| Linux | `$XDG_CONFIG_HOME/xana/config.toml` or `~/.config/xana/config.toml` |
| macOS | `~/Library/Application Support/io.github.labcoder.xana/config.toml` |
| Windows | `%APPDATA%\labcoder\xana\config\config.toml` |

`xana config path` is the authority for the current process.

## Shell and permission policy

`run_command` supports `platform`, `posix`, `git_bash`, `powershell`, and
`cmd` where appropriate. Program and arguments are passed separately. Xana
does not classify shell text or claim OS containment. `permission_mode` and
rules govern Xana's runtime authorization; see [Permissions](permissions.md).

## Deliberate limits

Quick Setup intentionally creates one functional default connection/profile.
It does not edit the full profile, route, orchestration, or shell schema. Xana
has no hosted OAuth service, direct ChatGPT backend transport, Claude
subscription login, automatic catalog refresh at ordinary startup, automatic
model routing, plaintext secret fallback, or automatic legacy repair. Use the
advanced connection/model commands or edit validated TOML for those concerns.
