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

Interactive terminal surfaces use one semantic presentation language with
dark, light, monochrome, Unicode/ASCII, narrow-width, and reduced-motion
fallbacks. Redirected output and `NO_COLOR` remain plain and control-free. See
[Terminal presentation](docs/user/presentation.md) for automatic detection and
the separate machine-local preference file.

Native connections run Xana's own agent loop. Codex is a managed runtime: Xana
provides the CLI and process/event/approval bridge but does not wrap the turn
in a second model call or copy Codex credentials. When Xana creates a managed
thread, it supplies its canonical built-in identity as a developer instruction,
so the assistant presents itself as Xana while Codex retains ownership of its
base instructions and inner loop.

## Install a developer preview

Published previews use verified per-user installers and require no Rust, Node,
Python, elevation, or checkout. A draft or tag alone is not a public release;
if the latest URL is unavailable, use the locked source path below.

macOS or x64 glibc Linux:

```bash
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  https://github.com/labcoder/xana/releases/latest/download/xana-installer.sh \
  | bash
```

Windows x64 PowerShell:

```powershell
$source = Invoke-RestMethod `
  -Uri 'https://github.com/labcoder/xana/releases/latest/download/xana-installer.ps1'
& ([scriptblock]::Create($source))
```

The installers verify one exact release manifest and SHA-256 before activation,
then hand readiness to `xana setup --if-needed`. Preview binaries are unsigned:
macOS is not notarized and Windows is not Authenticode-signed. Verify GitHub
attestations when provenance matters; do not weaken host security controls.

The locked Git source alternative requires the pinned Rust toolchain:

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
xana --version
xana setup
xana config check
xana
```

Use `--tag v0.5.1` or `--rev COMMIT_SHA` for an exact build. Xana is not
published to crates.io and has no automatic updater. Re-run an installer or
locked Cargo command to update. See [Installation, updates, verification, and
removal](docs/user/installation.md) for exact versions, manual archive and
attestation checks, custom directories, PATH consent, source checkout, and
state-preserving removal.

Development from a checkout remains supported:

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo install --path . --locked
```

## Choose a first connection

`xana setup` is the canonical first-run and rerunnable guided entry point. It
asks for Quick Setup, Full Setup, or a focused section; `xana setup --quick`
selects Quick directly. Xana does not preselect or recommend a provider. It
first establishes the chosen local,
API-key, or managed Codex connection and fetches its live catalog; only then
does it offer model and reasoning choices. Ordinary configuration remains in
memory until the redacted review is confirmed, then the credential reference
and valid config are committed atomically. Cancelling preserves the previous
installation. A minimal Ollama document is:

Installers and automation can ask Xana to own the readiness decision:

```bash
xana setup --if-needed
```

Healthy configuration returns success without mutation or provider traffic.
When setup or repair is needed, an interactive terminal enters the same
canonical flow; redirected/noninteractive use returns exit code `10` with a
versioned `XANA_SETUP_RESULT` receipt and exact `xana setup`/`xana doctor`
next steps. Shell wrappers never parse or repair `config.toml` themselves.

```toml
version = 4
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

Use `xana setup --full` for the guided advanced path. Focused reruns use
`--section connection|permissions-shell|profiles-routes|appearance`; the same
entries are available as `/setup SECTION` in plain chat and the TUI command
palette. Appearance applies immediately. Managed model/reasoning changes apply
to subsequent turns when compatible. Connection owner, shell, permission,
profile, and route changes never mutate the open conversation; start a new
conversation explicitly to use their new immutable snapshot. Every durable
section has a flag-driven `--non-interactive ... --yes` form.

Existing schema 1-3 files remain readable. `xana config migrate` prints a
redacted, read-only migration plan; `xana config migrate --apply` takes an
exact backup, initializes Xana's versioned private interoperability records,
and commits schema 4 atomically. A retry is safe and byte-stable when no work
remains.

Optional projects organize conversations without owning workspaces. Start with
`xana project create NAME`, inspect them with `xana project list`, and use
`xana project assign` or `xana project ungroup` to change only the private
membership relation. See [Projects](docs/user/projects.md) for lifecycle and
cross-workspace continuation rules.

Portable sharing stays opt-in: `xana project share PROJECT_ID` creates the
strict non-secret `.agents/xana/project.toml`; another installation can run
`xana project inspect-portable` before choosing `xana project register`.

Named profiles are first-class rather than fixed roles. `xana profile create`,
`list`, `edit`, `duplicate`, `resolve`, and lifecycle commands work globally or
with `--project PROJECT_ID`. Resolution shows exact effective values,
provenance, and readiness; each conversation can freeze an immutable snapshot,
and changing profiles creates a linked continuation. See
[Profiles](docs/user/profiles.md).

Inside plain chat or the TUI, the same operations are available as `/project
...` and `/profile ...`; the TUI restores ordinary terminal mode for the typed
operation and then reopens. Session rows label their optional project or
`Ungrouped`. `xana project continue ...` previews by default and `--apply`
commits same-workspace assignment or a fresh owner-correct continuation with a
frozen profile snapshot—never workspace deletion or silent transcript copying.
Logical connection/service requirements are resolved through redacted private
bindings and never copy local credentials or authority into the repository.

Agent Skills use the standard `.agents/skills/NAME/SKILL.md` layout. `xana
skill list` indexes bounded metadata, `inspect` and `validate` review exact
sources, and `activate` loads only one selected body plus necessary contained
references. `enable`/`disable` connect qualified skills to global or project
profiles for future prompt snapshots. Same-name collisions require
qualification (`user/NAME`, `project/NAME`, or `plugin:PLUGIN/NAME`), and skill
prose—including experimental `allowed-tools` metadata—never grants tools,
permissions, credentials, egress, or execution authority. See [Agent
Skills](docs/user/skills.md).

Agent Plugins use the Agent Plugins 1.0.0 declarative package boundary.
`xana plugin review PATH` reviews a local package without installing it;
`xana plugin install PATH --yes` copies the exact reviewed tree into Xana's
private content-addressed store while leaving every skill and MCP declaration
disabled. Exact Git installs require `--git --revision COMMIT`. Explicit
`--linked` development installs stay visibly mutable. `plugin enable` binds an
exact installed revision to user/project/profile scope; `update-check` plus
explicit `update` preserves approval only for an unchanged capability set, and
`rollback`, `disable`, `remove`, and `gc` are reversible or guarded lifecycle
operations. Enabled plugin skills enter qualified skill discovery; plugin MCP
declarations remain inert until the supervised MCP phase. See [Agent
Plugins](docs/user/plugins.md).

Configured MCP servers can now contribute only explicitly allowlisted
primitives. `xana mcp list` is side-effect free; `refresh`, `tools`,
`resources`, `read`, `prompts`, and `prompt` perform explicit bounded actions.
Native conversations expose qualified `mcp.SERVER.TOOL` capabilities through
the ordinary permission broker and outbound-data gate. Resource content and
prompt templates remain attributed untrusted data and never become ambient
system instructions. The same typed commands are available as `/mcp ...` in
plain chat and the TUI. See [MCP](docs/user/mcp.md).

For local composition, `xana mcp serve --workspace PATH --profile PROFILE
--allow xana_docs` exposes an isolated, stdio-only, noninteractive MCP process.
It has no ambient Xana conversation or frontend authority and opens no network
listener. The [MCP guide](docs/user/mcp.md#local-xana-mcp-server) documents its
exact policy and shutdown boundary.

Data leaving Xana for an external integration passes one typed outbound gate.
Connection, user, profile, and conversation policy can only narrow the allowed
classes; concrete messages, files, artifacts, and metadata still require exact
selection. New recipient/class combinations require approval, unresolved
noninteractive requests fail closed, and audits retain counts and digests
rather than selected content. MCP application calls already implement that
exact dispatch seam. A2A delegation uses it for explicitly selected messages,
files, artifacts, and workspace metadata; focused-service activation remains
unavailable until its application integration lands. See [Outbound
data approvals and privacy](docs/user/outbound-data.md).

Xana also has a bounded client-side protocol and progressive catalog foundation
for MCP `2026-07-28`. It pins the exact modern discovery contract, qualifies
tool identity as `mcp.<server>.<tool>`, separates tools/resources/prompts, and
indexes only exact profile-allowlisted primitives under deterministic memory
limits. Its owned stdio process adapter has bounded I/O, cancellation, health,
minimal environment, and process-tree cleanup. Its stateless Streamable HTTP
adapter adds pinned endpoint/DNS identity, no redirects or inherited proxy,
bounded JSON/request-scoped SSE, local PKCE OAuth completion, OS-store token
rotation, and exact outbound authorization. Both transports are explicitly
configurable and expose only per-profile allowlisted primitives. See [MCP
catalog and compatibility](docs/user/mcp.md).

Remote A2A agents can be declared, explicitly refreshed, inspected, trusted,
untrusted, and removed with `xana external-agent ...`. Xana pins an A2A 1.0
JSONRPC/text compatibility subset, caches sanitized Agent Card metadata in
private state, and invalidates trust whenever meaningful identity changes.
Profile-selected trusted agents expose a bounded qualified delegation tool;
Xana gates exact selected data, streams attributed activity, ingests immutable
artifacts, tracks task state, and supports explicit or best-effort cancellation.
Startup never discovers endpoints implicitly, and trust alone sends no task or
local data. See [External A2A agents](docs/user/external-agents.md).

## Diagnose and recover an installation

`xana doctor` performs bounded read-only checks of configuration, credential
references, live native catalogs, Codex executable/app-server/account/catalog/
rate-limit state, Xana-owned paths, presentation preferences, terminal mode,
and the current workspace's host descriptor. Each stable finding includes its
evidence source and an exact next command. `--output json` emits the versioned
redacted report. `xana doctor --fix` separately previews and confirms only
deterministic owner-permission repairs on Unix and unlocked stale-descriptor
removal; it never logs in, selects a provider/model, weakens permissions,
kills a process, or deletes conversations.

Use `xana config edit` for a manual edit through a bounded temporary copy. The
live file is replaced only after full schema validation and a concurrent-change
check, with the exact prior file retained as `config.toml.bak`. Invalid or
failed edits preserve both the live file and the draft for correction.

`xana reset --dry-run --scope SCOPE` previews `setup`, `sessions`, `caches`,
`credentials`, or `all`. Filesystem removal and referenced OS credentials have
separate confirmations; every scope preserves Codex-owned authentication and
conversations. The TUI exposes reset only as a guarded command-palette
lifecycle action. The legacy hidden `xana init` command remains a deprecated
compatibility path during the 0.5.x preview; `xana setup` is canonical.

## Add a remote API provider

Keys can be stored in the OS credential manager or referenced through one
named environment variable. Plaintext keys never belong in `config.toml`.

```bash
xana connection add openrouter --kind openrouter --model openai/gpt-4.1
xana connection set-key openrouter
xana model refresh openrouter
xana model use openrouter/openai/gpt-4.1
xana
```

Use `--kind openai` for the OpenAI API or `--kind anthropic` for Anthropic.
Anthropic is API-key-only; Xana does not offer Claude subscription OAuth.

## Use a ChatGPT subscription through Codex

On a fresh installation, install and log into a compatible Codex CLI, then
choose Codex in `xana setup`. Quick Setup probes the executable and app-server,
checks the Codex-owned account, fetches the live catalog, and refuses stale or
unadvertised model ids before writing configuration.

To add Codex to an existing Xana configuration instead:

```bash
xana connection add codex --kind codex --model ADVERTISED_MODEL_ID
xana connection status codex
xana connection login codex
xana model refresh codex
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
requests. The full-screen TUI uses `/activity view auto|hide|show` to persist an
automatic, pinned, or hidden activity pane. It shows correlated approval cards
even when hidden and labels Codex-owned work separately from Xana-native
children. The plain renderer keeps `/activity quiet|normal|verbose` for
append-only detail and `/details` for the last retained turn. `verbose` can show raw reasoning text only when Codex
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

During a native conversation, Xana exposes `spawn_agent`, atomic `spawn_many`,
`await_agent`, bounded `collect_agents`, `cancel_agent`, and the efficient `delegate_agent` composition
when at least one child route is configured. The model can give one task or a
fixed independent batch to exact routes (or the explicit default), while Xana
prints each child id, route, connection/model, lifecycle, activity, and
terminal status. Routes can mix Ollama, OpenAI-compatible, OpenAI API,
OpenRouter, Anthropic, and managed Codex routes in one batch. A native child gets
a fresh bounded prompt; a managed Codex child gets a fresh ephemeral Codex
thread. Both receive only the explicit task and selected bounded handoff data,
not the parent transcript, cannot delegate again, and return a bounded report
directly to the root turn. Native provider requests and managed Codex turn
usage retain exact child attribution. Batches reserve their complete budget
before becoming visible, run in input order up to the root profile's concurrency
limit, and fail atomically when any member or aggregate bound is invalid. Use `/agents`,
`/agent AGENT_ID`, and `/cancel-agent AGENT_ID` for active-process inspection
and cooperative cancellation; `xana session inspect SESSION_ID` is read-only
after restart. Typed summary/JSON reports overflow to immutable artifacts, and
multi-result collection preserves caller order, partial failures, and explicit
timeout/cancellation policy without loading artifact bodies. Closed versioned
plans validate fixed spawn/await/collect/cancel graphs before admission. See
[Child orchestration](docs/user/orchestration.md).

Managed Codex child routes support effective `ask` and `allow` modes. An
effective `deny` route is rejected because the current app-server contract
cannot prove that every Codex-owned inner tool effect is disabled. Child
activity is bounded before it reaches the root event stream, while permission
requests use a separate fail-closed control lane.

Descendant and aggregate tool/context/report/artifact budgets are cumulative
for the session; completed children release concurrency capacity but do not
replenish those totals. A cancellation request also does not overwrite the
owner's observed terminal outcome: completion can win the race, while a Codex
interrupt rejection remains a failed child with its remote error.

## Start first-run setup again

`xana reset` (alias: `xana clean`) previews the narrow setup state it will
remove and asks for confirmation. Use `--yes` for an explicit noninteractive
reset:

```bash
xana reset --yes
xana setup
```

From this source checkout, use `cargo run -- reset --yes` followed by `cargo
run -- setup`. Reset removes configuration, model selection/catalog caches, and
managed-thread handles. It preserves native sessions, artifacts, stored API
keys, Codex authentication, and Codex-owned conversations. `/clear` is
different: it clears only the current conversation.

## Chat, tools, and images

Bare `xana` starts the adaptive full-screen TUI when stdin and stdout are
interactive; redirected launches remain control-free plain output. Use
`xana --plain` for the permanent append-only interface or `xana --tui` to
require full-screen initialization. The native TUI has a bounded multiline
composer, confirmed paste, ordered follow-ups, exact interruption, a shared
slash-command/command palette, model picker, image staging, streamed turns,
bounded Markdown/code/diff rendering, explicit artifact actions, paged native
history, an expandable identity/status header, a one-to-six-row scrolling
composer, and adaptive wide/medium/narrow layouts. Shift+drag uses native
terminal text selection. An ordinary drag inside the conversation retains a
visible selection; Ctrl+C copies it and otherwise interrupts the active turn.
Other ordinary mouse events remain available for Xana's mouse-down clicks and
scrolling; queued drag motion samples the newest pointer position instead of
replaying stale coordinates. Ctrl+Q exits. Bracketed and detected key-stream
pastes are coalesced into one bounded confirmation, so pasted newlines do not
submit separate messages.
Managed Codex uses the same full-screen shell while Codex retains
ownership of its inner loop and history; Xana displays app-server activity and
routes exact approval decisions without a second model call. An implicit initialization
failure restores the terminal, warns, and falls back to plain, while explicit
`--tui` exits nonzero. See [Full-screen terminal UI](docs/user/tui.md).
Rich content, link safety, paging, and artifact actions are documented in
[Rich terminal content and artifacts](docs/user/rich-content.md).

Run `xana serve` to start an explicit loopback-only foreground host for the
canonical current workspace, then run `xana attach` from that workspace to
observe it. `xana attach --control` explicitly acquires the single controller
lease; `--prompt TEXT` submits one correlated turn and `--takeover` is the only
way to displace an existing controller. A dropped controller has a three-second
authenticated reconnect grace, after which pending approvals fail closed and
the active root is interrupted. Native and managed Codex execution use the
same observer/controller envelope. The capability is discovered through a
user-scoped runtime descriptor and never appears in argv, URLs, or shell
history. From a source checkout, use `cargo run -- serve` and `cargo run --
attach --control`. See [Local foreground host](docs/user/local-host.md).
Visible immutable artifacts can be retrieved as a verified 64 KiB preview with
`xana attach --artifact ARTIFACT_ID`; arbitrary paths are never accepted. The
host caps clients, frame rate, frame size, event queues, and socket-write time,
and owns a two-second graceful/five-second hard shutdown lifecycle.

Native
conversations use the latest compatible inactive session or create a new one;
`--continue` selects the latest
compatible conversation in the canonical workspace, while
`--resume SESSION_ID` selects one exact native session. `/clear` moves to a new
empty native history or a new Codex thread. In plain mode, `/quit`, Ctrl-C, and
EOF shut down the foreground runtime and print a compact native resume receipt.

`xana session list` shows the bounded native and managed conversation catalog
for the canonical current workspace, including active ownership. A single
OS-backed workspace gate permits only one root turn across local Xana
processes; another plain client may open a new inactive conversation for
drafting but cannot submit competing work. Exact resume fails with controlling
terminal/attach guidance. Xana does not lock the workspace filesystem: use
separate worktrees when parallel conversations might edit the same files.
`xana session new` starts an interactive fresh native session or managed Codex
thread with the current configuration. Retained Codex handles can be selected
with `xana session select CONNECTION THREAD_ID` and removed locally with
`xana session archive CONNECTION THREAD_ID`. Archiving never deletes
the vendor-owned thread.
In the full-screen client, `/sessions` opens the same bounded workspace catalog
for searchable read-only history navigation. Wide terminals also show a
session rail; `/sessions view show` or `/sessions view hide` persists only that
workspace-local layout preference, and hidden reserves no columns. Click the
panel title to hide it. `/sessions archive` removes the viewed inactive managed
handle, while `/sessions archive ID` selects an exact retained managed ID.
`/sessions new` restarts an idle frontend into a fresh native session or managed
Codex thread using the current resolved configuration while retaining the old
session. It refuses to compete with an active workspace root.
Viewing another transcript never transfers
the active root or submits its local draft.

Run exactly one turn with `xana -p "PROMPT"` or pipe one prompt into
`xana --print`. Final text is the only stdout payload; activity and diagnostics
use stderr. `--json` or `--output json` returns one versioned result envelope.
One-shot approvals fail closed instead of waiting for terminal input. From a
checkout, place CLI arguments after `--`, for example
`cargo run -- --json -p "summarize this repository"`. See
[Plain and one-shot modes](docs/user/automation.md) for pipelines, process
statuses, continuation, and the output contract.

For Codex, Xana retains a bounded catalog of opaque thread ids keyed by the
connection and canonical workspace. The next Xana process resumes the selected
Codex-owned thread on its first turn. It does not copy the conversation, tool
state, or credentials; `/clear` starts a new selected Codex thread while the
old opaque handle remains available for explicit selection.

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
a concise reference to `xana_docs`, a bounded durable root `AGENTS.md` view when
present, and exact explicitly activated Agent Skills from user, project, or
enabled-plugin scope. Xana does not discover `XANA.md` or nested `AGENTS.md`.
Inactive skill bodies/resources do not enter the prompt, and plugins remain
disabled until their separate lifecycle explicitly enables them.

Native tool intents and results are durably bracketed. Session resume performs
no automatic recovery; use `xana operation plan` and explicit `xana operation
resume` for eligible safe reads. See [Project context](docs/user/project-context.md),
[Permissions](docs/user/permissions.md), [Sessions](docs/user/sessions.md), and
[Operation recovery](docs/user/operations.md). Native child behavior and its
current limits are in [Child orchestration](docs/user/orchestration.md).

## Documentation and development

- [Documentation index](docs/README.md)
- [Architecture](docs/architecture/README.md)
- [Connections, models, and managed runtimes](docs/architecture/models-and-managed-runtimes.md)
- [Design principles](docs/principles.md)

Required checks:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --all-targets --no-default-features
```

## License

MIT - see [LICENSE](./LICENSE).
