//! Credential lifecycle primitives for optional subscription providers.
//!
//! This module intentionally stops short of a vendor-specific OAuth adapter.
//! It provides the safe seams needed once an official endpoint, audience,
//! scopes, and permission model are accepted.

use futures::future::BoxFuture;
use std::{
    collections::BTreeMap,
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialId(String);

impl CredentialId {
    pub fn new(value: impl Into<String>) -> Result<Self, AuthError> {
        let value = value.into();
        if value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
        {
            return Err(AuthError::InvalidId(value));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("REDACTED")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessCredential {
    pub access_token: SecretValue,
    pub refresh_token: Option<SecretValue>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStatus {
    Missing,
    Valid { expires_at: Option<u64> },
    Expired,
    Unavailable { reason: String },
}

pub trait Clock: Send + Sync {
    fn now_unix_seconds(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

pub trait TokenStore: Send + Sync {
    fn get<'a>(
        &'a self,
        id: &'a CredentialId,
    ) -> BoxFuture<'a, Result<Option<AccessCredential>, AuthError>>;
    fn replace<'a>(
        &'a self,
        id: &'a CredentialId,
        credential: AccessCredential,
    ) -> BoxFuture<'a, Result<(), AuthError>>;
    fn delete<'a>(&'a self, id: &'a CredentialId) -> BoxFuture<'a, Result<(), AuthError>>;
}

#[derive(Clone, Default)]
pub struct InMemoryTokenStore {
    inner: Arc<Mutex<BTreeMap<CredentialId, AccessCredential>>>,
}

impl TokenStore for InMemoryTokenStore {
    fn get<'a>(
        &'a self,
        id: &'a CredentialId,
    ) -> BoxFuture<'a, Result<Option<AccessCredential>, AuthError>> {
        Box::pin(async move { Ok(self.inner.lock().await.get(id).cloned()) })
    }
    fn replace<'a>(
        &'a self,
        id: &'a CredentialId,
        credential: AccessCredential,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            self.inner.lock().await.insert(id.clone(), credential);
            Ok(())
        })
    }
    fn delete<'a>(&'a self, id: &'a CredentialId) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            self.inner.lock().await.remove(id);
            Ok(())
        })
    }
}

pub trait CredentialProvider: Send + Sync {
    fn access<'a>(
        &'a self,
        id: &'a CredentialId,
    ) -> BoxFuture<'a, Result<AccessCredential, AuthError>>;
    fn status<'a>(
        &'a self,
        id: &'a CredentialId,
    ) -> BoxFuture<'a, Result<CredentialStatus, AuthError>>;
}

pub trait CredentialControl: Send + Sync {
    fn login<'a>(
        &'a self,
        id: &'a CredentialId,
    ) -> BoxFuture<'a, Result<DeviceAuthorization, AuthError>>;
    fn complete<'a>(
        &'a self,
        id: &'a CredentialId,
        authorization: &'a DeviceAuthorization,
    ) -> BoxFuture<'a, Result<AccessCredential, AuthError>>;
    fn logout<'a>(&'a self, id: &'a CredentialId) -> BoxFuture<'a, Result<(), AuthError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Pending,
    SlowDown,
    Authorized(AccessCredential),
    Expired,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    InvalidId(String),
    Unavailable(String),
    MissingCredential,
    Expired,
    PollExpired,
    AccessDenied,
    Store(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid credential id {id:?}"),
            Self::Unavailable(reason) => write!(f, "credential provider unavailable: {reason}"),
            Self::MissingCredential => f.write_str("credential is not configured"),
            Self::Expired => f.write_str("credential has expired"),
            Self::PollExpired => f.write_str("device authorization expired"),
            Self::AccessDenied => f.write_str("device authorization was denied"),
            Self::Store(reason) => write!(f, "credential store error: {reason}"),
        }
    }
}
impl std::error::Error for AuthError {}

pub async fn status<S: TokenStore, C: Clock>(
    store: &S,
    clock: &C,
    id: &CredentialId,
) -> Result<CredentialStatus, AuthError> {
    let Some(credential) = store.get(id).await? else {
        return Ok(CredentialStatus::Missing);
    };
    if credential
        .expires_at
        .is_some_and(|expires_at| expires_at <= clock.now_unix_seconds())
    {
        Ok(CredentialStatus::Expired)
    } else {
        Ok(CredentialStatus::Valid {
            expires_at: credential.expires_at,
        })
    }
}

pub async fn access<S: TokenStore, C: Clock>(
    store: &S,
    clock: &C,
    id: &CredentialId,
) -> Result<AccessCredential, AuthError> {
    let Some(credential) = store.get(id).await? else {
        return Err(AuthError::MissingCredential);
    };
    if credential
        .expires_at
        .is_some_and(|expires_at| expires_at <= clock.now_unix_seconds())
    {
        return Err(AuthError::Expired);
    }
    Ok(credential)
}

/// Serialize refresh work per credential id so concurrent turns cannot race
/// refresh-token rotation. The caller performs the network request and then
/// atomically replaces the credential in its secure store.
#[derive(Clone, Default)]
pub struct RefreshCoordinator {
    locks: Arc<Mutex<BTreeMap<CredentialId, Arc<Mutex<()>>>>>,
}

impl RefreshCoordinator {
    pub async fn run<F, Fut>(
        &self,
        id: &CredentialId,
        refresh: F,
    ) -> Result<AccessCredential, AuthError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<AccessCredential, AuthError>>,
    {
        let lock = {
            let mut locks = self.locks.lock().await;
            Arc::clone(
                locks
                    .entry(id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = lock.lock().await;
        refresh().await
    }
}

/// Drive an injected device-code poller. The caller owns the sleep scheduler
/// so tests and future frontends can cancel without blocking a worker thread.
pub async fn poll_device_code<F, Fut>(
    authorization: &DeviceAuthorization,
    mut poll: F,
    mut wait: impl FnMut(Duration) -> Fut,
) -> Result<AccessCredential, AuthError>
where
    F: FnMut(&str) -> BoxFuture<'static, Result<PollOutcome, AuthError>>,
    Fut: std::future::Future<Output = ()>,
{
    let mut remaining = authorization.expires_in;
    let mut interval = authorization.interval;
    while remaining > Duration::ZERO {
        match poll(&authorization.device_code).await? {
            PollOutcome::Authorized(credential) => return Ok(credential),
            PollOutcome::Denied => return Err(AuthError::AccessDenied),
            PollOutcome::Expired => return Err(AuthError::PollExpired),
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval = interval.saturating_add(Duration::from_secs(5)),
        }
        wait(interval).await;
        remaining = remaining.saturating_sub(interval);
    }
    Err(AuthError::PollExpired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    #[derive(Clone)]
    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn now_unix_seconds(&self) -> u64 {
            self.0
        }
    }

    fn id() -> CredentialId {
        CredentialId::new("test-provider").unwrap()
    }
    fn credential(expires_at: Option<u64>) -> AccessCredential {
        AccessCredential {
            access_token: SecretValue::new("secret"),
            refresh_token: Some(SecretValue::new("refresh")),
            expires_at,
        }
    }

    #[tokio::test]
    async fn status_and_access_are_expiry_aware_without_exposing_secret() {
        let store = InMemoryTokenStore::default();
        let id = id();
        store.replace(&id, credential(Some(10))).await.unwrap();
        assert_eq!(
            status(&store, &FixedClock(9), &id).await.unwrap(),
            CredentialStatus::Valid {
                expires_at: Some(10)
            }
        );
        assert_eq!(
            status(&store, &FixedClock(10), &id).await.unwrap(),
            CredentialStatus::Expired
        );
        assert_eq!(
            format!("{:?}", credential(Some(10)).access_token),
            "REDACTED"
        );
    }

    #[tokio::test]
    async fn device_flow_honors_pending_and_slow_down_then_rotates_once() {
        let authorization = DeviceAuthorization {
            device_code: "device".into(),
            user_code: "user".into(),
            verification_uri: "https://example.test".into(),
            expires_in: Duration::from_secs(30),
            interval: Duration::from_secs(1),
        };
        let mut outcomes = vec![
            PollOutcome::Pending,
            PollOutcome::SlowDown,
            PollOutcome::Authorized(credential(None)),
        ]
        .into_iter();
        let mut waits = Vec::new();
        let result = poll_device_code(
            &authorization,
            move |_| {
                let outcome = outcomes.next().unwrap().clone();
                async move { Ok(outcome) }.boxed()
            },
            |duration| {
                waits.push(duration);
                async {}
            },
        )
        .await
        .unwrap();
        assert_eq!(result.access_token.expose(), "secret");
        assert_eq!(waits, vec![Duration::from_secs(1), Duration::from_secs(6)]);
    }

    #[tokio::test]
    async fn missing_and_logout_are_explicit() {
        let store = InMemoryTokenStore::default();
        let id = id();
        assert_eq!(
            status(&store, &FixedClock(0), &id).await.unwrap(),
            CredentialStatus::Missing
        );
        store.replace(&id, credential(None)).await.unwrap();
        store.delete(&id).await.unwrap();
        assert_eq!(
            status(&store, &FixedClock(0), &id).await.unwrap(),
            CredentialStatus::Missing
        );
    }

    #[tokio::test]
    async fn refresh_coordinator_serializes_same_credential_only() {
        let coordinator = RefreshCoordinator::default();
        let id = id();
        let counter = Arc::new(Mutex::new(0usize));
        let first_counter = Arc::clone(&counter);
        let first = coordinator.run(&id, move || async move {
            let mut value = first_counter.lock().await;
            *value += 1;
            Ok(credential(None))
        });
        let second_counter = Arc::clone(&counter);
        let second = coordinator.run(&id, move || async move {
            let mut value = second_counter.lock().await;
            *value += 1;
            Ok(credential(None))
        });
        let _ = tokio::join!(first, second);
        assert_eq!(*counter.lock().await, 2);
    }
}
