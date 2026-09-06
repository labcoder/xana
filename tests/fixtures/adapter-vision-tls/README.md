# Test-only HTTPS fixture

These base64-encoded DER files contain a disposable fixture CA certificate,
server certificate and **public test-only PKCS#8 private key**. They grant no
access to any service and must never be used outside tests. The leaf names only
`localhost` and `127.0.0.1`; certificates expire in 2126.

The adapter fixture starts its own loopback Rustls server and passes this CA
through the explicit exact-origin `DesktopLaunch` trust option. It never edits
the operating system trust store, disables TLS verification, or changes global
HTTP clients. Other origins and the native conversational provider retain their
normal trust behavior.
