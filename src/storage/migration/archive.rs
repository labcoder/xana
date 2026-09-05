//! Explicit inspection/export of derived files retained during migration.

use super::{Entry, ProtectedStore};
use crate::artifact::ContentHash;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{fs, path::Path};

#[derive(Serialize)]
pub(crate) struct ArchivedFile {
    pub id: String,
    pub name: String,
    pub bytes: u64,
}

pub(crate) fn list(store: &ProtectedStore, after: Option<&str>) -> Result<Vec<ArchivedFile>> {
    let after = after.unwrap_or("");
    ensure!(
        after.is_empty() || ContentHash::parse(after.to_owned()).is_ok(),
        "invalid archive cursor"
    );
    let names = store.with_database(|db| {
        let mut query = db.connection.prepare("SELECT name FROM documents WHERE name GLOB 'migration/archive/*' AND name > ?1 ORDER BY name LIMIT 128")?;
        Ok(query.query_map([format!("migration/archive/{after}")], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    })?;
    names
        .into_iter()
        .map(|name| {
            let bytes = store
                .document(&name, 4096)?
                .context("archive entry unavailable")?;
            let (entry, _hash, length): (Entry, ContentHash, u64) = serde_json::from_slice(&bytes)?;
            Ok(ArchivedFile {
                id: name
                    .strip_prefix("migration/archive/")
                    .context("invalid archive name")?
                    .into(),
                name: entry.name,
                bytes: length,
            })
        })
        .collect()
}

pub(crate) fn export(store: &ProtectedStore, id: &str, destination: &Path) -> Result<()> {
    ensure!(
        ContentHash::parse(id.to_owned()).is_ok(),
        "invalid archive id"
    );
    let bytes = store
        .document(&format!("migration/archive/{id}"), 4096)?
        .context("archive entry unavailable")?;
    let (_entry, hash, length): (Entry, ContentHash, u64) = serde_json::from_slice(&bytes)?;
    let mut output = super::super::create_private_file(destination)
        .context("export requires a new explicit destination")?;
    let identity = same_file::Handle::from_file(output.try_clone()?)?;
    let result = store
        .export_artifact(&hash, length, &mut output)
        .and_then(|()| Ok(output.sync_all()?));
    drop(output);
    if result.is_err()
        && same_file::Handle::from_path(destination).is_ok_and(|current| current == identity)
    {
        fs::remove_file(destination)?;
    }
    result
}
