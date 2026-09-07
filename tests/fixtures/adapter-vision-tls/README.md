# Test-only HTTPS fixture

These base64-encoded DER files contain a disposable fixture CA certificate,
server certificate and **public test-only PKCS#8 private key**. They grant no
access to any service and must never be used outside tests. The leaf names only
`localhost` and `127.0.0.1`. The leaf is RSA-2048/SHA-256, has explicit
`serverAuth` extended key usage and digital-signature key usage, and is valid
for 397 days. The CA is valid for ten years. A century-long leaf without EKU
is not a portable test fixture: Apple's native verifier rejects it even when
the caller explicitly trusts its CA.

The adapter fixture starts its own loopback Rustls server and passes this CA
through the explicit exact-origin `DesktopLaunch` trust option. It never edits
the operating system trust store, disables TLS verification, or changes global
HTTP clients. Other origins and the native conversational provider retain their
normal trust behavior.

## Check and renew

From the repository root, PowerShell 7 can check the fixture without changing
anything:

```text
pwsh -NoProfile -File tests/fixtures/adapter-vision-tls/certificates.ps1
```

CI checks its usage and lifetime and gives a renewal error fourteen days before
expiry. Run the same command with `-Renew` to regenerate **all three** fixture
files using in-memory .NET keys, then review and commit those files together.
Renewal has no OpenSSL CLI dependency and does not install trust or contact any
service. Run `cargo test --locked --test adapter_vision` and the native CI lanes
after renewal; checking metadata alone is not a TLS handshake test.

See [Apple's TLS certificate requirements](https://support.apple.com/en-gb/103769).
