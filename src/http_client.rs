//! Xana-owned HTTP client construction and TLS provider selection.

pub(crate) fn builder() -> reqwest::ClientBuilder {
    install_crypto_provider();
    reqwest::Client::builder()
}

#[cfg(test)]
pub(crate) fn client() -> reqwest::Client {
    install_crypto_provider();
    reqwest::Client::new()
}

fn install_crypto_provider() {
    // Another embedding application may have installed a process-wide provider first.
    let _ = rustls::crypto::ring::default_provider().install_default();
}
