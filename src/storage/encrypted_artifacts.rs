//! Standard age streaming objects with whole-content authentication on every read.

use super::{
    ProtectedStore,
    database::{Database, read_u64},
};
use crate::artifact::ContentHash;
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;
use zeroize::Zeroizing;

const CHUNK: usize = 64 * 1024;

impl ProtectedStore {
    pub(crate) fn put_artifact(
        &self,
        input: &mut dyn Read,
        max_bytes: usize,
        validate_source: impl FnOnce(u64) -> Result<()>,
    ) -> Result<(ContentHash, u64, bool)> {
        ensure!(
            max_bytes > 0 && max_bytes <= crate::resource::MAX_RESOURCE_SOURCE_BYTES,
            "invalid protected artifact bound"
        );
        self.with_database(|db| {
            let root = self.inner.root.join("objects");
            fs::create_dir_all(&root)?;
            let file_id = Uuid::new_v4();
            let pending = root.join(format!("{file_id}.pending"));
            let final_path = root.join(format!("{file_id}.age"));
            let result = (|| {
                let file = fs::File::create_new(&pending)?;
                fs2::FileExt::try_lock_exclusive(&file)?;
                let recipient = db.secrets.artifacts.to_public();
                let encryptor = age::Encryptor::with_recipients(std::iter::once(
                    &recipient as &dyn age::Recipient,
                ))?;
                let mut writer = encryptor.wrap_output(file)?;
                let mut chunk = Zeroizing::new([0u8; CHUNK]);
                let mut hash = blake3::Hasher::new();
                let mut length = 0usize;
                loop {
                    let read = input.read(&mut *chunk)?;
                    if read == 0 {
                        break;
                    }
                    length = length.checked_add(read).context("artifact size overflow")?;
                    ensure!(length <= max_bytes, "artifact exceeds source bound");
                    hash.update(&chunk[..read]);
                    writer.write_all(&chunk[..read])?;
                }
                writer.finish()?.sync_all()?;
                validate_source(length as u64)?;
                let content_hash = ContentHash::parse(hash.finalize().to_hex().to_string())
                    .map_err(anyhow::Error::msg)?;
                let existing: Option<(String, u64)> = db
                    .connection
                    .query_row(
                        "SELECT file_id,length FROM encrypted_artifacts WHERE hash=?1",
                        [content_hash.as_str()],
                        |row| Ok((row.get(0)?, read_u64(row, 1)?)),
                    )
                    .optional()?;
                if let Some((existing_id, existing_len)) = existing {
                    ensure!(
                        existing_len == length as u64,
                        "existing artifact length differs"
                    );
                    let existing_path = object_path(&root, &existing_id)?;
                    verified_range(db, &existing_path, &content_hash, existing_len, 0, 0)?;
                    return Ok((content_hash, length as u64, false));
                }
                fs::hard_link(&pending, &final_path)?;
                if let Err(error) = db.connection.execute(
                    "INSERT INTO encrypted_artifacts VALUES(?1,?2,?3)",
                    params![
                        content_hash.as_str(),
                        file_id.to_string(),
                        i64::try_from(length)?
                    ],
                ) {
                    let _ = fs::remove_file(&final_path);
                    return Err(error.into());
                }
                Ok((content_hash, length as u64, true))
            })();
            // Only our own unique staging name is removed; never other processes' files.
            let _ = fs::remove_file(&pending);
            result
        })
    }

    pub(crate) fn read_artifact(
        &self,
        hash: &ContentHash,
        declared: u64,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        ensure!(
            declared <= crate::resource::MAX_RESOURCE_SOURCE_BYTES as u64 && offset <= declared,
            "invalid artifact declaration or range"
        );
        self.with_database(|db| {
            let (file_id, actual): (String, u64) = db
                .connection
                .query_row(
                    "SELECT file_id,length FROM encrypted_artifacts WHERE hash=?1",
                    [hash.as_str()],
                    |row| Ok((row.get(0)?, read_u64(row, 1)?)),
                )
                .optional()?
                .context("protected artifact is unavailable")?;
            ensure!(
                declared == actual,
                "protected artifact registration length differs"
            );
            let path = object_path(&self.inner.root.join("objects"), &file_id)?;
            verified_range(db, &path, hash, actual, offset, length)
        })
    }

    /// The caller owns an explicit export destination and removes its partial
    /// file on failure. No managed plaintext mirror is created by this store.
    pub(crate) fn export_artifact(
        &self,
        hash: &ContentHash,
        declared: u64,
        output: &mut dyn Write,
    ) -> Result<()> {
        ensure!(
            declared <= crate::resource::MAX_RESOURCE_SOURCE_BYTES as u64,
            "invalid artifact declaration"
        );
        self.with_database(|db| {
            let (file_id, actual): (String, u64) = db.connection.query_row(
                "SELECT file_id,length FROM encrypted_artifacts WHERE hash=?1",
                [hash.as_str()],
                |row| Ok((row.get(0)?, read_u64(row, 1)?)),
            )?;
            ensure!(
                actual == declared,
                "protected artifact registration length differs"
            );
            let path = object_path(&self.inner.root.join("objects"), &file_id)?;
            verified_chunks(db, &path, hash, actual, |_, chunk| {
                Ok(output.write_all(chunk)?)
            })
        })
    }
}

fn object_path(root: &Path, id: &str) -> Result<std::path::PathBuf> {
    let id: Uuid = id.parse().context("invalid encrypted object identity")?;
    Ok(root.join(format!("{id}.age")))
}

fn verified_range(
    db: &Database,
    path: &Path,
    expected_hash: &ContentHash,
    declared: u64,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>> {
    let end = offset
        .checked_add(length as u64)
        .context("artifact range overflow")?
        .min(declared);
    let mut output = Vec::with_capacity(usize::try_from(end - offset)?);
    verified_chunks(db, path, expected_hash, declared, |begin, chunk| {
        let copy_start = offset.max(begin);
        let copy_end = end.min(begin + chunk.len() as u64);
        if copy_start < copy_end {
            output.extend_from_slice(
                &chunk[(copy_start - begin) as usize..(copy_end - begin) as usize],
            );
        }
        Ok(())
    })?;
    Ok(output)
}

fn verified_chunks(
    db: &Database,
    path: &Path,
    expected_hash: &ContentHash,
    declared: u64,
    mut consume: impl FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "encrypted artifact is not a regular file"
    );
    let file = fs::File::open(path)?;
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let metadata = file.metadata()?;
    let ciphertext_len = metadata.len();
    // age framing is small but grows with chunks; reject unbounded trailing data.
    let max_ciphertext = declared
        .checked_add(declared / CHUNK as u64 * 16)
        .and_then(|n| n.checked_add(CHUNK as u64))
        .context("encrypted artifact size overflow")?;
    ensure!(
        ciphertext_len <= max_ciphertext,
        "encrypted artifact exceeds framing bounds"
    );
    let decryptor = age::Decryptor::new(file)?;
    let mut reader =
        decryptor.decrypt(std::iter::once(&db.secrets.artifacts as &dyn age::Identity))?;
    let mut chunk = Zeroizing::new([0u8; CHUNK]);
    let mut hasher = blake3::Hasher::new();
    let mut actual = 0u64;
    loop {
        let read = reader
            .read(&mut *chunk)
            .context("could not authenticate encrypted artifact")?;
        if read == 0 {
            break;
        }
        let begin = actual;
        actual = actual
            .checked_add(read as u64)
            .context("artifact length overflow")?;
        ensure!(
            actual <= declared,
            "decrypted artifact exceeds declared length"
        );
        hasher.update(&chunk[..read]);
        consume(begin, &chunk[..read])?;
    }
    ensure!(
        actual == declared && hasher.finalize().to_hex().as_str() == expected_hash.as_str(),
        "artifact identity verification failed"
    );
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file()
            && same_file::Handle::from_path(path)? == identity
            && fs::metadata(path)?.len() == ciphertext_len
            && fs::metadata(path)?.modified()? == metadata.modified()?,
        "encrypted artifact changed during verification"
    );
    Ok(())
}
