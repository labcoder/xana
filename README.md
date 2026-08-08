![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust. Its name comes from Asturian folklore: a xana is a mysterious guide associated with water, forests, and hidden places. The project reinterprets that idea as a guide within the system, making complex paths understandable without becoming a source of hidden authority.

## Install from source

Xana's developer preview is built locally with the pinned Rust toolchain. From
a checkout:

```bash
git clone https://github.com/labcoder/xana.git
cd xana
cargo install --path crates/xana-cli --locked
xana --version
xana init
xana config check
xana
```

Install directly from the Git repository with:

```bash
cargo install --git https://github.com/labcoder/xana.git --locked
```

Use `--rev COMMIT_SHA` for a reviewed, repeatable Git build. Xana is not
published to crates.io and does not provide prebuilt binaries, an installer,
or automatic updates. See [Source installation](docs/user/installation.md) for
Rust and shell prerequisites, platform-correct `XANA_HOME` examples, updates,
and uninstall instructions.

The initializer offers local Ollama or a custom unauthenticated OpenAI-compatible endpoint, asks which shell `run_command` should use, and selects `deny`, `ask`, or `allow` permission behavior with `ask` as the human default. It validates the resulting document and creates `config.toml` without replacing an existing file. Before writing, it explains that tools use the user's host permissions and asks for confirmation.

The workspace also contains the focused `xana-core` contracts and the
`xana-cli` package. Provider adapters keep OpenRouter and Anthropic wire
formats private; their credentials are injected by the runtime edge.

## Configuration

Xana loads a strict, versioned `config.toml` at startup. A minimal local Ollama configuration is:

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

See [Configuration](docs/user/configuration.md) for initialization, diagnostics, platform paths, `XANA_HOME`, validation rules, and legacy `config.kv` migration.

## CLI and tools

Bare `xana` creates a durable session and starts multi-turn terminal chat through a configured OpenAI-compatible endpoint. Xana prints the session id and JSONL path. Resume that exact history with `xana --resume SESSION_ID`; Xana never guesses the latest session or replays unfinished work. `/clear` commits an empty thread head without deleting earlier entries, while `/quit`, Ctrl-C, and EOF shut down the foreground runtime. The configured model must support native tool calling to use Xana's tool path.

For each accepted root turn Xana freezes one `xana-prompt-v1` snapshot: its built-in identity and operating guidelines, a summary of the tools actually registered for that agent, owned runtime and CLI context, and a bounded materialization of a durable root `AGENTS.md` version when present. The snapshot stays fixed across that turn's tool rounds; changed project instructions become a new version on the next root turn. Xana does not discover `XANA.md`, nested `AGENTS.md`, skills, or plugins yet. See [Project context and system prompt](docs/user/project-context.md).

The command boundary also provides `xana init`, `xana config path`, `xana config check`, and the read-only `xana session inspect SESSION_ID`. Interrupted effects have a separate read-only `xana operation plan --session SESSION_ID OPERATION_ID` command and explicit `xana operation resume --session SESSION_ID OPERATION_ID` reconciliation command. Opening or resuming chat never triggers recovery. Interactive startup and setup show Xana's terminal mark; `--no-banner` suppresses it, and `NO_COLOR` keeps a monochrome version.

Xana advertises four workspace tools:

- `read_file` reads a regular UTF-8 file or an inclusive one-based line range.
- `list_files` lists one directory non-recursively as sorted JSON, with limits of 256 entries and 64 KiB of output.
- `edit_file` replaces exactly one occurrence of text in an existing regular UTF-8 file.
- `run_command` runs one command through the configured shell from an existing workspace directory. Its permission scope binds the selected shell, exact command, and canonical working directory. Stdout and stderr are each limited to 32 KiB.

Tool paths must be relative to Xana's launch directory and remain beneath it after resolution. Reads and resulting edits are capped at 64 KiB. The agent loop is bounded to eight model/tool rounds per turn.

Every built-in tool crosses one runtime-owned permission broker. `permission_mode` sets the default to `deny`, `ask`, or `allow`; matching rules use deny-before-ask-before-allow precedence. An ask can be denied, allowed once, or allowed for the exact current-session scope. Decisions bind the final arguments and canonical scope to the active operation and tool invocation, and losing the controlling terminal fails closed. See [Permissions](docs/user/permissions.md).

Permission is not containment. Allowed tools use the Xana process's ordinary host access, and policy, path checks, effect classification, and replay-safety metadata are not OS-level isolation. Permission audit facts are durable session records, but grants remain memory-only; `edit_file` does not claim atomic or crash-safe writes.

Tool effects are bracketed by durable intent and result records. If Xana stops
after intent but before result, the outcome is unknown. Only `read_file` and
`list_files` are eligible for an explicit, currently authorized replay;
`edit_file` and `run_command` are interrupted without repetition. Recovery may
prompt again and may require manual reconciliation. It provides process-crash
record boundaries, not power-loss durability, automatic retry, idempotency, or
containment. See [Operation recovery](docs/user/operations.md).

## Documentation

- [Source installation](docs/user/installation.md) explains supported Cargo
  installation routes, prerequisites, platform homes, updates, and uninstall.
- [Configuration](docs/user/configuration.md) is the user reference.
- [Project context and system prompt](docs/user/project-context.md) explains root `AGENTS.md`, prompt layers, budgets, and trust boundaries.
- [Permissions](docs/user/permissions.md) explains policy precedence, scopes, controller decisions, and host-access limits.
- [Sessions](docs/user/sessions.md) explains durable history, explicit resume, artifact paths, inspection, corruption, and backup limits.
- [Operation recovery](docs/user/operations.md) explains crash-safe intent,
  read-only plans, explicit replay, and interruption behavior.
- [Documentation index](docs/README.md) separates user and engineering material and explains its authority model.
- [Architecture](docs/architecture/README.md) describes what Xana is and how the implemented system works.
- [Design Principles](docs/principles.md) defines durable constraints for future changes.

## Development

Install the stable Rust toolchain, then run:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

See [Code organization](docs/development/code-organization.md) for repository policy.

## License

MIT — see [LICENSE](./LICENSE).
