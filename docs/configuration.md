# Xana configuration

Xana loads one human-authored, versioned TOML document at startup. Startup
prints the resolved configuration path before loading the file.

## Complete version 1 example

```toml
version = 1
default_profile = "default"
permission_mode = "allow"

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

## File location

Without a non-empty `XANA_HOME`, Xana follows platform conventions through the
stable application identity `io.github.labcoder.xana`:

| Platform | Default `config.toml` location |
|---|---|
| Linux | `$XDG_CONFIG_HOME/xana/config.toml`, or `~/.config/xana/config.toml` |
| macOS | `~/Library/Application Support/io.github.labcoder.xana/config.toml` |
| Windows | `%APPDATA%\labcoder\xana\config\config.toml` |

Xana prints the actual resolved path at startup because platform environment
and user folders can change these defaults.

The resolver keeps state categories separate:

| Category | Platform default | With `XANA_HOME=/absolute/root` |
|---|---|---|
| Config file | platform config directory + `config.toml` | `/absolute/root/config.toml` |
| Durable data | platform data directory | `/absolute/root/data/` |
| Disposable cache | platform cache directory | `/absolute/root/cache/` |
| Runtime coordination | platform runtime directory, or cache + `run/` | `/absolute/root/run/` |

Path resolution does not create these directories. The code that first writes
a category is responsible for creating only the directory it owns.

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

These examples affect the current shell. Persistent setup belongs in the
appropriate user-level shell or environment configuration for that platform.

## Version 1 field reference

| Field | Type | Required? | Default | Meaning |
|---|---|---:|---|---|
| `version` | integer | Yes | None | Configuration schema version; this build accepts `1` |
| `default_profile` | string | Yes | None | Name beneath `[profiles]` selected at startup |
| `permission_mode` | string enum | Yes | None | Phase 1 behavior; only `allow` is accepted |
| `providers.<name>.kind` | string enum | Yes | None | Provider adapter kind; v1 accepts `openai_compat` |
| `providers.<name>.base_url` | string | Yes | None | Absolute HTTP(S) provider base URL |
| `profiles.<name>.provider` | string | Yes | None | Name beneath `[providers]` |
| `profiles.<name>.model` | string | Yes | None | Non-blank model identifier sent to the provider |
| `profiles.<name>.max_tool_rounds` | integer | No | `8` | Bounded agent tool rounds; accepted range is `1..=64` |

Unknown fields are errors. This keeps misspellings and unsupported future
settings from being silently ignored.

## Name and URL rules

Provider/profile names must begin with a lowercase ASCII letter or digit. The
remaining characters may be lowercase ASCII letters, digits, `_`, or `-`.

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

Secrets will use operating-system credential storage or explicit environment
references when those provider features land. An unknown field such as
`api_key` is rejected in version 1.

## Migrating from `config.kv`

Xana does not rewrite the temporary Phase 1 file automatically. An old file:

```text
model=qwen3:1.7b
base_url=http://localhost:11434/v1
```

can become:

```toml
version = 1
default_profile = "default"
permission_mode = "allow"

[providers.ollama]
kind = "openai_compat"
base_url = "http://localhost:11434/v1"

[profiles.default]
provider = "ollama"
model = "qwen3:1.7b"
```

Choose the `ollama` and `default` names yourself, and confirm that the current
automatic `allow` behavior is your intent. Write the result to `config.toml`
beside the old file. When both files exist, Xana uses `config.toml`; remove or
archive `config.kv` after verifying startup.

## Current limitations

- Version 1 accepts only `permission_mode = "allow"`; the permission protocol
  lands in a later phase.
- Multiple named providers and profiles are validated, but only
  `default_profile` can be selected in Phase 1.
- Version 1 has one provider kind, `openai_compat`, and no plaintext credential
  fields.
