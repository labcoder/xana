# AGENTS.md

## Project

Xana is a small, extensible personal AI agent harness written in Rust. Keep the implementation cross-platform and keep the core independent of any frontend or provider wire format.

## Required checks

Run these before submitting changes:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Architecture rules

1. Wire formats never leave their provider module. The rest of Xana uses its internal conversation types.
2. The agent loop is a value, not `main()`. `main` constructs agents and subscribes to their events.
3. The core is headless. It emits events and accepts commands; it does not render a CLI or GUI.
4. Configuration is per agent and passed in. Core code does not read global configuration or environment variables.
5. Sessions record internal types and agent lineage.
6. Keep Phase 1 blocking; make the async boundary explicit when streaming is introduced.
7. Make no platform assumptions. Use Rust path APIs and explicit shell abstractions, and keep Linux, macOS, and Windows CI green.
8. Keep core small. Prefer tools, events, and context hooks as extension seams.
9. Treat context as a budget, not a bucket. Every prompt input must pass through an explicit token budget.
10. Keep optional capability lifecycle explicit. Discovery is pure and
    read-only; installation and enablement are control-plane operations outside
    agent turns; each agent receives an immutable, truthful tool snapshot.
    Lazy activation may initialize an already-installed expensive provider, but
    must not install one.
11. Treat structured documents as untrusted parser input. Resolve and authorize
    one regular file, read it once through a bound, prefer content/container
    identity over extensions, enforce layered resource/output/context limits,
    preserve typed failures, and never auto-fetch or execute extracted content.
