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

## Development

Install the stable Rust toolchain, then run:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## License

MIT - see [LICENSE](./LICENSE)