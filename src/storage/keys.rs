//! Independent age recovery and explicitly local OS custody. Never log secrets.

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use keyring_core::api::CredentialStoreApi;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

pub(crate) type RecoveryIdentity = age::x25519::Identity;
pub(crate) type RecoveryRecipient = age::x25519::Recipient;

pub(crate) trait KeyCustody {
    fn load(&self, id: Uuid) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn store(&self, id: Uuid, secret: &[u8]) -> Result<()>;
}

pub(crate) struct OsCustody;

/// Explicit portable initialization. This never becomes an automatic fallback
/// when OS custody fails; the user must select and retain independent recovery.
pub(crate) struct RecoveryOnlyCustody;
impl KeyCustody for RecoveryOnlyCustody {
    fn load(&self, _id: Uuid) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(None)
    }
    fn store(&self, _id: Uuid, _secret: &[u8]) -> Result<()> {
        Ok(())
    }
}

pub(crate) fn read_recovery_identity(path: &std::path::Path) -> Result<RecoveryIdentity> {
    let bytes = Zeroizing::new(crate::bounded_file::read(path, 4096)?);
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid recovery key encoding"))?;
    let mut keys = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let key = keys.next().context("recovery key is empty")?;
    ensure!(
        keys.next().is_none(),
        "recovery file must contain exactly one identity"
    );
    key.parse()
        .map_err(|_| anyhow::anyhow!("invalid age recovery identity"))
}

impl OsCustody {
    fn entry(id: Uuid) -> Result<keyring_core::Entry> {
        const SERVICE: &str = "dev.xana.protected-storage";
        #[cfg(target_os = "windows")]
        let entry = windows_native_keyring_store::Store::new()?.build(
            SERVICE,
            &id.to_string(),
            Some(&std::collections::HashMap::from([("persistence", "Local")])),
        )?;
        #[cfg(target_os = "macos")]
        let entry = apple_native_keyring_store::keychain::Store::new()?.build(
            SERVICE,
            &id.to_string(),
            None,
        )?;
        #[cfg(target_os = "linux")]
        let entry = zbus_secret_service_keyring_store::Store::new()?.build(
            SERVICE,
            &id.to_string(),
            None,
        )?;
        Ok(entry)
    }
}

impl KeyCustody for OsCustody {
    fn load(&self, id: Uuid) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match Self::entry(id)?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => {
                Err(error).context("OS storage custody is unavailable; no plaintext fallback")
            }
        }
    }

    fn store(&self, id: Uuid, secret: &[u8]) -> Result<()> {
        let entry = Self::entry(id)?;
        entry
            .set_secret(secret)
            .context("could not store protected key in OS custody")?;
        let recovered = Zeroizing::new(entry.get_secret()?);
        ensure!(
            recovered.as_slice() == secret,
            "OS custody verification differs"
        );
        Ok(())
    }
}

pub(super) struct Secrets {
    pub(super) database: Zeroizing<[u8; 32]>,
    pub(super) artifacts: age::x25519::Identity,
}

#[derive(Serialize, Deserialize, Zeroize)]
#[serde(deny_unknown_fields)]
#[zeroize(drop)]
struct EncodedSecrets {
    version: u32,
    database: String,
    artifacts: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryBundle {
    version: u32,
    id: Uuid,
    secrets: EncodedSecrets,
}

impl Secrets {
    pub(super) fn generate() -> Result<Self> {
        let mut database = Zeroizing::new([0; 32]);
        getrandom::fill(&mut *database)
            .map_err(|_| anyhow::anyhow!("OS randomness is unavailable"))?;
        Ok(Self {
            database,
            artifacts: age::x25519::Identity::generate(),
        })
    }

    fn encoded(&self) -> EncodedSecrets {
        EncodedSecrets {
            version: 1,
            database: STANDARD.encode(*self.database),
            artifacts: self.artifacts.to_string().expose_secret().to_owned(),
        }
    }

    pub(super) fn encode(&self) -> Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(serde_json::to_vec(&self.encoded())?))
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= super::MAX_BOOTSTRAP,
            "invalid storage-key size"
        );
        // Never include serde's offending text or a secret in the reported error.
        let encoded = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("invalid protected storage key"))?;
        Self::from_encoded(encoded)
    }

    fn from_encoded(encoded: EncodedSecrets) -> Result<Self> {
        ensure!(encoded.version == 1, "unsupported protected-key version");
        let raw = Zeroizing::new(
            STANDARD
                .decode(&encoded.database)
                .map_err(|_| anyhow::anyhow!("invalid database key"))?,
        );
        ensure!(raw.len() == 32, "invalid database-key length");
        let mut database = Zeroizing::new([0; 32]);
        database.copy_from_slice(&raw);
        let artifacts = encoded
            .artifacts
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid artifact key"))?;
        Ok(Self {
            database,
            artifacts,
        })
    }

    pub(super) fn recovery_envelope(
        &self,
        id: Uuid,
        recipient: &RecoveryRecipient,
    ) -> Result<Vec<u8>> {
        let bundle = RecoveryBundle {
            version: 1,
            id,
            secrets: self.encoded(),
        };
        let clear = Zeroizing::new(serde_json::to_vec(&bundle)?);
        Ok(age::encrypt(recipient, &clear)?)
    }

    pub(super) fn recover(id: Uuid, envelope: &[u8], identity: &RecoveryIdentity) -> Result<Self> {
        ensure!(
            envelope.len() <= super::MAX_BOOTSTRAP,
            "invalid recovery-envelope size"
        );
        let clear = Zeroizing::new(
            age::decrypt(identity, envelope)
                .map_err(|_| anyhow::anyhow!("could not authenticate recovery envelope"))?,
        );
        let bundle: RecoveryBundle = serde_json::from_slice(&clear)
            .map_err(|_| anyhow::anyhow!("invalid recovery bundle"))?;
        ensure!(
            bundle.version == 1 && bundle.id == id,
            "recovery identity mismatch"
        );
        Self::from_encoded(bundle.secrets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_recovery_authenticates_identity_and_key() {
        let secrets = Secrets::generate().unwrap();
        let recovery = RecoveryIdentity::generate();
        let id = Uuid::new_v4();
        let envelope = secrets
            .recovery_envelope(id, &recovery.to_public())
            .unwrap();
        let restored = Secrets::recover(id, &envelope, &recovery).unwrap();
        assert_eq!(*restored.database, *secrets.database);
        assert!(Secrets::recover(Uuid::new_v4(), &envelope, &recovery).is_err());
        assert!(Secrets::recover(id, &envelope, &RecoveryIdentity::generate()).is_err());
        let mut tampered = envelope;
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(Secrets::recover(id, &tampered, &recovery).is_err());
    }
}
