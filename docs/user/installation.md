# Source installation

> Audience: People installing, updating, or removing Xana from source.

Xana's developer preview is distributed as Rust source. It has no prebuilt
binary archive, platform installer, package-manager channel, automatic updater,
or crates.io release. Source compilation happens on the local machine.

## Prerequisites

Install Git and Rust through [rustup](https://rustup.rs/). Xana pins Rust
`1.97.1` in `rust-toolchain.toml`; Cargo, rustfmt, and Clippy use that toolchain
inside a checkout. Platform-default shell execution also requires `sh` on
macOS/Linux or PowerShell on Windows. Git Bash and `cmd` are explicit Windows
alternatives.

ChatGPT subscription access additionally requires a compatible `codex`
executable on `PATH` (or an explicit connection `codex_program`). API-key and
Ollama connections do not require Codex.

## Install from a checkout

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo install --path crates/xana-cli --locked
```

`--locked` uses Xana's checked-in application dependency graph. Repeating the
command rebuilds and replaces the installed Cargo binary when the source
version changes.

## Install from Git

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
```

That command follows the repository's current default branch. For a reviewed,
repeatable build, add `--rev COMMIT_SHA`. No Phase 2 release tag is claimed
until one actually exists.

Confirm which binary is on the path:

```bash
xana --version
xana --help
```

For a repeatable isolated smoke test, use the repository helper with a
temporary or dedicated install prefix:

```bash
scripts/run-isolated.sh /tmp/xana-install --version
scripts/run-isolated.sh /tmp/xana-install init \
  --non-interactive \
  --kind ollama \
  --provider-name ollama \
  --base-url http://localhost:11434/v1 \
  --model qwen3:1.7b \
  --permission-mode ask
```

The helper installs from the checkout with `--locked`, runs the installed
binary from an empty temporary workspace, and supplies a temporary
`XANA_HOME`. Its temporary configuration, sessions, artifacts, and workspace
are removed when the command exits. The install prefix is retained so a later
invocation can rebuild it; it is safe to remove that prefix when finished.

## Choose Xana's home

An unset `XANA_HOME` uses platform-standard directories. When set, it must be
a nonempty native absolute path; Xana does not expand `~` and Windows Rust does
not treat Git Bash's `/c/...` spelling as absolute.

macOS or Linux:

```bash
export XANA_HOME="$HOME/.xana"
```

Windows PowerShell:

```powershell
$env:XANA_HOME = 'C:\Users\you\.xana'
```

Windows Git Bash:

```bash
export XANA_HOME="$(cygpath -m "$HOME/.xana")"
```

The shell expands `$HOME` before Xana starts. On Git Bash, `cygpath` converts
the shell path to a Windows absolute path; do not pass `/c/...` or a literal
`~/.xana` to the Windows executable.

## Initialize and verify

Interactive setup:

```bash
xana init
xana config check
xana
```

For a disposable noninteractive local Ollama setup:

```bash
xana init --non-interactive \
  --kind ollama \
  --provider-name ollama \
  --base-url http://localhost:11434/v1 \
  --model qwen3:1.7b \
  --shell platform \
  --permission-mode ask
xana config check
```

Interactive setup can instead create a managed Codex connection for a
ChatGPT subscription. Install the Codex CLI, choose that option, then follow
the printed status, login, model refresh, model list, and model use commands.
Codex model IDs depend on the installed runtime and account, so the initial ID
is provisional until that live check. Xana delegates OAuth and credential
storage to Codex; it does not need a hosted callback server.

When developing from this checkout, pass Xana arguments after Cargo's `--`:

```bash
cargo run -- connection status codex
cargo run -- connection login codex
cargo run -- connection refresh codex
cargo run -- model list --connection codex
cargo run -- model use codex/ADVERTISED_MODEL_ID
cargo run
```

Alternatively, `cargo install --path crates/xana-cli --locked` installs the
`xana` command used throughout the documentation. `cargo init` is Cargo's
command for creating a new Rust package; it does not initialize Xana.

To return only Xana's setup state to first run while preserving sessions,
artifacts, stored credentials, and external Codex state:

```bash
cargo run -- reset
cargo run -- init
```

Use `cargo run -- reset --yes` when no interactive confirmation is possible.

See [Configuration](configuration.md) for paths and schema,
[Permissions](permissions.md) for host authority, [Project context and system
prompt](project-context.md) for `AGENTS.md`, [Sessions](sessions.md) for durable
history, and [Operation recovery](operations.md) for interrupted effects.

## Update or remove

Pull or select a reviewed Git revision, then repeat the relevant `cargo
install ... --locked` command. This source channel does not update itself.

Remove the Cargo-installed binary with:

```bash
cargo uninstall xana-cli
```

Uninstalling the binary does not delete Xana's configuration, sessions, or
artifacts. Report platform or installation failures at the repository's
[issue tracker](https://github.com/labcoder/xana/issues).
