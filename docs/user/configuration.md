# Xana configuration

> Audience: People installing, configuring, or using Xana.

Xana loads one strict, versioned `config.toml`, with a 1 MiB file limit.
Human-authored configuration declares connections and profiles; static
secrets, cached catalogs, model selection, sessions, and artifacts live
elsewhere.

## Initialize

```bash
xana init
xana config path
xana config check
```

The initializer creates an unauthenticated local Ollama or custom
OpenAI-compatible connection, asks for the model, shell, bounded tool rounds,
and `deny`/`ask`/`allow` permission default, then validates before a create-new
write. It never replaces an existing file. `--dry-run` renders without writing;
`--non-interactive` requires provider, URL, model, and permission values and
never reads stdin.

```bash
xana init --non-interactive \
  --provider-name ollama \
  --base-url http://localhost:11434/v1 \
  --model qwen3:1.7b \
  --max-tool-rounds 8 \
  --shell platform \
  --permission-mode ask
```

## Connections and models

`xana model` is the normal discovery and selection entry point:

```text
xana model
xana model list --connection openrouter
xana model refresh openrouter
xana model use openrouter/openai/gpt-4.1
xana model use codex/gpt-5.3-codex --effort high --summary auto
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
xana connection add codex --kind codex --model gpt-5.3-codex
xana connection status codex
xana connection login codex
# Headless alternative:
xana connection login codex --device-code
xana connection refresh codex
xana model use codex/gpt-5.3-codex
xana
```

The exact model must be advertised by the installed Codex app-server; use
`xana model list --connection codex` after refresh. Xana launches `codex
app-server --stdio`. Codex owns browser/device login, token storage and
refresh, inference, tools, sandbox, approvals, and inner thread history. Xana
never reads or copies Codex's auth file and needs no hosted OAuth callback
server.

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

## Version 2 example

```toml
version = 2
default_profile = "default"
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

[providers.codex.models."gpt-5.3-codex"]

[profiles.default]
provider = "ollama"
model = "qwen3:1.7b"
max_tool_rounds = 8
```

Version 1 documents remain readable. The first connection edit writes version
2 while preserving TOML comments. Model selection is stored separately in
`data/selection.toml`, so choosing a model does not rewrite this file. The
selection document is version 2 and may include non-secret `reasoning_effort`
and `reasoning_summary` fields for Codex. Legacy version 1 selections remain
readable.

### Field reference

| Field | Meaning |
|---|---|
| `version` | This build accepts schema 1 or 2 and writes 2 |
| `default_profile` | Profile used when no separate selection exists |
| `permission_mode` | `deny`, `ask`, or `allow` |
| `permission_rules` | Optional scoped user authority rules |
| `shell.kind` / `shell.program` | Platform shell policy for `run_command` |
| `providers.<id>.kind` | `ollama`, `openai_compat`, `openai`, `openrouter`, `anthropic`, or `codex` |
| `base_url` | Optional/defaulted HTTP(S) API base; required for custom compatible providers and forbidden for Codex |
| `credential` | Tagged `environment` or `stored` reference; never the secret |
| `models.<id>` | Optional capability/limit overrides for a connection-owned model |
| `codex_program` / `codex_home` | Codex-only executable and absolute account-home override |
| `profiles.<id>.provider` | Connection used by the profile |
| `profiles.<id>.model` | Initial model id |
| `max_tool_rounds` | Native loop limit, `1..=64`, default 8 |

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

Initialization does not yet provide the full connection/model manager; add
authenticated and Codex connections afterward. Xana has no hosted OAuth
service, direct ChatGPT backend transport, Claude subscription login,
automatic catalog refresh, automatic model routing, plaintext secret fallback,
`--force`, or automatic legacy repair.
