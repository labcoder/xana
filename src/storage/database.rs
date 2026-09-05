//! One owned SQLCipher connection with durable transactions and bounded caches.

use super::keys::Secrets;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, limits::Limit};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::Duration,
};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(super) struct Database {
    pub(super) connection: Connection,
    pub(super) secrets: Secrets,
    // Shared live-owner lease. SQLite transactions serialize writes; a lifecycle
    // operation needs the exclusive lease, and each Conversation has one writer.
    _lease: File,
}

pub(super) fn read_usize(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<usize> {
    let value: i64 = row.get(column)?;
    usize::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

pub(super) fn read_u64(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(column)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

impl Database {
    pub(super) fn create(root: &Path, id: Uuid, secrets: Secrets) -> Result<Self> {
        let lease = lease(root)?;
        let path = root.join("content.sqlite");
        File::create_new(&path)
            .context("protected database already exists or cannot be created")?;
        let mut connection = connect(&path, &secrets)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch("
            CREATE TABLE store_identity(id TEXT PRIMARY KEY, version INTEGER NOT NULL);
            CREATE TABLE documents(name TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision > 0), body BLOB NOT NULL);
            PRAGMA user_version=1;")?;
        transaction.execute_batch(super::history::SCHEMA)?;
        transaction.execute_batch("CREATE TABLE encrypted_artifacts(hash TEXT PRIMARY KEY, file_id TEXT NOT NULL UNIQUE, length INTEGER NOT NULL);")?;
        transaction.execute(
            "INSERT INTO store_identity VALUES (?1, 1)",
            [id.to_string()],
        )?;
        transaction.commit()?;
        Ok(Self {
            connection,
            secrets,
            _lease: lease,
        })
    }

    pub(super) fn open(root: &Path, id: Uuid, secrets: Secrets) -> Result<Self> {
        let lease = lease(root)?;
        let path = root.join("content.sqlite");
        ensure!(
            std::fs::symlink_metadata(&path)?.file_type().is_file(),
            "protected database is not a regular file"
        );
        let connection = connect(&path, &secrets)?;
        let (actual, version): (String, u32) = connection
            .query_row("SELECT id,version FROM store_identity", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .context("protected database could not be authenticated; no plaintext fallback")?;
        ensure!(
            actual == id.to_string() && version == 1,
            "protected database identity or version differs"
        );
        Ok(Self {
            connection,
            secrets,
            _lease: lease,
        })
    }

    pub(super) fn verify(&self) -> Result<()> {
        // Check the cryptographic page envelope as well as SQLite's structure:
        // https://www.zetetic.net/sqlcipher/sqlcipher-api/#cipher_integrity_check
        ensure!(
            !self
                .connection
                .prepare("PRAGMA cipher_integrity_check")?
                .exists([])?,
            "protected database page authentication failed"
        );
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        ensure!(
            integrity == "ok",
            "protected database integrity verification failed"
        );
        Ok(())
    }

    pub(super) fn checkpoint(&self) -> Result<()> {
        let busy: u32 =
            self.connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        ensure!(
            busy == 0,
            "protected database checkpoint is busy; store remains unlocked"
        );
        Ok(())
    }

    pub(super) fn exclusive(&self) -> Result<()> {
        fs2::FileExt::unlock(&self._lease)?;
        fs2::FileExt::try_lock_exclusive(&self._lease)
            .context("close other Xana owners before this storage lifecycle operation")
    }

    pub(super) fn seal(self, root: &Path) -> Result<()> {
        self.checkpoint()?;
        let Self {
            connection,
            secrets,
            _lease: lease,
        } = self;
        drop(connection);
        drop(secrets);
        // Do not claim a global lock while another live owner can retain keys.
        fs2::FileExt::unlock(&lease)?;
        fs2::FileExt::try_lock_exclusive(&lease).context(
            "another Xana owner is still open; this handle was closed, but the home is not locked",
        )?;
        if !root.join("locked").exists() {
            super::write_new_synced(&root.join("locked"), b"locked\n")?;
        }
        Ok(())
    }
}

fn connect(path: &Path, secrets: &Secrets) -> Result<Connection> {
    use std::fmt::Write;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut raw_key = Zeroizing::new(String::with_capacity(67));
    raw_key.push_str("x'");
    for byte in secrets.database.iter() {
        write!(&mut *raw_key, "{byte:02x}")?;
    }
    raw_key.push('\'');
    connection.pragma_update(None, "key", raw_key.as_str())?;
    connection.pragma_update(None, "cipher_log_level", "NONE")?;
    let version: String = connection.query_row("PRAGMA cipher_version", [], |row| row.get(0))?;
    ensure!(
        version.starts_with("4.18.0 "),
        "unreviewed linked SQLCipher version"
    );
    // Force key validation before any schema/configuration mutation.
    connection
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, u32>(0)
        })
        .context("protected database could not be authenticated")?;
    connection.busy_timeout(Duration::from_millis(100))?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, 16 * 1024 * 1024)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "cache_size", -2048)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    Ok(connection)
}

fn lease(root: &Path) -> Result<File> {
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("owner.lock"))?;
    fs2::FileExt::try_lock_shared(&lease).context(
        "protected store has an exclusive lifecycle operation; try again after it completes",
    )?;
    Ok(lease)
}
