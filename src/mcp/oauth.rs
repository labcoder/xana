//! MCP OAuth 2.1 discovery, local PKCE completion, and secure token rotation.

use super::http::{McpHttpError, McpHttpSecurity, pinned_client};
use crate::credential::{CredentialError, SecretStore, SecretString};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use futures::StreamExt;
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs,
    future::Future,
    io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAX_CHALLENGE_BYTES: usize = 8192;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_SCOPES: usize = 64;
const MAX_AUTHORIZATION_SERVERS: usize = 8;
const MAX_SCOPE_BYTES: usize = 256;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const REFRESH_SKEW_SECONDS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpAuthChallenge {
    pub(crate) resource_metadata: Url,
    pub(crate) scopes: BTreeSet<String>,
    pub(crate) insufficient_scope: bool,
}

impl McpAuthChallenge {
    pub(crate) fn parse(value: &str) -> Result<Self, McpOAuthError> {
        if value.len() > MAX_CHALLENGE_BYTES {
            return Err(McpOAuthError::Challenge);
        }
        let value = value.trim();
        let parameters = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .ok_or(McpOAuthError::Challenge)?;
        let mut fields = BTreeMap::new();
        for field in split_challenge_fields(parameters)? {
            let (name, raw) = field.split_once('=').ok_or(McpOAuthError::Challenge)?;
            let name = name.trim().to_ascii_lowercase();
            let raw = raw.trim();
            let value = raw
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(raw)
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
            if fields.insert(name, value).is_some() {
                return Err(McpOAuthError::Challenge);
            }
        }
        let resource_metadata = fields
            .get("resource_metadata")
            .ok_or(McpOAuthError::Challenge)
            .and_then(|value| Url::parse(value).map_err(|_| McpOAuthError::Challenge))?;
        if resource_metadata.scheme() != "https"
            || resource_metadata.username() != ""
            || resource_metadata.password().is_some()
            || resource_metadata.fragment().is_some()
        {
            return Err(McpOAuthError::Challenge);
        }
        let scopes = parse_scopes(fields.get("scope").map(String::as_str).unwrap_or(""))?;
        Ok(Self {
            resource_metadata,
            scopes,
            insufficient_scope: fields
                .get("error")
                .is_some_and(|value| value == "insufficient_scope"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpOAuthMetadata {
    pub(crate) issuer: Url,
    pub(crate) authorization_endpoint: Url,
    pub(crate) token_endpoint: Url,
    pub(crate) scopes_supported: BTreeSet<String>,
    pub(crate) code_challenge_s256: bool,
    pub(crate) authorization_response_iss_parameter_supported: bool,
}

impl McpOAuthMetadata {
    pub(crate) fn parse(bytes: &[u8], expected_issuer: &Url) -> Result<Self, McpOAuthError> {
        Self::parse_with_security(bytes, expected_issuer, McpHttpSecurity::default())
    }

    fn parse_with_security(
        bytes: &[u8],
        expected_issuer: &Url,
        security: McpHttpSecurity,
    ) -> Result<Self, McpOAuthError> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(McpOAuthError::MetadataTooLarge);
        }
        let value: ValueMap = serde_json::from_slice(bytes).map_err(|_| McpOAuthError::Metadata)?;
        let issuer = parse_exact_url(value.issuer.as_deref().ok_or(McpOAuthError::Metadata)?)?;
        if issuer.as_str() != expected_issuer.as_str() {
            return Err(McpOAuthError::IssuerMismatch);
        }
        let authorization_endpoint = parse_exact_url(
            value
                .authorization_endpoint
                .as_deref()
                .ok_or(McpOAuthError::Metadata)?,
        )?;
        let token_endpoint = parse_exact_url(
            value
                .token_endpoint
                .as_deref()
                .ok_or(McpOAuthError::Metadata)?,
        )?;
        for endpoint in [&issuer, &authorization_endpoint, &token_endpoint] {
            if !permitted_metadata_url(endpoint, security) {
                return Err(McpOAuthError::Metadata);
            }
        }
        let code_challenge_s256 = value
            .code_challenge_methods_supported
            .unwrap_or_default()
            .iter()
            .any(|method| method == "S256");
        if !code_challenge_s256 {
            return Err(McpOAuthError::PkceUnsupported);
        }
        let scopes_supported = parse_scope_array(value.scopes_supported.unwrap_or_default())?;
        Ok(Self {
            issuer,
            authorization_endpoint,
            token_endpoint,
            scopes_supported,
            code_challenge_s256,
            authorization_response_iss_parameter_supported: value
                .authorization_response_iss_parameter_supported
                .unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpProtectedResourceMetadata {
    pub(crate) resource: Url,
    pub(crate) authorization_servers: Vec<Url>,
    pub(crate) scopes_supported: BTreeSet<String>,
}

impl McpProtectedResourceMetadata {
    fn parse(
        bytes: &[u8],
        expected_resource: &Url,
        security: McpHttpSecurity,
    ) -> Result<Self, McpOAuthError> {
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(McpOAuthError::MetadataTooLarge);
        }
        let value: ProtectedResourceWire =
            serde_json::from_slice(bytes).map_err(|_| McpOAuthError::Metadata)?;
        let resource = parse_exact_url(value.resource.as_deref().ok_or(McpOAuthError::Metadata)?)?;
        if resource.as_str() != expected_resource.as_str()
            || !permitted_metadata_url(&resource, security)
        {
            return Err(McpOAuthError::ResourceMismatch);
        }
        let servers = value.authorization_servers.unwrap_or_default();
        if servers.is_empty() || servers.len() > MAX_AUTHORIZATION_SERVERS {
            return Err(McpOAuthError::UnsupportedIssuer);
        }
        let mut authorization_servers = Vec::with_capacity(servers.len());
        for server in servers {
            let server = parse_exact_url(&server)?;
            if !permitted_metadata_url(&server, security) || authorization_servers.contains(&server)
            {
                return Err(McpOAuthError::Metadata);
            }
            authorization_servers.push(server);
        }
        Ok(Self {
            resource,
            authorization_servers,
            scopes_supported: parse_scope_array(value.scopes_supported.unwrap_or_default())?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceWire {
    resource: Option<String>,
    authorization_servers: Option<Vec<String>>,
    scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpOAuthDiscovery {
    pub(crate) protected_resource: McpProtectedResourceMetadata,
    pub(crate) authorization_server: McpOAuthMetadata,
}

#[derive(Debug, Deserialize)]
struct ValueMap {
    issuer: Option<String>,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    scopes_supported: Option<Vec<String>>,
    code_challenge_methods_supported: Option<Vec<String>>,
    authorization_response_iss_parameter_supported: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpOAuthReference {
    pub(crate) credential_id: String,
    pub(crate) issuer: String,
    pub(crate) client_id: String,
    pub(crate) resource: String,
    pub(crate) scopes: BTreeSet<String>,
}

impl McpOAuthReference {
    pub(crate) fn validate(&self) -> Result<(), McpOAuthError> {
        validate_id(&self.credential_id)?;
        validate_id(&self.client_id)?;
        let issuer = parse_exact_url(&self.issuer)?;
        let resource = parse_exact_url(&self.resource)?;
        if issuer.scheme() != "https"
            || resource.scheme() != "https"
            || resource.fragment().is_some()
        {
            return Err(McpOAuthError::Reference);
        }
        validate_scopes(&self.scopes)
    }
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpOAuthToken {
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    expires_at_unix: u64,
    #[zeroize(skip)]
    scopes: BTreeSet<String>,
    issuer: String,
    client_id: String,
    resource: String,
}

impl fmt::Debug for McpOAuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthToken")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("token_type", &self.token_type)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("scopes", &self.scopes)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("resource", &self.resource)
            .finish()
    }
}

impl McpOAuthToken {
    fn validate_for(&self, reference: &McpOAuthReference) -> Result<(), McpOAuthError> {
        if self.token_type != "Bearer"
            || self.access_token.trim().is_empty()
            || self.issuer != reference.issuer
            || self.client_id != reference.client_id
            || self.resource != reference.resource
            || !reference.scopes.is_subset(&self.scopes)
        {
            return Err(McpOAuthError::TokenBinding);
        }
        Ok(())
    }

    fn access_secret(&self) -> Result<SecretString, McpOAuthError> {
        SecretString::new(self.access_token.clone()).map_err(|_| McpOAuthError::Token)
    }

    fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at_unix <= now.saturating_add(REFRESH_SKEW_SECONDS)
    }
}

pub(crate) struct McpOAuthStore<S> {
    store: S,
    lock_path: PathBuf,
    local: Mutex<()>,
}

impl<S: SecretStore> McpOAuthStore<S> {
    pub(crate) fn new(store: S, lock_path: impl Into<PathBuf>) -> Self {
        Self {
            store,
            lock_path: lock_path.into(),
            local: Mutex::new(()),
        }
    }

    pub(crate) fn save(
        &self,
        reference: &McpOAuthReference,
        token: &McpOAuthToken,
    ) -> Result<(), McpOAuthError> {
        reference.validate()?;
        token.validate_for(reference)?;
        let mut encoded = serde_json::to_string(token).map_err(|_| McpOAuthError::Token)?;
        if encoded.len() > MAX_TOKEN_BYTES {
            encoded.zeroize();
            return Err(McpOAuthError::Token);
        }
        let result = self
            .store
            .set(&reference.credential_id, &encoded)
            .map_err(map_store_error);
        encoded.zeroize();
        result
    }

    pub(crate) fn load(
        &self,
        reference: &McpOAuthReference,
    ) -> Result<McpOAuthToken, McpOAuthError> {
        reference.validate()?;
        let mut encoded = self
            .store
            .get(&reference.credential_id)
            .map_err(map_store_error)?
            .ok_or(McpOAuthError::CredentialMissing)?;
        if encoded.len() > MAX_TOKEN_BYTES {
            encoded.zeroize();
            return Err(McpOAuthError::Token);
        }
        let parsed = serde_json::from_str(&encoded).map_err(|_| McpOAuthError::Token);
        encoded.zeroize();
        let token: McpOAuthToken = parsed?;
        token.validate_for(reference)?;
        Ok(token)
    }

    pub(crate) fn logout(&self, reference: &McpOAuthReference) -> Result<bool, McpOAuthError> {
        self.store
            .delete(&reference.credential_id)
            .map_err(map_store_error)
    }

    pub(crate) async fn access_or_refresh<F, Fut>(
        &self,
        reference: &McpOAuthReference,
        now_unix: u64,
        refresh: F,
    ) -> Result<SecretString, McpOAuthError>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<McpOAuthToken, McpOAuthError>>,
    {
        let first = self.load(reference)?;
        if !first.needs_refresh(now_unix) {
            return first.access_secret();
        }
        let _local = self.local.lock().await;
        let _file = RefreshFileLock::acquire(&self.lock_path).await?;
        let current = self.load(reference)?;
        if !current.needs_refresh(now_unix) {
            return current.access_secret();
        }
        let refresh_token = current
            .refresh_token
            .as_ref()
            .cloned()
            .ok_or(McpOAuthError::RefreshUnavailable)?;
        let rotated = refresh(refresh_token).await?;
        rotated.validate_for(reference)?;
        self.save(reference, &rotated)?;
        rotated.access_secret()
    }
}

struct RefreshFileLock {
    file: fs::File,
}

impl RefreshFileLock {
    async fn acquire(path: &Path) -> Result<Self, McpOAuthError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| McpOAuthError::Lock)?;
        }
        let deadline = tokio::time::Instant::now() + REFRESH_LOCK_TIMEOUT;
        loop {
            let file = fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .map_err(|_| McpOAuthError::Lock)?;
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(McpOAuthError::LockBusy);
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(_) => return Err(McpOAuthError::Lock),
            }
        }
    }
}

impl Drop for RefreshFileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) struct McpOAuthFlow {
    listener: TcpListener,
    state: String,
    verifier: String,
    expected_issuer: String,
    require_issuer: bool,
    redirect_uri: String,
    authorization_url: Url,
}

impl McpOAuthFlow {
    pub(crate) async fn begin(
        metadata: &McpOAuthMetadata,
        client_id: &str,
        resource: &str,
        scopes: &BTreeSet<String>,
    ) -> Result<Self, McpOAuthError> {
        validate_id(client_id)?;
        validate_scopes(scopes)?;
        let resource = parse_exact_url(resource)?;
        if resource.fragment().is_some() {
            return Err(McpOAuthError::Reference);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| McpOAuthError::Loopback)?;
        let port = listener
            .local_addr()
            .map_err(|_| McpOAuthError::Loopback)?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
        let verifier = random_urlsafe();
        let state = random_urlsafe();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization_url = metadata.authorization_endpoint.clone();
        authorization_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", resource.as_str());
        if !scopes.is_empty() {
            authorization_url.query_pairs_mut().append_pair(
                "scope",
                &scopes.iter().cloned().collect::<Vec<_>>().join(" "),
            );
        }
        Ok(Self {
            listener,
            state,
            verifier,
            expected_issuer: metadata.issuer.as_str().to_owned(),
            require_issuer: metadata.authorization_response_iss_parameter_supported,
            redirect_uri,
            authorization_url,
        })
    }

    pub(crate) fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    pub(crate) fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub(crate) async fn wait(
        self,
        cancellation: &CancellationToken,
    ) -> Result<OAuthCallback, McpOAuthError> {
        let accepted = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpOAuthError::Cancelled),
            accepted = tokio::time::timeout(CALLBACK_TIMEOUT, self.listener.accept()) => {
                accepted.map_err(|_| McpOAuthError::CallbackTimeout)?
                    .map_err(|_| McpOAuthError::Loopback)?
            }
        };
        let (mut stream, peer) = accepted;
        if !peer.ip().is_loopback() {
            return Err(McpOAuthError::Loopback);
        }
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let count = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
                .await
                .map_err(|_| McpOAuthError::Callback)?
                .map_err(|_| McpOAuthError::Callback)?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > MAX_CALLBACK_BYTES {
                return Err(McpOAuthError::Callback);
            }
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = std::str::from_utf8(&bytes).map_err(|_| McpOAuthError::Callback)?;
        let target = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.strip_suffix(" HTTP/1.1"))
            .ok_or(McpOAuthError::Callback)?;
        let callback_url = Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|_| McpOAuthError::Callback)?;
        if callback_url.path() != "/oauth/callback" {
            return Err(McpOAuthError::CallbackPath);
        }
        let fields = callback_url
            .query_pairs()
            .into_owned()
            .collect::<BTreeMap<_, _>>();
        if fields.get("state") != Some(&self.state) {
            return Err(McpOAuthError::StateMismatch);
        }
        match fields.get("iss") {
            Some(issuer) if issuer != &self.expected_issuer => {
                return Err(McpOAuthError::IssuerMismatch);
            }
            None if self.require_issuer => return Err(McpOAuthError::IssuerMissing),
            _ => {}
        }
        if fields.contains_key("error") {
            return Err(McpOAuthError::AuthorizationRejected);
        }
        let code = fields.get("code").cloned().ok_or(McpOAuthError::Callback)?;
        if code.is_empty() || code.len() > 4096 {
            return Err(McpOAuthError::Callback);
        }
        let body = b"Authorization complete. You may close this window.";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(body).await;
        let _ = stream.shutdown().await;
        Ok(OAuthCallback {
            code,
            verifier: self.verifier,
            redirect_uri: self.redirect_uri,
        })
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct OAuthCallback {
    code: String,
    verifier: String,
    #[zeroize(skip)]
    redirect_uri: String,
}

impl fmt::Debug for OAuthCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthCallback")
            .field("code", &"<redacted>")
            .field("verifier", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct McpOAuthClient {
    security: McpHttpSecurity,
}

impl McpOAuthClient {
    pub(crate) fn new() -> Result<Self, McpOAuthError> {
        Ok(Self {
            security: McpHttpSecurity::default(),
        })
    }

    #[cfg(test)]
    fn allowing_loopback_http() -> Self {
        Self {
            security: McpHttpSecurity {
                allow_loopback_http: true,
            },
        }
    }

    pub(crate) async fn discover(
        &self,
        challenge: &McpAuthChallenge,
        expected_resource: &Url,
        expected_issuer: Option<&Url>,
        cancellation: &CancellationToken,
    ) -> Result<McpOAuthDiscovery, McpOAuthError> {
        let protected_bytes = self
            .get_json(&challenge.resource_metadata, cancellation)
            .await?;
        let protected_resource = McpProtectedResourceMetadata::parse(
            &protected_bytes,
            expected_resource,
            self.security,
        )?;
        let issuer = match expected_issuer {
            Some(expected)
                if protected_resource
                    .authorization_servers
                    .iter()
                    .any(|server| server == expected) =>
            {
                expected.clone()
            }
            Some(_) => return Err(McpOAuthError::UnsupportedIssuer),
            None if protected_resource.authorization_servers.len() == 1 => {
                protected_resource.authorization_servers[0].clone()
            }
            None => return Err(McpOAuthError::UnsupportedIssuer),
        };
        if !challenge.scopes.is_empty()
            && !protected_resource.scopes_supported.is_empty()
            && !challenge
                .scopes
                .is_subset(&protected_resource.scopes_supported)
        {
            return Err(McpOAuthError::InsufficientScope);
        }
        let metadata_url = oauth_metadata_url(&issuer)?;
        let authorization_bytes = self.get_json(&metadata_url, cancellation).await?;
        let authorization_server =
            McpOAuthMetadata::parse_with_security(&authorization_bytes, &issuer, self.security)?;
        Ok(McpOAuthDiscovery {
            protected_resource,
            authorization_server,
        })
    }

    async fn get_json(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, McpOAuthError> {
        let client = pinned_client(url, self.security, TOKEN_TIMEOUT)
            .await
            .map_err(map_http_error)?;
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(McpOAuthError::Cancelled),
            response = client.get(url.clone()).header(header::ACCEPT, "application/json").send() => {
                response.map_err(|error| if error.is_timeout() { McpOAuthError::Timeout } else { McpOAuthError::Transport })?
            }
        };
        if response.status().is_redirection() {
            return Err(McpOAuthError::Redirect);
        }
        if !response.status().is_success() {
            return Err(McpOAuthError::DiscoveryEndpoint(response.status().as_u16()));
        }
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return Err(McpOAuthError::MediaType);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_METADATA_BYTES as u64)
        {
            return Err(McpOAuthError::MetadataTooLarge);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(McpOAuthError::Cancelled),
                next = stream.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|_| McpOAuthError::Transport)?;
            if body.len().saturating_add(chunk.len()) > MAX_METADATA_BYTES {
                return Err(McpOAuthError::MetadataTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    pub(crate) async fn exchange(
        &self,
        metadata: &McpOAuthMetadata,
        reference: &McpOAuthReference,
        callback: OAuthCallback,
        client_secret: Option<&SecretString>,
    ) -> Result<McpOAuthToken, McpOAuthError> {
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", callback.code.clone()),
            ("redirect_uri", callback.redirect_uri.clone()),
            ("client_id", reference.client_id.clone()),
            ("code_verifier", callback.verifier.clone()),
            ("resource", reference.resource.clone()),
        ];
        if let Some(secret) = client_secret {
            form.push(("client_secret", secret.expose().to_owned()));
        }
        self.token_request(metadata, reference, form, None).await
    }

    pub(crate) async fn refresh(
        &self,
        metadata: &McpOAuthMetadata,
        reference: &McpOAuthReference,
        refresh_token: String,
        client_secret: Option<&SecretString>,
    ) -> Result<McpOAuthToken, McpOAuthError> {
        let mut form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token.clone()),
            ("client_id", reference.client_id.clone()),
            ("resource", reference.resource.clone()),
        ];
        if !reference.scopes.is_empty() {
            form.push((
                "scope",
                reference
                    .scopes
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
            ));
        }
        if let Some(secret) = client_secret {
            form.push(("client_secret", secret.expose().to_owned()));
        }
        self.token_request(metadata, reference, form, Some(refresh_token))
            .await
    }

    async fn token_request(
        &self,
        metadata: &McpOAuthMetadata,
        reference: &McpOAuthReference,
        mut form: Vec<(&str, String)>,
        prior_refresh: Option<String>,
    ) -> Result<McpOAuthToken, McpOAuthError> {
        let client = pinned_client(&metadata.token_endpoint, self.security, TOKEN_TIMEOUT)
            .await
            .map_err(map_http_error)?;
        let response = client
            .post(metadata.token_endpoint.clone())
            .form(&form)
            .send()
            .await;
        for (_, value) in &mut form {
            value.zeroize();
        }
        let response = response.map_err(|error| {
            if error.is_timeout() {
                McpOAuthError::Timeout
            } else {
                McpOAuthError::Transport
            }
        })?;
        if response.status().is_redirection() {
            return Err(McpOAuthError::Redirect);
        }
        if response.status() == reqwest::StatusCode::BAD_REQUEST
            || response.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(McpOAuthError::InvalidGrant);
        }
        if !response.status().is_success() {
            return Err(McpOAuthError::TokenEndpoint(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOKEN_BYTES as u64)
        {
            return Err(McpOAuthError::Token);
        }
        let mut bytes = response
            .bytes()
            .await
            .map_err(|_| McpOAuthError::Transport)?
            .to_vec();
        if bytes.len() > MAX_TOKEN_BYTES {
            bytes.zeroize();
            return Err(McpOAuthError::Token);
        }
        let parsed = serde_json::from_slice(&bytes).map_err(|_| McpOAuthError::Token);
        bytes.zeroize();
        let wire: TokenResponse = parsed?;
        if wire.token_type != "Bearer" || wire.access_token.trim().is_empty() {
            return Err(McpOAuthError::Token);
        }
        let scopes = if let Some(scope) = wire.scope {
            parse_scopes(&scope)?
        } else {
            reference.scopes.clone()
        };
        if !reference.scopes.is_subset(&scopes) {
            return Err(McpOAuthError::InsufficientScope);
        }
        let now = unix_now()?;
        Ok(McpOAuthToken {
            access_token: wire.access_token,
            refresh_token: wire.refresh_token.or(prior_refresh),
            token_type: wire.token_type,
            expires_at_unix: now.saturating_add(wire.expires_in.min(31_536_000)),
            scopes,
            issuer: reference.issuer.clone(),
            client_id: reference.client_id.clone(),
            resource: reference.resource.clone(),
        })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

fn split_challenge_fields(value: &str) -> Result<Vec<&str>, McpOAuthError> {
    let mut fields = Vec::new();
    let mut start = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            fields.push(value[start..index].trim());
            start = index + 1;
        }
    }
    if quoted || escaped {
        return Err(McpOAuthError::Challenge);
    }
    fields.push(value[start..].trim());
    if fields.iter().any(|field| field.is_empty()) {
        return Err(McpOAuthError::Challenge);
    }
    Ok(fields)
}

fn parse_scopes(value: &str) -> Result<BTreeSet<String>, McpOAuthError> {
    parse_scope_array(value.split_ascii_whitespace().map(str::to_owned).collect())
}

fn parse_scope_array(values: Vec<String>) -> Result<BTreeSet<String>, McpOAuthError> {
    let scopes = values.into_iter().collect::<BTreeSet<_>>();
    validate_scopes(&scopes)?;
    Ok(scopes)
}

fn validate_scopes(scopes: &BTreeSet<String>) -> Result<(), McpOAuthError> {
    if scopes.len() > MAX_SCOPES
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > MAX_SCOPE_BYTES
                || scope.bytes().any(|byte| byte <= 0x20 || byte >= 0x7f)
        })
    {
        Err(McpOAuthError::Scope)
    } else {
        Ok(())
    }
}

fn validate_id(value: &str) -> Result<(), McpOAuthError> {
    if value.is_empty()
        || value.len() > 1024
        || value.chars().any(|character| character.is_control())
    {
        Err(McpOAuthError::Reference)
    } else {
        Ok(())
    }
}

fn parse_exact_url(value: &str) -> Result<Url, McpOAuthError> {
    let url = Url::parse(value).map_err(|_| McpOAuthError::Metadata)?;
    if url.username() != "" || url.password().is_some() || url.host_str().is_none() {
        return Err(McpOAuthError::Metadata);
    }
    Ok(url)
}

fn permitted_metadata_url(url: &Url, security: McpHttpSecurity) -> bool {
    url.fragment().is_none()
        && match url.scheme() {
            "https" => true,
            "http" if security.allow_loopback_http => url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }),
            _ => false,
        }
}

fn oauth_metadata_url(issuer: &Url) -> Result<Url, McpOAuthError> {
    if issuer.query().is_some() || issuer.fragment().is_some() {
        return Err(McpOAuthError::Metadata);
    }
    let suffix = issuer.path().trim_start_matches('/');
    let path = if suffix.is_empty() {
        "/.well-known/oauth-authorization-server".to_owned()
    } else {
        format!("/.well-known/oauth-authorization-server/{suffix}")
    };
    let mut metadata = issuer.clone();
    metadata.set_path(&path);
    metadata.set_query(None);
    Ok(metadata)
}

fn map_http_error(error: McpHttpError) -> McpOAuthError {
    match error {
        McpHttpError::HttpsRequired | McpHttpError::InvalidEndpoint => McpOAuthError::Metadata,
        McpHttpError::Dns | McpHttpError::Connect => McpOAuthError::Transport,
        McpHttpError::AddressPolicy => McpOAuthError::AddressPolicy,
        McpHttpError::Timeout => McpOAuthError::Timeout,
        _ => McpOAuthError::Transport,
    }
}

fn random_urlsafe() -> String {
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(first.as_bytes());
    bytes[16..].copy_from_slice(second.as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_now() -> Result<u64, McpOAuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| McpOAuthError::Clock)
}

fn map_store_error(_error: CredentialError) -> McpOAuthError {
    McpOAuthError::StoreUnavailable
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpOAuthError {
    Challenge,
    Metadata,
    MetadataTooLarge,
    ResourceMismatch,
    UnsupportedIssuer,
    DiscoveryEndpoint(u16),
    MediaType,
    AddressPolicy,
    IssuerMismatch,
    IssuerMissing,
    PkceUnsupported,
    Reference,
    Scope,
    Loopback,
    Callback,
    CallbackPath,
    CallbackTimeout,
    StateMismatch,
    AuthorizationRejected,
    Cancelled,
    Timeout,
    Redirect,
    Transport,
    TokenEndpoint(u16),
    InvalidGrant,
    InsufficientScope,
    Token,
    TokenBinding,
    CredentialMissing,
    RefreshUnavailable,
    StoreUnavailable,
    Lock,
    LockBusy,
    Clock,
}

impl fmt::Display for McpOAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Challenge => formatter.write_str("MCP OAuth challenge is invalid"),
            Self::Metadata => formatter.write_str("MCP OAuth metadata is invalid"),
            Self::MetadataTooLarge => formatter.write_str("MCP OAuth metadata exceeds its limit"),
            Self::ResourceMismatch => {
                formatter.write_str("MCP OAuth resource identity does not match")
            }
            Self::UnsupportedIssuer => {
                formatter.write_str("MCP OAuth authorization-server selection is unsupported")
            }
            Self::DiscoveryEndpoint(status) => write!(
                formatter,
                "MCP OAuth discovery endpoint returned status {status}"
            ),
            Self::MediaType => {
                formatter.write_str("MCP OAuth endpoint returned an unsupported media type")
            }
            Self::AddressPolicy => {
                formatter.write_str("MCP OAuth endpoint resolves to a blocked address")
            }
            Self::IssuerMismatch => formatter.write_str("MCP OAuth issuer does not match"),
            Self::IssuerMissing => {
                formatter.write_str("MCP OAuth callback omitted the required issuer")
            }
            Self::PkceUnsupported => {
                formatter.write_str("MCP OAuth server does not support PKCE S256")
            }
            Self::Reference => formatter.write_str("MCP OAuth credential reference is invalid"),
            Self::Scope => formatter.write_str("MCP OAuth scope set is invalid"),
            Self::Loopback => formatter.write_str("MCP OAuth loopback listener is unavailable"),
            Self::Callback => formatter.write_str("MCP OAuth callback is invalid"),
            Self::CallbackPath => formatter.write_str("MCP OAuth callback path is invalid"),
            Self::CallbackTimeout => formatter.write_str("MCP OAuth callback timed out"),
            Self::StateMismatch => formatter.write_str("MCP OAuth callback state does not match"),
            Self::AuthorizationRejected => {
                formatter.write_str("MCP OAuth authorization was rejected")
            }
            Self::Cancelled => formatter.write_str("MCP OAuth operation was cancelled"),
            Self::Timeout => formatter.write_str("MCP OAuth endpoint timed out"),
            Self::Redirect => formatter.write_str("MCP OAuth endpoint redirect was rejected"),
            Self::Transport => formatter.write_str("MCP OAuth transport failed"),
            Self::TokenEndpoint(status) => write!(
                formatter,
                "MCP OAuth token endpoint returned status {status}"
            ),
            Self::InvalidGrant => formatter.write_str("MCP OAuth grant is invalid or revoked"),
            Self::InsufficientScope => {
                formatter.write_str("MCP OAuth grant omitted required scopes")
            }
            Self::Token => formatter.write_str("MCP OAuth token response is invalid"),
            Self::TokenBinding => {
                formatter.write_str("MCP OAuth token binding does not match its reference")
            }
            Self::CredentialMissing => formatter.write_str("MCP OAuth credential is missing"),
            Self::RefreshUnavailable => formatter.write_str("MCP OAuth refresh is unavailable"),
            Self::StoreUnavailable => formatter.write_str("MCP OAuth secure store is unavailable"),
            Self::Lock => formatter.write_str("MCP OAuth refresh lock is unavailable"),
            Self::LockBusy => formatter.write_str("MCP OAuth refresh is active in another process"),
            Self::Clock => formatter.write_str("system time is unavailable"),
        }
    }
}

impl Error for McpOAuthError {}

#[cfg(test)]
mod tests;
