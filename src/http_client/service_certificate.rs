//! Explicit extra trust for one already-configured specialist HTTPS origin.

#[derive(Clone)]
pub(crate) struct ScopedServiceCertificate {
    origin: String,
    certificate: reqwest::Certificate,
}

impl std::fmt::Debug for ScopedServiceCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedServiceCertificate")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl ScopedServiceCertificate {
    pub(crate) fn new(origin: &str, der: Vec<u8>) -> Result<Self, &'static str> {
        if origin.len() > 2048 || der.is_empty() || der.len() > 64 * 1024 {
            return Err("service certificate exceeds bounded origin or certificate limits");
        }
        let url = reqwest::Url::parse(origin).map_err(|_| "invalid service certificate origin")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("service certificate requires an exact HTTPS origin without a path");
        }
        // Parse the certificate now, not after admission or credential lookup.
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(der.clone()))
            .map_err(|_| "invalid service certificate DER")?;
        let certificate =
            reqwest::Certificate::from_der(&der).map_err(|_| "invalid service certificate DER")?;
        Ok(Self {
            origin: url.origin().ascii_serialization(),
            certificate,
        })
    }

    pub(crate) fn matches(&self, endpoint: &str) -> bool {
        reqwest::Url::parse(endpoint).is_ok_and(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.origin().ascii_serialization() == self.origin
        })
    }

    pub(crate) fn apply(
        &self,
        endpoint: &str,
        builder: reqwest::ClientBuilder,
    ) -> reqwest::ClientBuilder {
        if self.matches(endpoint) {
            builder.tls_certs_merge([self.certificate.clone()])
        } else {
            builder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn certificate() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(include_str!("../../tests/fixtures/adapter-vision-tls/test-ca.der.b64").trim())
            .unwrap()
    }

    #[test]
    fn trust_is_exact_origin_bounded_and_never_a_tls_bypass() {
        let trust = ScopedServiceCertificate::new("https://localhost:8443", certificate()).unwrap();
        assert!(trust.matches("https://localhost:8443/v1/chat/completions"));
        for endpoint in [
            "https://localhost:8444/v1",
            "https://127.0.0.1:8443/v1",
            "http://localhost:8443/v1",
            "https://secret@localhost:8443/v1",
        ] {
            assert!(!trust.matches(endpoint));
        }
        for origin in [
            "http://localhost:8443",
            "https://localhost:8443/v1",
            "https://secret@localhost:8443",
            "https://localhost:8443/?secret",
            "https://localhost:8443/#fragment",
        ] {
            assert!(ScopedServiceCertificate::new(origin, certificate()).is_err());
        }
        assert!(ScopedServiceCertificate::new("https://localhost", vec![0; 65 * 1024]).is_err());
        assert!(ScopedServiceCertificate::new("https://localhost", vec![0; 12]).is_err());
        assert!(!format!("{trust:?}").contains("BEGIN CERTIFICATE"));
    }
}
