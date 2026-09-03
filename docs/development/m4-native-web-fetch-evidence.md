# M4 native web-fetch evidence

> Audience: Contributors and coding agents
> Authority: Descriptive

This record validates the M4-03C bounded native web-fetch implementation in
Xana commit `e56db86` and its user/architecture contract in `551132a`. It uses
only deterministic loopback fixtures; no test depends on the public Internet,
paid credentials, or a mutable third-party page.

## Contract exercised

The stock native capability snapshot now exposes `network.fetch` through the
typed `web_fetch` tool. The focused tests prove:

- exact canonical URL-chain binding through both permission and outbound-data
  review, with durable deny precedence and audit-before-transport semantics
  inherited from the shared `OutboundGuard`;
- public HTTPS-only production admission, no ambient proxy or credentials,
  address resolution and pinning for every connection attempt, and rejection
  of private/special-use IPv4, IPv6, and IPv4-mapped addresses;
- stop-before-contact behavior for an unreviewed redirect, exact ordered retry,
  and failure on redirect mismatch, cycles, limits, or HTTPS downgrade;
- separate bounds for request URLs, redirects, headers, encoded response bytes,
  whole-request time, HTML extraction time, extracted text, and inline text;
- rejection of compressed, unsupported-MIME, unsupported-charset, and malformed
  UTF-8 responses; hostile HTML scripts and styles never enter extracted text;
- cancellation and timeout of a slow peer, and zero connection attempts when
  durable outbound policy/audit state cannot be opened; and
- complete bounded source preservation in the immutable content-addressed
  artifact store whenever the inline result cannot carry the source.

The result is structurally attributed with requested/final URLs, fetch time,
MIME, response bytes, BLAKE3 digest, redirect provenance, truncation,
`fresh_not_cached`, and `untrusted = true`. It provides no browser, JavaScript,
cookies, authentication, search, upload, cache, or executable-download
authority.

## Resource observations

Measured on Windows x86-64 on 2026-09-01 with the repository-pinned Rust
toolchain and lockfile:

| Probe | Observation |
|---|---|
| Complete prompt baseline | 6,258 system bytes / 2,085 estimated tokens |
| Ten provider tool schemas | 8,013 bytes / 2,674 estimated tokens |
| Default/maximum response | 1 MiB / 4 MiB encoded bytes |
| Response headers | 32 KiB maximum |
| Extracted/inline text | 256 KiB / 24 KiB maximum |
| Redirects | Three exact reviewed destinations maximum |
| Default/maximum request time | 20 / 60 seconds |
| HTML extraction time | Two seconds on Tokio's blocking pool |

The HTML parser receives at most four MiB, runs outside the async executor, and
cannot load CSS, JavaScript, or subresources. The response collector rejects a
declared or streamed overflow before appending beyond the reviewed byte bound.
Returned inline text is bounded before it reaches provider history, session
projection, or frontend memory.

## Verification

The required local gate passed:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo doc --locked --no-deps --all-features
```

The full test run passed 887 library tests with 6 intentionally ignored manual
probes, 20 CLI integration tests, and 4 settings integration tests. The focused
web-fetch suite passed 11 tests. The shared outbound, permission, operation,
diagnostics, protocol, and frontend projection suites ran in the same full
gate. Windows passed locally; Linux/macOS compilation and tests await the next
authorized CI push and are not claimed here.
