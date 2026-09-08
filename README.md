![xana banner image](./assets/xana-clean.jpg)

# Xana

[![CI](https://github.com/labcoder/xana/actions/workflows/ci.yml/badge.svg)](https://github.com/labcoder/xana/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

Xana is a terminal-first personal AI agent for work in local repositories. It
can chat, edit files, run commands with permission, read documents, use images,
and coordinate bounded child tasks.

Use Xana's native agent loop with Ollama, OpenAI-compatible servers, OpenAI,
OpenRouter, or Anthropic. You can also connect a ChatGPT Plus or Pro account
through a local Codex app-server, which keeps ownership of its login, tools,
sandbox, and conversation history.

> [!IMPORTANT]
> Xana is a developer preview. The current release is a terminal application,
> and its configuration and runtime contracts may change between previews.

## What Xana includes

| Area | Support |
| --- | --- |
| Interfaces | Source-built native GPUI Desktop with graphical setup/settings/recovery, adaptive full-screen TUI, append-only terminal, and JSON/text one-shot output |
| Models | Local Ollama, OpenAI-compatible endpoints, OpenAI, OpenRouter, Anthropic, and managed Codex |
| Native tools | Bounded workspace discovery/search, paged reads, explicit file creation, atomic exact edits, timed commands, text/CSV extraction, bundled Xana docs, and [public web search/page reading](docs/user/public-web.md) |
| State | Durable native sessions with lossless history, resumable round-budget boundaries, and bounded compaction; Codex thread handles, immutable artifacts, projects, and named profiles |
| Privacy | Opt-in [protected storage](docs/user/protected-storage.md), reviewed legacy migration, encrypted backup/restore, local OS unlock, independent recovery, and explicit locking |
| Extensions | Agent Skills, declarative Agent Plugins, allowlisted MCP servers, and trusted A2A agents |
| Media | PNG, JPEG, and GIF input plus named image-generation and vision routes |
| Local autonomy | Explicit [durable jobs](docs/user/durable-schedules.md), selected-file and named GitHub CI triggers, bounded background supervision |
| Retained work | [Retained children](docs/user/retained-workers.md), explicit follow-ups and cited context operations under cumulative parent limits |
| Personal learning | Scoped [memory](docs/user/personal-memory.md), reviewable learned candidates, exact undo and inert Skill drafts |
| Completion | [Finite-work evidence](docs/user/completion-evidence.md) distinguishes delivered answers from observed checks and unresolved effects |
| Browser | Optional [dedicated local browser](docs/user/local-browser.md) with reviewed recipients, bounded evidence, takeover and owned cleanup on the qualified Windows adapter |

Xana keeps its native engine separate from terminal presentation and provider
wire formats. The same application policy drives interactive chat, automation,
and attached local clients.

## Install

Published previews provide native builds for macOS ARM64 and Intel, x64 glibc
Linux, and x64 Windows. The installers verify the release manifest and SHA-256
digest before replacing the per-user executable. You do not need Rust for
these installs. A Git tag or draft is not a public release; the installers use
the latest published release.

### macOS or x64 glibc Linux

```bash
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://github.com/labcoder/xana/releases/latest/download/xana-installer.sh \
  | bash
```

### Windows x64 PowerShell

```powershell
$source = Invoke-RestMethod `
  -Uri 'https://github.com/labcoder/xana/releases/latest/download/xana-installer.ps1'
& ([scriptblock]::Create($source))
```

Preview binaries are unsigned. macOS builds lack notarization, and Windows
builds lack Authenticode signatures. Use the source install if unsigned
binaries do not fit your trust policy.

### Install from Git

Xana is not published to crates.io. A Git install requires Rust 1.97.1 and the
checked-in lockfile:

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
```

See the [installation guide](docs/user/installation.md) for exact-version
installs, archive and attestation checks, custom directories, updates,
troubleshooting, and removal.

## Quick start

Run guided setup, then start a conversation:

```bash
xana setup
xana
```

Choose Start with one connection, Full customize, or Blank. Blank records no
provider or model and leaves `xana connect provider` as the next action. A
connection setup fetches the chosen connection's live model catalog before it
saves a selection. API-key connections can use the operating-system credential store
or one named environment variable. If you choose managed Codex, install and
sign in to a compatible Codex CLI first.

Run one noninteractive turn with `-p`:

```bash
xana -p "Summarize this repository"
xana --json -p "List the main risks in this change"
xana --output stream-json -p "Report progress while checking this workspace"
```

One-shot mode writes the final result to stdout and sends activity to stderr.
Requests that need an approval fail closed when no interactive controller is
present. The repository-private `stream-json` form instead writes bounded,
ordered JSONL observations, one end-of-run semantic summary, and one
authoritative result frame; see
[Plain mode and automation](docs/user/automation.md).

## Native and managed execution

| Mode | Owner | Use it with |
| --- | --- | --- |
| Native | Xana owns the agent loop, tools, permissions, and durable session | Ollama, OpenAI-compatible servers, OpenAI, OpenRouter, Anthropic |
| Managed Codex | Codex app-server owns inference, tools, sandbox, approvals, login, and inner history | ChatGPT Plus or Pro through an installed Codex CLI |

Switching execution owners starts a new conversation. Xana does not translate
history between its native loop and Codex. Read
[Connections, models, and managed runtimes](docs/architecture/models-and-managed-runtimes.md)
for the ownership boundaries.

## Common commands

| Command | Purpose |
| --- | --- |
| `xana setup` | Add or update a connection and choose a model |
| `xana settings` | Browse and edit settings in the terminal workspace |
| `xana connect` | Open the provider-neutral integration hub |
| `xana connection test ID` | Test one configured connection without changing durable state |
| `xana connection repair ID` | Re-establish one connection and repair only its derived model catalog |
| `xana model` | Inspect the active model and available catalog |
| `xana usage` | Inspect model facts and cached provider/account usage; add `--refresh` for a bounded live refresh |
| `xana usage ledger` / `xana budget` | Inspect [durable usage and local admission limits](docs/user/usage-budgets.md) in a protected home |
| `xana memory` | Inspect, correct or forget [scoped personal memory](docs/user/personal-memory.md), authorize bounded learning, and control next-turn use |
| `xana capabilities` | Report what is configured, selected, authorized, and presentable here without network probes |
| `xana conversation list` | List conversations for the current workspace (`session` remains an alias) |
| `xana conversation preview ID` | Print a bounded, read-only native transcript preview without acquiring control |
| `xana conversation attach ID` | Open the interactive frontend on one exact idle retained Conversation |
| `xana conversation continue` | Continue the latest compatible Conversation for this workspace |
| `xana conversation search QUERY` | Search bounded retained native Conversation text; add `--json` for structured output |
| `xana conversation branch ID --at POINT` | Preserve a source and create an explicit continuation |
| `xana serve` | Run an explicit loopback-only foreground host for the current workspace |
| `xana attach [--control] [--takeover]` | Observe that host or request exact Conversation controller authority |
| `xana --continue` | Continue the latest compatible conversation |
| `xana doctor` | Inspect configuration, credentials, paths, and runtime readiness |
| `xana logs list` | Inspect local metadata-only diagnostics |
| `xana --help` | Show the complete CLI command surface |

Inside chat, use `/help` or the TUI command palette to discover conversation
commands.

## Safety and data ownership

Xana applies its permission policy before native tools read, write, or run a
command. An allowed native tool retains the Xana process's host access; Xana
does not provide a native sandbox. Managed Codex turns use Codex's sandbox and
approval system.

Xana stores static API keys in the operating-system credential service or
reads one configured environment variable. Codex retains its own credentials.
MCP, A2A, and focused-service requests pass selected data through Xana's
recipient and data-class approval boundary.

Read [Permissions](docs/user/permissions.md) and
[Outbound data approvals and privacy](docs/user/outbound-data.md) before
granting broad tool or integration access.

## Documentation

The [documentation index](docs/README.md) separates user guides from
engineering contracts. Useful starting points include:

- [Configuration and provider setup](docs/user/configuration.md)
- [Usage, limits, and model facts](docs/user/usage.md)
- [Full-screen terminal UI](docs/user/tui.md)
- [Plain mode and automation](docs/user/automation.md)
- [Conversations and recovery](docs/user/sessions.md)
- [Workspace file, search, and command tools](docs/user/workspace-tools.md)
- [Rich content, resources, and safe fallbacks](docs/user/rich-content.md)
- [Native bounded web fetch](docs/user/web-fetch.md)
- [Agent Skills](docs/user/skills.md) and [Agent Plugins](docs/user/plugins.md)
- [MCP integrations](docs/user/mcp.md)

Contributors should start with the [architecture](docs/architecture/README.md)
and [design principles](docs/principles.md).

## Development

The repository pins Rust 1.97.1.

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo build --locked
cargo run -- setup
```

The native Desktop is developed in the same Cargo workspace and includes the
matching Xana runtime. It owns that embedded runtime when the workspace is
unclaimed, or attaches to a compatible live foreground Xana host; it never
discovers or launches a `xana` executable from `PATH`:

```bash
cargo run --locked -p xana-desktop
cargo run --locked -p xana-desktop -- --workspace .
```

Desktop currently provides native and managed Codex Conversations, native
menus, a searchable command palette, a bounded shortcut set, redacted
notifications, safe close handling, and one instance per canonical
`XANA_HOME`. Its persistent Project/Conversation sidebar and bounded,
recoverable Workbench support trusted panels, resizable split layouts, one user
default, and inert layout sharing. Its contextual navigation actions rename and
archive Projects, move or ungroup Conversations, and create exact
source-preserving branches or cross-workspace continuations through the shared
runtime services. The isolated Message panel supports multiline drafts, multiple
validated image attachments, drag/drop and clipboard images, queued follow-ups,
interrupt, retry, and explicit edit/regenerate recovery. Managed Codex model and
reasoning changes preserve the vendor thread when accepted; native model and
Profile changes start a fresh Conversation instead of claiming to rewrite
history. Complete `xana setup` first.
Launching without arguments opens a read-only workspace chooser; it neither
infers the process directory nor creates a Project or Conversation. Use
`--workspace .` during repository development to open the current directory
directly.
Graphical setup, Settings, connection/model management, Doctor, and recovery are
available alongside a global/Project Espejo command center. Complete graphical
rich-content rendering includes sanitized Markdown, selectable code and diff
content, safe links, bounded static image previews, and typed fallbacks for
formats without a reviewed native adapter.
When a compatible CLI/TUI foreground host already owns the selected workspace,
Desktop joins it as a local client instead of creating a second writer. It
requests only an unclaimed controller lease and otherwise opens as an observer;
takeover remains explicit in the terminal host flow.
See [using Xana Desktop](docs/user/desktop.md) and
[Desktop development](docs/contributing/desktop-development.md).

Contributors can inspect the provider-free visual foundation and deterministic
component fixtures without creating configuration:

```bash
cargo run --locked -p xana-desktop -- --catalog
```

Run the required checks before submitting a change:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

Windows contributors can run the full installer, release-contract, and package
suite with:

```powershell
./scripts/ci-local.ps1 -RequireClean
```

Read [Code organization](docs/contributing/code-organization.md) before moving
module boundaries or adding public interfaces. Xana's Cargo workspace contains
the `xana` application/runtime package and the native `xana-desktop` binary;
their repository-private Rust seam is not a stable SDK.

## License

Xana is available under the [MIT License](./LICENSE).
