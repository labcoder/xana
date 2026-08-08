//! Production API-key resolution and OS-backed secret storage.
//!
//! Shared configuration contains only a credential reference. Static API keys
//! live in the operating-system credential store or in an explicitly named
//! process environment variable; no plaintext file fallback exists.

use crate::config::CredentialReference;
use std::{error::Error, ffi::OsString, fmt};
use zeroize::{Zeroize, ZeroizeOnDrop};

const KEYRING_SERVICE: &str = "dev.xana.credentials";

#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct SecretString(String);

impl SecretString {
    pub(crate) fn new(mut value: String) -> Result<Self, CredentialError> {
        if value.trim().is_empty() {
            value.zeroize();
            return Err(CredentialError::Empty);
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(REDACTED)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialAvailability {
    Available,
    Missing,
}

#[derive(Debug)]
pub(crate) enum CredentialError {
    MissingEnvironment { variable: String },
    NonUnicodeEnvironment { variable: String },
    MissingStored { id: String },
    Empty,
    StoreUnavailable(String),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment { variable } => {
                write!(f, "environment variable {variable} is not set")
            }
            Self::NonUnicodeEnvironment { variable } => {
                write!(f, "environment variable {variable} is not valid Unicode")
            }
            Self::MissingStored { id } => write!(f, "stored credential {id:?} is missing"),
            Self::Empty => f.write_str("credential is empty"),
            Self::StoreUnavailable(reason) => {
                write!(
                    f,
                    "operating-system credential store is unavailable: {reason}"
                )
            }
        }
    }
}

impl Error for CredentialError {}

pub(crate) trait Environment: Send + Sync {
    fn get(&self, name: &str) -> Option<OsString>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

pub(crate) trait SecretStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<String>, CredentialError>;
    fn set(&self, id: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, id: &str) -> Result<bool, CredentialError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OsSecretStore;

impl OsSecretStore {
    fn entry(id: &str) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(KEYRING_SERVICE, id)
            .map_err(|error| CredentialError::StoreUnavailable(error.to_string()))
    }
}

impl SecretStore for OsSecretStore {
    fn get(&self, id: &str) -> Result<Option<String>, CredentialError> {
        match Self::entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(CredentialError::StoreUnavailable(error.to_string())),
        }
    }

    fn set(&self, id: &str, secret: &str) -> Result<(), CredentialError> {
        if secret.trim().is_empty() {
            return Err(CredentialError::Empty);
        }
        Self::entry(id)?
            .set_password(secret)
            .map_err(|error| CredentialError::StoreUnavailable(error.to_string()))
    }

    fn delete(&self, id: &str) -> Result<bool, CredentialError> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(CredentialError::StoreUnavailable(error.to_string())),
        }
    }
}

pub(crate) struct CredentialResolver<E = ProcessEnvironment, S = OsSecretStore> {
    environment: E,
    store: S,
}

impl Default for CredentialResolver {
    fn default() -> Self {
        Self {
            environment: ProcessEnvironment,
            store: OsSecretStore,
        }
    }
}

impl<E: Environment, S: SecretStore> CredentialResolver<E, S> {
    #[cfg(test)]
    fn new(environment: E, store: S) -> Self {
        Self { environment, store }
    }

    pub(crate) fn resolve(
        &self,
        reference: &CredentialReference,
    ) -> Result<SecretString, CredentialError> {
        match reference {
            CredentialReference::Environment { variable } => {
                let value = self.environment.get(variable).ok_or_else(|| {
                    CredentialError::MissingEnvironment {
                        variable: variable.clone(),
                    }
                })?;
                let value =
                    value
                        .into_string()
                        .map_err(|_| CredentialError::NonUnicodeEnvironment {
                            variable: variable.clone(),
                        })?;
                SecretString::new(value)
            }
            CredentialReference::Stored { id } => {
                let value = self
                    .store
                    .get(id)?
                    .ok_or_else(|| CredentialError::MissingStored { id: id.clone() })?;
                SecretString::new(value)
            }
        }
    }

    pub(crate) fn status(
        &self,
        reference: &CredentialReference,
    ) -> Result<CredentialAvailability, CredentialError> {
        let available = match reference {
            CredentialReference::Environment { variable } => self
                .environment
                .get(variable)
                .and_then(|value| value.into_string().ok())
                .is_some_and(|value| SecretString::new(value).is_ok()),
            CredentialReference::Stored { id } => self
                .store
                .get(id)?
                .is_some_and(|value| SecretString::new(value).is_ok()),
        };
        Ok(if available {
            CredentialAvailability::Available
        } else {
            CredentialAvailability::Missing
        })
    }
}

pub(crate) fn store_secret(id: &str, secret: &SecretString) -> Result<(), CredentialError> {
    OsSecretStore.set(id, secret.expose())
}

pub(crate) fn delete_secret(id: &str) -> Result<bool, CredentialError> {
    OsSecretStore.delete(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};

    struct FakeEnvironment(BTreeMap<String, OsString>);
    impl Environment for FakeEnvironment {
        fn get(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    #[derive(Default)]
    struct FakeStore(Mutex<BTreeMap<String, String>>);
    impl SecretStore for FakeStore {
        fn get(&self, id: &str) -> Result<Option<String>, CredentialError> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        fn set(&self, id: &str, secret: &str) -> Result<(), CredentialError> {
            self.0.lock().unwrap().insert(id.into(), secret.into());
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<bool, CredentialError> {
            Ok(self.0.lock().unwrap().remove(id).is_some())
        }
    }

    #[test]
    fn references_resolve_only_from_the_declared_source() {
        let store = FakeStore::default();
        store.set("remote", "stored-secret").unwrap();
        let resolver = CredentialResolver::new(
            FakeEnvironment(BTreeMap::from([(
                "REMOTE_KEY".into(),
                OsString::from("environment-secret"),
            )])),
            store,
        );
        let environment = resolver
            .resolve(&CredentialReference::Environment {
                variable: "REMOTE_KEY".into(),
            })
            .unwrap();
        let stored = resolver
            .resolve(&CredentialReference::Stored {
                id: "remote".into(),
            })
            .unwrap();
        assert_eq!(environment.expose(), "environment-secret");
        assert_eq!(stored.expose(), "stored-secret");
        assert_eq!(format!("{environment:?}"), "SecretString(REDACTED)");
    }

    #[test]
    fn missing_declared_source_never_falls_back() {
        let resolver =
            CredentialResolver::new(FakeEnvironment(BTreeMap::new()), FakeStore::default());
        assert!(matches!(
            resolver.resolve(&CredentialReference::Environment {
                variable: "MISSING".into(),
            }),
            Err(CredentialError::MissingEnvironment { .. })
        ));
    }
}
