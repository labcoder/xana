fn main() {
    // A normalized .crate excludes the nested vendored package and its patch.
    // Fail clearly instead of silently linking the registry's older cipher.
    let native = std::path::Path::new("vendor/libsqlite3-sys/sqlcipher/sqlite3.c");
    assert!(
        native.is_file(),
        "Xana requires its complete Git workspace and reviewed native dependency; a standalone .crate is not a supported source distribution"
    );
    println!("cargo:rerun-if-changed={}", native.display());
    println!("cargo:rerun-if-changed=Cargo.toml");
}
