use super::*;
use crate::{
    config::{InitialConfig, InitialConnection, PermissionMode, ProfileUpdate},
    plugin::{PackageSource, PluginManager, PluginScope},
    portable_project::{PortableProfile, PortableProjectStore},
    private_state::ensure_interoperable_records,
    project::ProjectStore,
    shell::ShellConfig,
};
use std::{ffi::OsString, fs};
use tempfile::tempdir;

fn write_plugin(root: &std::path::Path) {
    fs::create_dir_all(root.join("skills/review")).unwrap();
    fs::write(
        root.join("plugin.json"),
        format!(
            r#"{{"$schema":"{}","name":"quality"}}"#,
            crate::plugin::AGENT_PLUGIN_MANIFEST_SCHEMA
        ),
    )
    .unwrap();
    fs::write(
        root.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review safely.\n---\nReview.",
    )
    .unwrap();
}

fn fixture() -> (tempfile::TempDir, XanaPaths) {
    let directory = tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(
        paths.config_file(),
        XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://localhost:11434/v1".into(),
            },
            model: "qwen".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap(),
    )
    .unwrap();
    ensure_interoperable_records(&paths).unwrap();
    (directory, paths)
}

#[test]
fn global_profiles_have_independent_stable_identity_and_full_lifecycle() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let created = store
        .create_global("review".into(), "local".into(), "qwen".into())
        .unwrap();
    let edited = store
        .edit_global(
            "review",
            ProfileUpdate {
                identity: Some(Some("Review carefully.".into())),
                max_tool_rounds: Some(2),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(edited.profile_id, created.profile_id);
    assert_eq!(edited.identity.as_deref(), Some("Review carefully."));

    let duplicate = store.duplicate_global("review", "copy").unwrap();
    assert_ne!(duplicate.profile_id, created.profile_id);
    assert_eq!(duplicate.identity, edited.identity);
    let renamed = store.rename_global("copy", "renamed").unwrap();
    assert_eq!(renamed.profile_id, duplicate.profile_id);
    assert!(store.set_global_archived("renamed", true).unwrap().archived);
    assert_eq!(store.list_global(false).unwrap().len(), 2);
    store.delete_global("renamed").unwrap();
    assert!(store.inspect_global("renamed").is_err());
}

#[test]
fn resolved_profile_is_deterministic_redacted_and_frozen_per_conversation() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let first = store.resolve_global("default").unwrap();
    let second = store.resolve_global("default").unwrap();
    assert_eq!(first, second);
    let snapshot = store.freeze("conversation-one", &first).unwrap();
    assert_eq!(store.freeze("conversation-one", &first).unwrap(), snapshot);

    store
        .create_global("other".into(), "local".into(), "another".into())
        .unwrap();
    let other = store.resolve_global("other").unwrap();
    assert!(matches!(
        store.freeze("conversation-one", &other),
        Err(ProfileError::Immutable(_))
    ));
    let next = store.continue_with("conversation-one", &other).unwrap();
    assert_eq!(
        store.predecessor(&next.to_string()).unwrap().as_deref(),
        Some("conversation-one")
    );
    assert_eq!(
        store
            .snapshot(&next.to_string())
            .unwrap()
            .unwrap()
            .profile_name,
        "other"
    );
}

#[test]
fn global_profile_freezes_exact_locally_enabled_plugin_revision() {
    let (directory, paths) = fixture();
    let source = directory.path().join("plugin");
    write_plugin(&source);
    let manager = PluginManager::open(&paths);
    let spec = PackageSource::Directory(source);
    let review = manager.inspect_source(&spec).unwrap();
    manager.install(&spec, &review.digest).unwrap();
    XanaConfig::set_profile_plugin(paths.config_file(), "default", "quality", true).unwrap();
    manager
        .enable(
            "quality",
            &PluginScope::Profile {
                project: None,
                profile: "default".to_owned(),
            },
        )
        .unwrap();

    let resolved = ProfileStore::open(&paths)
        .resolve_global("default")
        .unwrap();
    assert!(resolved.is_ready());
    assert_eq!(resolved.plugin_revisions["quality"], review.digest);
    let snapshot = ProfileStore::open(&paths)
        .freeze("plugin-conversation", &resolved)
        .unwrap();
    assert_eq!(
        snapshot.resolved["plugin_revisions"]["quality"],
        review.digest
    );
}

#[test]
fn project_profile_resolves_private_binding_and_reports_staleness_separately() {
    let (directory, paths) = fixture();
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let project = ProjectStore::open(&paths)
        .unwrap()
        .create("Portable", &workspace)
        .unwrap();
    PortableProjectStore::share(&project).unwrap();
    let portable = PortableProjectStore::open(&paths);
    portable
        .create_profile(
            &paths,
            project.id,
            "safe",
            PortableProfile {
                profile_id: None,
                archived: false,
                authority_profile: "default".into(),
                connection: "chat".into(),
                model: "qwen".into(),
                reasoning_effort: None,
                reasoning_summary: None,
                identity: Some("Project guidance".into()),
                capabilities: Vec::new(),
                permission_mode: Some(PermissionMode::Deny),
                max_tool_rounds: Some(1),
                orchestration: None,
                skills: Vec::new(),
                plugins: Vec::new(),
                mcp_servers: Vec::new(),
                external_agents: Vec::new(),
                service_routes: Vec::new(),
                egress: Vec::new(),
                applies_to: vec![ProfileUse::Primary],
            },
        )
        .unwrap();
    portable.register(&paths, &workspace).unwrap();
    portable.bind(project.id, "chat", "local").unwrap();
    portable.refresh(&paths, project.id).unwrap();

    let resolved = ProfileStore::open(&paths)
        .resolve_project(&paths, project.id, "safe")
        .unwrap();
    assert!(resolved.is_ready());
    assert_eq!(resolved.connection.value, "local");
    assert_eq!(resolved.permission_mode.value, PermissionMode::Deny);
    assert_eq!(resolved.max_tool_rounds.value, 1);
}
