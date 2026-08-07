# Xana configuration

> Audience: People installing, configuring, or using Xana.

Xana loads one human-authored, versioned TOML document at startup. Startup
prints the resolved configuration path before loading the file.

## First-time initialization

From a source checkout, start the interactive initializer:

```bash
cargo run -- init
```

It offers two connection routes:

- local Ollama, with provider name `ollama` and base URL
  `http://localhost:11434/v1`; or
- a custom unauthenticated OpenAI-compatible HTTP(S) endpoint.

Both routes require a model. The prompt defaults the bounded tool-call limit
to `8`, collects the shell used by `run_command`, explains `deny`, `ask`, and
`allow`, defaults permission to `ask`, previews the selected values, and asks for final
confirmation. The generated TOML is serialized through the version 1 schema
and parsed back through the production validator before any directory or file
is created.

For automation, every provider/model value and the permission mode must be
explicit:

```bash
cargo run -- init \
  --non-interactive \
  --provider-name ollama \
  --base-url http://localhost:11434/v1 \
  --model qwen3:1.7b \
  --max-tool-rounds 8 \
  --shell platform \
  --permission-mode ask
```

Noninteractive setup never reads stdin. Omitting `--max-tool-rounds` uses `8`,
and omitting `--shell` uses `platform`; the provider name, URL, model, and
permission mode have no hidden defaults. `--shell-program PATH` explicitly
overrides the program used by the selected shell.

Add `--dry-run` to either route to render and validate the proposed document
without creating its parent directory or `config.toml`. Interactive dry-run
still asks the setup questions; a scripted dry-run uses the complete
noninteractive command above plus `--dry-run`.

Initialization handles existing state conservatively:

| State | Result |
|---|---|
| Valid `config.toml` | Reports that Xana is already initialized, exits successfully, and does not prompt or rewrite it |
| Invalid `config.toml` | Reports the real validation error and leaves the bytes unchanged |
| Legacy `config.kv` without `config.toml` | Reports the manual migration requirement and creates no new file |
| Declined confirmation or EOF | Prints `No changes made.` and creates nothing |
| Another process creates the target first | The create-new open fails and never overwrites the winner |

Interactive setup requires a terminal. Piped input is rejected instead of
hanging; use the explicit noninteractive route in scripts.

## Configuration diagnostics

These commands inspect the same active path used by startup and initialization:

```bash
cargo run -- config path
cargo run -- config check
```

`config path` prints the resolved `config.toml` path without loading it.
`config check` loads and validates the complete document without constructing
an agent or contacting a provider. Bare `cargo run` remains normal chat; when
the file is missing it points explicitly to `xana init`.

## Complete version 1 example

```toml
version = 1
default_profile = "default"
permission_mode = "ask"

[shell]
kind = "platform"

[providers.ollama]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "ollama"
model = "qwen3:1.7b"
# max_tool_rounds = 8
```

Change the URL and model to values supported by your provider. Provider and
profile names such as `ollama` and `default` are user-chosen references.

## Permission policy

Version 1 accepts `deny`, `ask`, and `allow` as the default
`permission_mode`, plus an optional `permission_rules` array. Rules can match
tool name, effect, relative workspace path, and exact command. All matching
rules are combined with deny-before-ask-before-allow precedence, regardless of
their TOML order. See [Permissions](permissions.md) for the complete schema,
examples, scopes, session grants, and fail-closed behavior.

Existing documents with `permission_mode = "allow"` remain compatible and
authorize all built-in host-tool effects automatically unless a matching rule
narrows them. New initialization defaults to `ask`.

## Shell execution

`run_command` interprets one command string through the configured shell. Its
optional `cwd` is relative to Xana's launch workspace and must resolve to an
existing directory beneath that workspace. Before a process spawn, Xana plans
the selected shell, canonical working directory, and exact command and sends
that immutable scope through the same permission broker as every file tool.
An `ask` may be denied, allowed once, or allowed for the exact session command
scope. EOF and blank input deny.

The shell kinds are:

| Kind | Availability | Default program and argv prefix |
|---|---|---|
| `platform` | All platforms | `sh -lc` on macOS/Linux; `powershell.exe -NoLogo -NoProfile -NonInteractive -Command` on Windows |
| `posix` | macOS/Linux | `sh -lc` |
| `git_bash` | Windows | `bash.exe -lc` |
| `powershell` | Windows | `powershell.exe -NoLogo -NoProfile -NonInteractive -Command` |
| `cmd` | Windows | `cmd.exe /D /S /C` |

Set `program` only when the executable cannot be found by its default name or
when selecting a specific compatible installation:

```toml
[shell]
kind = "git_bash"
program = "C:/Program Files/Git/bin/bash.exe"
```

Xana passes the program and argument vector separately to the operating
system. It does not classify command text, create a sandbox, or provide a
timeout, background process, PTY, persistent grant, or process containment.
Stdout and stderr are captured independently; each is limited to 32 KiB and
reports whether truncation occurred.

## File location

Path selection follows this precedence: a non-empty absolute `XANA_HOME` wins;
otherwise Xana follows platform conventions through the stable application
identity `io.github.labcoder.xana`:

| Platform | Default `config.toml` location |
|---|---|
| Linux | `$XDG_CONFIG_HOME/xana/config.toml`, or `~/.config/xana/config.toml` |
| macOS | `~/Library/Application Support/io.github.labcoder.xana/config.toml` |
| Windows | `%APPDATA%\labcoder\xana\config\config.toml` |

Xana prints the resolved path at startup because platform environment and user
folders can change these defaults.

The resolver keeps state categories separate:

| Category | Platform default | With `XANA_HOME=/absolute/root` |
|---|---|---|
| Config file | platform config directory + `config.toml` | `/absolute/root/config.toml` |
| Durable data | platform data directory | `/absolute/root/data/` |
| Disposable cache | platform cache directory | `/absolute/root/cache/` |
| Runtime coordination | platform runtime directory, or cache + `run/` | `/absolute/root/run/` |

Path resolution does not create these directories. The code that first writes
a category is responsible for creating only the directory it owns.

Starting chat creates `data/sessions/` and `data/artifacts/`. Session JSONL,
permission audits, context metadata, and immutable artifact bytes never belong
in `config.toml`; see [Sessions](sessions.md).

## `XANA_HOME`

Set `XANA_HOME` to an absolute path to use one predictable portable root for
Xana's shared backend locations. A missing or explicitly empty value uses the
platform defaults. A non-empty relative value is rejected.

The override does not redirect operating-system credential storage or
frontend-local window and UI preferences.

macOS or Linux:

```bash
export XANA_HOME="$HOME/.xana"
cargo run
```

Windows Git Bash:

```bash
export XANA_HOME="$(cygpath -m "$HOME/.xana")"
cargo run
```

Windows PowerShell:

```powershell
$env:XANA_HOME = "$HOME\.xana"
cargo run
```

Windows Command Prompt:

```bat
set "XANA_HOME=%USERPROFILE%\.xana"
cargo run
```

These examples affect the active shell. Persistent setup belongs in the
appropriate user-level shell or environment configuration for that platform.
Xana reads `XANA_HOME`; `xana init` never writes it or edits a shell profile.

## Terminal presentation

Bare interactive startup and interactive initialization show Xana's static
terminal portrait and wordmark. Redirected output, configuration diagnostics,
noninteractive setup, and dry-run omit it. Use `--no-banner` to suppress it;
setting `NO_COLOR` retains the mark without ANSI color. A dumb terminal also
receives plain operational output without the mark.

## Version 1 field reference

| Field | Type | Required? | Default | Meaning |
|---|---|---:|---|---|
| `version` | integer | Yes | None | Configuration schema version; this build accepts `1` |
| `default_profile` | string | Yes | None | Name beneath `[profiles]` selected at startup |
| `permission_mode` | string enum | Yes | None | Default tool authority: `deny`, `ask`, or `allow`; new initialization selects `ask` |
| `permission_rules` | array of tables | No | Empty | User-owned rules with id, decision, and tool/effect/workspace/command matchers |
| `shell.kind` | string enum | No | `platform` | `platform`, `posix`, `git_bash`, `powershell`, or `cmd`, subject to platform support |
| `shell.program` | path | No | Selected kind's program | Explicit executable used for the selected shell |
| `providers.<name>.kind` | string enum | Yes | None | Provider adapter kind; version 1 accepts `openai_compat` |
| `providers.<name>.base_url` | string | Yes | None | Absolute HTTP(S) provider base URL |
| `profiles.<name>.provider` | string | Yes | None | Name beneath `[providers]` |
| `profiles.<name>.model` | string | Yes | None | Non-blank model identifier sent to the provider |
| `profiles.<name>.max_tool_rounds` | integer | No | `8` | Bounded agent tool rounds; accepted range is `1..=64` |

Unknown fields are errors. This keeps misspellings and unsupported settings
from being silently ignored.

## Name and URL rules

Provider and profile names must begin with a lowercase ASCII letter or digit.
Remaining characters may be lowercase ASCII letters, digits, `_`, or `-`.

Valid examples: `ollama`, `local_qwen`, `fast-worker`, `worker2`.

Invalid examples: `Ollama`, `worker.dev`, an empty name, or a name containing
spaces.

A provider base URL must:

- be an absolute `http` or `https` URL with a host;
- be usable as a base URL;
- contain no username or password;
- contain no query string; and
- contain no fragment.

A path component such as `/v1` is valid.

## What does not belong in `config.toml`

Do not store plaintext credentials, OAuth sessions, conversation or operation
records, artifacts, cache data, audit records, logs, or frontend-local UI
preferences in this file.

Version 1 does not support credential fields or authenticated-provider setup.
An unknown field such as `api_key` is rejected.

## Migrating from `config.kv`

Xana does not rewrite the legacy `config.kv` file automatically. An old file:

```text
model=qwen3:1.7b
base_url=http://localhost:11434/v1
```

can become:

```toml
version = 1
default_profile = "default"
permission_mode = "allow"

[shell]
kind = "platform"

[providers.ollama]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "ollama"
model = "qwen3:1.7b"
```

Choose the `ollama` and `default` names yourself, and confirm that automatic
host-tool `allow` behavior is your intent. Use `ask` instead for the new
initializer default. The platform shell default is
portable; select another supported shell explicitly if needed. Write the
result to `config.toml` beside the old file. When both files exist, Xana uses
`config.toml`; remove or archive `config.kv` after verifying startup.

## Limitations

- Permission audit facts are appended to the durable session, while session
  grants remain memory-only. There are no persistent grants, remote controller
  roles, or permission-derived process containment.
- Multiple named providers and profiles are validated, but the executable
  selects only `default_profile`.
- Version 1 has one provider kind, `openai_compat`, and no plaintext credential
  fields.
- Initialization does not collect credentials or onboard authenticated remote
  providers.
- There is no `--force`, automatic repair, automatic legacy migration, project
  initialization, shell-profile editing, or general configuration editor.
