![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust. Its name comes from Asturian folklore: a xana is a mysterious guide associated with water, forests, and hidden places. The project reinterprets that idea as a guide within the system, making complex paths understandable without becoming a source of hidden authority.

## Quick start from a source checkout

Run the first-time initializer, then start Xana:

```bash
cargo run -- init
cargo run
```

The initializer offers local Ollama or a custom unauthenticated OpenAI-compatible endpoint. It validates the resulting document and creates `config.toml` without replacing an existing file. Before writing, it explains that tools run automatically with the user's host permissions and asks for confirmation.

As an optional developer convenience, `cargo install --path . --locked` installs the checked-out source. It is not a Xana release or distribution channel.

## Configuration

Xana loads a strict, versioned `config.toml` at startup. A minimal local Ollama configuration is:

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

See [Configuration](docs/user/configuration.md) for initialization, diagnostics, platform paths, `XANA_HOME`, validation rules, and legacy `config.kv` migration.

## CLI and tools

Bare `xana` starts multi-turn terminal chat through a configured OpenAI-compatible endpoint. The configured model must support native tool calling to use Xana's tool path.

The command boundary also provides `xana init`, `xana config path`, and `xana config check`. Interactive startup and setup show Xana's terminal mark; `--no-banner` suppresses it, and `NO_COLOR` keeps a monochrome version.

Xana advertises three workspace tools:

- `read_file` reads a regular UTF-8 file or an inclusive one-based line range.
- `list_files` lists one directory non-recursively as sorted JSON, with limits of 256 entries and 64 KiB of output.
- `edit_file` replaces exactly one occurrence of text in an existing regular UTF-8 file.

Tool paths must be relative to Xana's launch directory and remain beneath it after resolution. Reads and resulting edits are capped at 64 KiB. The agent loop is bounded to eight model/tool rounds per turn.

Tools run automatically with the Xana process's ordinary host access. Path and resource checks, effect classification, and replay-safety metadata are not a permission system or sandbox. Xana does not prompt for per-tool approval, and `edit_file` does not claim atomic or crash-safe writes.

## Documentation

- [Configuration](docs/user/configuration.md) is the user reference.
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
