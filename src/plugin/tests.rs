use super::*;
use crate::{paths::XanaPaths, private_state::PackageStateDocument};
use std::{ffi::OsString, fs, path::Path, process::Command};

fn paths() -> (tempfile::TempDir, XanaPaths) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
    (directory, paths)
}

fn write_plugin(root: &Path, name: &str) {
    fs::create_dir_all(root.join("skills/review/references")).unwrap();
    fs::write(
        root.join("plugin.json"),
        format!(
            r#"{{"$schema":"{AGENT_PLUGIN_MANIFEST_SCHEMA}","name":"{name}","version":"1.2.3"}}"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review a change.\n---\nUse [rules](references/rules.md).",
    )
    .unwrap();
    fs::write(
        root.join("skills/review/references/rules.md"),
        "Stay bounded.",
    )
    .unwrap();
    fs::write(
        root.join("mcp.json"),
        format!(
            r#"{{
              "$schema":"{AGENT_PLUGIN_MCP_SCHEMA}",
              "mcpServers":{{
                "local":{{"type":"stdio","command":"helper","args":["--quiet"],"env":{{"MODE":"safe"}}}},
                "remote":{{"type":"streamable-http","url":"https://example.com/mcp","headers":{{"X-Public":"value"}}}},
                "legacy":{{"type":"sse","url":"https://example.com/events"}}
              }}
            }}"#
        ),
    )
    .unwrap();
}

#[test]
fn inspection_reports_supported_components_without_executing_them() {
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "quality-tools");
    let (_home, paths) = paths();

    let review = PluginManager::open(&paths)
        .inspect_source(&PackageSource::Directory(source.path().to_owned()))
        .unwrap();

    assert_eq!(review.manifest.name, "quality-tools");
    assert_eq!(review.skills, ["review"]);
    assert_eq!(
        review
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>(),
        ["local", "remote"]
    );
    assert!(
        review
            .diagnostics
            .iter()
            .any(|item| item.contains("legacy"))
    );
    assert_eq!(review.mcp_servers[0].environment_names, ["MODE"]);
}

#[test]
fn unknown_manifest_fields_are_nonfatal_and_other_schema_errors_are_fatal() {
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "quality-tools");
    fs::write(
        source.path().join("plugin.json"),
        format!(
            r#"{{"$schema":"{AGENT_PLUGIN_MANIFEST_SCHEMA}","name":"quality-tools","mystery":"ignored","extensions":"ignored"}}"#
        ),
    )
    .unwrap();
    let (_home, paths) = paths();
    let manager = PluginManager::open(&paths);
    let review = manager
        .inspect_source(&PackageSource::Directory(source.path().to_owned()))
        .unwrap();
    assert_eq!(review.diagnostics.len(), 3); // legacy SSE plus two manifest diagnostics

    fs::write(
        source.path().join("plugin.json"),
        format!(r#"{{"$schema":"{AGENT_PLUGIN_MANIFEST_SCHEMA}","name":7}}"#),
    )
    .unwrap();
    assert!(matches!(
        manager.inspect_source(&PackageSource::Directory(source.path().to_owned())),
        Err(PluginError::Json { .. })
    ));
}

#[test]
fn immutable_install_is_disabled_idempotent_and_content_addressed() {
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "quality-tools");
    let (_home, paths) = paths();
    let manager = PluginManager::open(&paths);
    let spec = PackageSource::Directory(source.path().to_owned());

    let review = manager.inspect_source(&spec).unwrap();
    let first = manager.install(&spec, &review.digest).unwrap();
    let second = manager.install(&spec, &review.digest).unwrap();

    assert_eq!(first.active_revision, second.active_revision);
    assert!(first.enabled_scopes.is_empty());
    assert!(!first.mutable);
    assert!(first.root.starts_with(paths.package_store_dir()));
    assert!(first.root.join("plugin.json").is_file());
    let state: PackageStateDocument =
        crate::private_state::read_document(&paths.package_state_file()).unwrap();
    assert_eq!(state.plugins.len(), 1);
    assert_eq!(state.plugins["quality-tools"].revisions.len(), 1);

    fs::write(
        source.path().join("skills/review/references/rules.md"),
        "Changed source.",
    )
    .unwrap();
    assert!(matches!(
        manager.install(&spec, &review.digest),
        Err(PluginError::ReviewChanged { .. })
    ));
    assert_eq!(
        fs::read_to_string(first.root.join("skills/review/references/rules.md")).unwrap(),
        "Stay bounded."
    );
}

#[test]
fn linked_install_is_explicitly_mutable_and_never_copied() {
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "linked-tools");
    let expected_root = source.path().canonicalize().unwrap();
    let (_home, paths) = paths();
    let manager = PluginManager::open(&paths);
    let source_spec = PackageSource::Linked(source.path().to_owned());
    let review = manager.inspect_source(&source_spec).unwrap();
    let installed = manager.install(&source_spec, &review.digest).unwrap();

    assert!(installed.mutable);
    assert_eq!(installed.root, expected_root);
    assert!(installed.enabled_scopes.is_empty());
}

#[test]
fn invalid_plugin_never_appears_installed() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("plugin.json"), b"not json").unwrap();
    let (_home, paths) = paths();
    let manager = PluginManager::open(&paths);

    assert!(
        manager
            .inspect_source(&PackageSource::Directory(source.path().to_owned()))
            .is_err()
    );
    assert!(manager.list().unwrap().is_empty());
}

#[test]
fn pinned_git_install_records_exact_revision() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let repository = tempfile::tempdir().unwrap();
    run_git(repository.path(), &["init"]);
    run_git(repository.path(), &["config", "user.name", "Xana Tests"]);
    run_git(
        repository.path(),
        &["config", "user.email", "xana@example.invalid"],
    );
    write_plugin(repository.path(), "git-tools");
    run_git(repository.path(), &["add", "."]);
    run_git(repository.path(), &["commit", "-m", "fixture"]);
    let revision = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let (_home, paths) = paths();
    let manager = PluginManager::open(&paths);
    let source = PackageSource::Git {
        url: repository.path().to_string_lossy().into_owned(),
        revision: revision.clone(),
    };
    let review = manager.inspect_source(&source).unwrap();
    let installed = manager.install(&source, &review.digest).unwrap();

    assert_eq!(
        installed.source.revision.as_deref(),
        Some(revision.as_str())
    );
    assert!(!installed.mutable);
}

fn run_git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
#[test]
fn symlink_entries_are_rejected() {
    use std::os::unix::fs::symlink;
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "unsafe-tools");
    symlink("plugin.json", source.path().join("alias.json")).unwrap();
    let (_home, paths) = paths();
    assert!(matches!(
        PluginManager::open(&paths)
            .inspect_source(&PackageSource::Directory(source.path().to_owned())),
        Err(PluginError::UnsafePath(_))
    ));
}

#[cfg(windows)]
#[test]
fn symlink_entries_are_rejected_when_available() {
    use std::os::windows::fs::symlink_file;
    let source = tempfile::tempdir().unwrap();
    write_plugin(source.path(), "unsafe-tools");
    if symlink_file("plugin.json", source.path().join("alias.json")).is_err() {
        return;
    }
    let (_home, paths) = paths();
    assert!(matches!(
        PluginManager::open(&paths)
            .inspect_source(&PackageSource::Directory(source.path().to_owned())),
        Err(PluginError::UnsafePath(_))
    ));
}
