//! Bounded logical documents. Names and bodies are both inside SQLCipher.

use super::{ProtectedStore, database::read_usize};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};

impl ProtectedStore {
    pub(crate) fn document(&self, name: &str, max_bytes: usize) -> Result<Option<Vec<u8>>> {
        self.with_database(|db| read(&db.connection, name, max_bytes))
    }

    pub(crate) fn set_document(&self, name: &str, bytes: &[u8], max_bytes: usize) -> Result<()> {
        validate_name(name)?;
        ensure!(
            bytes.len() <= max_bytes && max_bytes <= 16 * 1024 * 1024,
            "protected document exceeds its write limit"
        );
        self.with_database(|db| {
            db.connection.execute(
                "INSERT INTO documents(name,revision,body) VALUES(?1,1,?2)
                ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body",
                params![name, bytes],
            )?;
            Ok(())
        })
    }

    pub(crate) fn remove_document(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        self.with_database(|db| {
            db.connection
                .execute("DELETE FROM documents WHERE name=?1", [name])?;
            Ok(())
        })
    }

    pub(crate) fn document_names(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        validate_name(prefix)?;
        ensure!(limit <= 10_000, "protected document listing exceeds bound");
        self.with_database(|db| {
            let mut statement = db.connection.prepare("SELECT name FROM documents WHERE substr(name,1,length(?1))=?1 ORDER BY name LIMIT ?2")?;
            let names = statement.query_map(params![prefix, i64::try_from(limit + 1)?], |row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            ensure!(names.len() <= limit, "protected document listing exceeds bound");
            Ok(names)
        })
    }
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 1024
            && !name.starts_with('/')
            && !name.contains('\\')
            && !name.split('/').any(|part| part == ".." || part == ".")
            && !name.chars().any(char::is_control),
        "invalid managed document name"
    );
    Ok(())
}

/// Transaction-local bounded read for multi-record privacy decisions.
pub(super) fn read(
    db: &rusqlite::Connection,
    name: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    validate_name(name)?;
    let selected:Option<(usize,Option<Vec<u8>>)>=db.query_row(
        "SELECT length(body),CASE WHEN length(body)<=?2 THEN body END FROM documents WHERE name=?1",
        params![name,i64::try_from(max_bytes)?],|r|Ok((read_usize(r,0)?,r.get(1)?))).optional()?;
    let Some((length, body)) = selected else {
        return Ok(None);
    };
    ensure!(
        length <= max_bytes,
        "protected document exceeds its read limit"
    );
    Ok(body)
}
