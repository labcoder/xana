![xana banner image](./assets/xana-clean.jpg)

# Xana

Xana is a small, extensible personal AI agent harness written in Rust.

The project is at an early stage. The first target is a focused terminal agent with explicit provider boundaries, tools, event-driven frontends, and cross-platform behavior.

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

Xana advertises one tool: `read_file`. When the model requests it, Xana reads
a regular UTF-8 file beneath the directory where the process started and
returns the contents to the same conversation. Paths must be relative, must
remain within that launch directory after resolution, and cannot name files
larger than 64 KiB.

These reads currently run automatically. Path validation limits what
`read_file` accepts, but it is not a permission system or a sandbox. Xana does
not yet prompt for approval, and the process retains its normal host access.

## Development

Install the stable Rust toolchain, then run:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## License

MIT - see [LICENSE](./LICENSE)
