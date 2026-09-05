//! Native-library contract: exercise the actual linked cipher, not a mock.

use rusqlite::Connection;
use sha2::{Digest, Sha256};

#[test]
fn reviewed_amalgamation_has_not_drifted() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/libsqlite3-sys/sqlcipher");
    for (file, expected) in [
        (
            "sqlite3.c",
            "2ae782aed6f15ae3fd13e508f0e2d13a1182c507cc790bcdfb9097b956f9e4e3",
        ),
        (
            "sqlite3.h",
            "3b5107efc25741380db6aff27a57510837d85024ac87de9cca3e1dc069bd2cee",
        ),
        (
            "sqlite3ext.h",
            "a3ca6e430c8e97edf8cbd66867ac178ab179a41d85c04cad48889a8b84806dcd",
        ),
    ] {
        let bytes = std::fs::read(root.join(file)).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected, "{file}");
    }
}

#[test]
fn encrypted_wal_and_fts_do_not_spill_content() {
    let home = tempfile::tempdir().unwrap();
    let db = Connection::open(home.path().join("canary.sqlite")).unwrap();
    db.pragma_update(None, "key", "synthetic-key").unwrap();
    db.pragma_update(None, "temp_store", "MEMORY").unwrap();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.pragma_update(None, "synchronous", "FULL").unwrap();
    db.execute_batch("CREATE VIRTUAL TABLE search USING fts5(body); INSERT INTO search VALUES('secret-marker-never-in-plaintext');").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM search WHERE search MATCH 'marker'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    for entry in std::fs::read_dir(home.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(b"secret-marker".len())
                .any(|b| b == b"secret-marker")
        );
    }
}

#[test]
fn encrypted_records_require_the_reviewed_cipher_and_correct_key() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("canary.sqlite");
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "key", "synthetic-test-key-not-a-user-secret")
        .unwrap();
    let cipher: String = db
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .unwrap();
    assert!(
        cipher.starts_with("4.18.0"),
        "unreviewed SQLCipher: {cipher}"
    );
    let sqlite: String = db
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sqlite, "3.53.4");
    db.execute_batch(
        "CREATE TABLE content(value TEXT); INSERT INTO content VALUES('protected canary');",
    )
    .unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !bytes
            .windows(b"protected canary".len())
            .any(|b| b == b"protected canary")
    );
    let wrong = Connection::open(&path).unwrap();
    wrong
        .pragma_update(None, "key", "different-test-key")
        .unwrap();
    assert!(
        wrong
            .query_row("SELECT value FROM content", [], |r| r.get::<_, String>(0))
            .is_err()
    );
    drop(wrong);
    let reopened = Connection::open(&path).unwrap();
    reopened
        .pragma_update(None, "key", "synthetic-test-key-not-a-user-secret")
        .unwrap();
    assert_eq!(
        reopened
            .query_row("SELECT value FROM content", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "protected canary"
    );
}
