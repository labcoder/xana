![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust.

The project is at an early stage. The first target is a focused terminal agent with explicit provider boundaries, tools, event-driven frontends, and cross-platform behavior.

## Configuration

Xana loads a strict, versioned `config.toml` at startup. A minimal local Ollama
configuration is:

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

See [Configuration](docs/configuration.md) for platform paths, `XANA_HOME`,
validation rules, and migration from the temporary `config.kv` format.

## Design direction

- Keep the core small and headless.
- Treat providers as adapters around one internal conversation model.
- Make tools and events the extension seams.
- Keep configuration and limits per agent.
- Build and test on Linux, macOS, and Windows.

## Current CLI

Xana currently provides multi-turn terminal chat through an
OpenAI-compatible local endpoint. The configured model must support native
tool calling to use Xana's tool path.

Xana advertises three workspace tools through a trait-based registry:

- `read_file` reads a regular UTF-8 file, or an inclusive one-based line range.
- `list_files` lists one directory non-recursively as sorted JSON, with limits
  of 256 entries and 64 KiB of output.
- `edit_file` replaces exactly one occurrence of text in an existing regular
  UTF-8 file.

All paths must be relative to the directory where Xana started and must remain
beneath that directory after resolution. Reads and resulting edits are capped
at 64 KiB. The agent loop is also bounded to eight model/tool rounds per turn.

These tools currently run automatically. Their path and resource validation,
effect class, and replay-safety metadata are not a permission system or a
sandbox. Xana does not yet prompt for approval, and the process retains its
normal host access. `edit_file` also does not claim atomic or crash-safe writes.

## Development

Install the stable Rust toolchain, then run:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## License

MIT - see [LICENSE](./LICENSE)
