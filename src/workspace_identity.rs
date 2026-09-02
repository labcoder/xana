//! Filesystem-backed identity for local workspace collision domains.
//!
//! Canonical paths remain useful display and containment values, but path text
//! does not establish cross-process ownership: symlinks, junctions, case
//! aliases, and Windows path-prefix variants may name the same directory. This
//! module resolves both values once and derives the coordination key from the
//! opened filesystem object.

use same_file::Handle;
use std::{
    hash::{Hash, Hasher},
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
};

const IDENTITY_DOMAIN: &[u8] = b"xana-workspace-filesystem-identity-v1";

#[derive(Debug)]
pub(crate) struct WorkspaceIdentity {
    canonical_path: PathBuf,
    collision_key: String,
}

pub(crate) fn next_locked_generation(lock: &std::fs::File) -> io::Result<u64> {
    let mut file = lock.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut file).take(32).read_to_end(&mut bytes)?;
    let current = if bytes.is_empty() {
        0
    } else {
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid owner generation"))?
    };
    let generation = current
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "owner generation exhausted"))?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    writeln!(file, "{generation}")?;
    file.sync_data()?;
    Ok(generation)
}

impl WorkspaceIdentity {
    pub(crate) fn resolve(path: &Path) -> io::Result<Self> {
        let canonical_path = path.canonicalize()?;
        let handle = Handle::from_path(&canonical_path)?;
        let mut hasher = Blake3HashWriter::new();
        IDENTITY_DOMAIN.hash(&mut hasher);
        handle.hash(&mut hasher);
        Ok(Self {
            canonical_path,
            collision_key: hasher.finalize(),
        })
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn collision_key(&self) -> &str {
        &self.collision_key
    }

    pub(crate) fn matches(&self, other: &Path) -> io::Result<bool> {
        same_file::is_same_file(&self.canonical_path, other)
    }
}

struct Blake3HashWriter(blake3::Hasher);

impl Blake3HashWriter {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    fn finalize(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

impl Hasher for Blake3HashWriter {
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight bytes"))
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(&(bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
    }

    fn write_u8(&mut self, value: u8) {
        self.write(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn write_u128(&mut self, value: u128) {
        self.write(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write(&(value as u64).to_le_bytes());
    }

    fn write_i8(&mut self, value: i8) {
        self.write(&value.to_le_bytes());
    }

    fn write_i16(&mut self, value: i16) {
        self.write(&value.to_le_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    fn write_i128(&mut self, value: i128) {
        self.write(&value.to_le_bytes());
    }

    fn write_isize(&mut self, value: isize) {
        self.write(&(value as i64).to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn spelling_aliases_share_one_filesystem_collision_key() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();

        let direct = WorkspaceIdentity::resolve(&workspace).unwrap();
        let dotted = WorkspaceIdentity::resolve(&workspace.join(".")).unwrap();

        assert_eq!(direct.collision_key(), dotted.collision_key());
        assert!(direct.matches(dotted.canonical_path()).unwrap());
    }

    #[test]
    fn locked_generations_are_monotonic_and_reject_corruption() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("owner.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        file.try_lock().unwrap();

        assert_eq!(next_locked_generation(&file).unwrap(), 1);
        assert_eq!(next_locked_generation(&file).unwrap(), 2);
        let mut corrupt = file.try_clone().unwrap();
        corrupt.seek(SeekFrom::Start(0)).unwrap();
        corrupt.set_len(0).unwrap();
        corrupt.write_all(b"not-a-generation").unwrap();
        corrupt.sync_data().unwrap();
        assert_eq!(
            next_locked_generation(&file).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_one_filesystem_collision_key() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let alias = directory.path().join("workspace-alias");
        fs::create_dir(&workspace).unwrap();
        symlink(&workspace, &alias).unwrap();

        let direct = WorkspaceIdentity::resolve(&workspace).unwrap();
        let linked = WorkspaceIdentity::resolve(&alias).unwrap();

        assert_eq!(direct.collision_key(), linked.collision_key());
        assert!(direct.matches(&alias).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefix_and_case_aliases_share_one_filesystem_collision_key() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("WorkspaceIdentity");
        fs::create_dir(&workspace).unwrap();
        let canonical = workspace.canonicalize().unwrap();
        let canonical_text = canonical.to_string_lossy();
        let ordinary = PathBuf::from(
            canonical_text
                .strip_prefix(r"\\?\")
                .unwrap_or(canonical_text.as_ref()),
        );
        let case_alias = PathBuf::from(ordinary.to_string_lossy().to_ascii_uppercase());

        let extended = WorkspaceIdentity::resolve(&canonical).unwrap();
        let cased = WorkspaceIdentity::resolve(&case_alias).unwrap();

        assert_eq!(extended.collision_key(), cased.collision_key());
        assert!(extended.matches(&ordinary).unwrap());
    }
}
