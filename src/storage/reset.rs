//! Explicit reset removes selected logical records, never the entire database.

use super::ProtectedStore;
use anyhow::Result;

impl ProtectedStore {
    pub(crate) fn reset_records(&self, sessions: bool, setup: bool) -> Result<()> {
        self.with_exclusive_database(|db| {
            let transaction = db.connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if sessions {
                transaction.execute("DELETE FROM native_sessions", [])?;
                // Ciphertext objects are unreferenced, not claimed securely erased.
                transaction.execute("DELETE FROM encrypted_artifacts", [])?;
                transaction.execute("DELETE FROM documents WHERE name GLOB 'workspace-hosts/*' OR name GLOB 'frontend/composer-history/*'", [])?;
            }
            if sessions || setup {
                transaction.execute("DELETE FROM documents WHERE name GLOB 'managed-threads/*'", [])?;
            }
            transaction.commit()?;
            db.checkpoint()
        })
    }
}
