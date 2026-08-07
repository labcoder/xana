![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust. Its name comes from Asturian folklore: a xana is a mysterious guide associated with water, forests, and hidden places. The project reinterprets that idea as a guide within the system, making complex paths understandable without becoming a source of hidden authority.

## Quick start from a source checkout

Run the first-time initializer, then start Xana:

```bash
cargo run -- init
cargo run
```

The initializer offers local Ollama or a custom unauthenticated OpenAI-compatible endpoint, asks which shell `run_command` should use, and selects `deny`, `ask`, or `allow` permission behavior with `ask` as the human default. It validates the resulting document and creates `config.toml` without replacing an existing file. Before writing, it explains that tools use the user's host permissions and asks for confirmation.

As an optional developer convenience, `cargo install --path . --locked` installs the checked-out source. It is not a Xana release or distribution channel.

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

Bare `xana` starts multi-turn terminal chat through a configured OpenAI-compatible endpoint. Assistant text is rendered as streaming deltas while each turn runs. `/clear` resets the foreground runtime's transient conversation, while `/quit`, Ctrl-C, and EOF shut it down. The configured model must support native tool calling to use Xana's tool path.

At startup Xana freezes `xana-prompt-v1`: its built-in identity and operating guidelines, a summary of the tools actually registered for that agent, owned runtime and CLI context, and a bounded preview of root `AGENTS.md` when present. The same system message is sent on every model request. Xana does not discover `XANA.md`, nested `AGENTS.md`, skills, or plugins yet. See [Project context and system prompt](docs/user/project-context.md).

The command boundary also provides `xana init`, `xana config path`, and `xana config check`. Interactive startup and setup show Xana's terminal mark; `--no-banner` suppresses it, and `NO_COLOR` keeps a monochrome version.

Xana advertises four workspace tools:

- `read_file` reads a regular UTF-8 file or an inclusive one-based line range.
- `list_files` lists one directory non-recursively as sorted JSON, with limits of 256 entries and 64 KiB of output.
- `edit_file` replaces exactly one occurrence of text in an existing regular UTF-8 file.
- `run_command` runs one command through the configured shell from an existing workspace directory. Its permission scope binds the selected shell, exact command, and canonical working directory. Stdout and stderr are each limited to 32 KiB.

Tool paths must be relative to Xana's launch directory and remain beneath it after resolution. Reads and resulting edits are capped at 64 KiB. The agent loop is bounded to eight model/tool rounds per turn.

Every built-in tool crosses one runtime-owned permission broker. `permission_mode` sets the default to `deny`, `ask`, or `allow`; matching rules use deny-before-ask-before-allow precedence. An ask can be denied, allowed once, or allowed for the exact current-session scope. Decisions bind the final arguments and canonical scope to the active operation and tool invocation, and losing the controlling terminal fails closed. See [Permissions](docs/user/permissions.md).

Permission is not containment. Allowed tools use the Xana process's ordinary host access, and policy, path checks, effect classification, and replay-safety metadata are not OS-level isolation. Session grants are memory-only, audit facts are not durable yet, and `edit_file` does not claim atomic or crash-safe writes.

## Documentation

- [Configuration](docs/user/configuration.md) is the user reference.
- [Project context and system prompt](docs/user/project-context.md) explains root `AGENTS.md`, prompt layers, budgets, and trust boundaries.
- [Permissions](docs/user/permissions.md) explains policy precedence, scopes, controller decisions, and host-access limits.
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
