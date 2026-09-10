use super::*;

#[test]
fn creation_reuses_binding_and_chooses_default_only_when_requested() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let before = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    let profile = store
        .create_from_defaults("xana-dev", None, None, false)
        .unwrap();
    let after = XanaConfig::load_registry_from(paths.config_file()).unwrap();
    assert_eq!(after.default_profile, "default");
    assert_eq!(after.connections, before.connections);
    assert_eq!(profile.connection, "local");
    assert_eq!(profile.model, "qwen");
    assert_ne!(profile.profile_id, before.profiles["default"].profile_id);
    store
        .create_from_defaults("personal", None, None, true)
        .unwrap();
    assert_eq!(
        XanaConfig::load_registry_from(paths.config_file())
            .unwrap()
            .default_profile,
        "personal"
    );
    assert!(
        store
            .create_from_defaults("personal", None, None, true)
            .is_err()
    );
}

#[test]
fn removing_first_default_preserves_resources_and_does_not_reroute_children() {
    for archive in [false, true] {
        let (_directory, paths) = fixture();
        let store = ProfileStore::open(&paths);
        let frozen = store.resolve_global("default").unwrap();
        store.freeze("kept-conversation", &frozen).unwrap();
        store
            .create_from_defaults("work", None, None, false)
            .unwrap();
        let before = XanaConfig::load_registry_from(paths.config_file()).unwrap();
        let plan = store.plan_retirement("default", archive, None).unwrap();
        assert_eq!(plan.replacement.as_deref(), Some("work"));
        assert_eq!(plan.removed_routes, ["default"]);
        store.retire(&plan).unwrap();
        let after = XanaConfig::load_registry_from(paths.config_file()).unwrap();
        assert_eq!(after.default_profile, "work");
        assert_eq!(after.connections, before.connections);
        assert_eq!(after.profiles.contains_key("default"), archive);
        assert!(after.routes.is_empty());
        assert!(after.default_child_route.is_none());
        assert_eq!(
            store
                .resolved_snapshot("kept-conversation")
                .unwrap()
                .unwrap(),
            frozen
        );
        assert!(paths.config_file().with_extension("toml.bak").exists());
    }
}

#[test]
fn last_eligible_profile_guard_ignores_archived_and_child_only_profiles() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    assert!(
        store
            .plan_retirement("default", false, None)
            .unwrap_err()
            .to_string()
            .contains("at least one")
    );
    store
        .create_from_defaults("child", None, None, false)
        .unwrap();
    store
        .create_from_defaults("archived", None, None, false)
        .unwrap();
    store.set_global_archived("archived", true).unwrap();
    let mut document = fs::read_to_string(paths.config_file())
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let mut uses = toml_edit::Array::new();
    uses.push("child");
    document["profiles"]["child"]["applies_to"] = toml_edit::value(uses);
    fs::write(paths.config_file(), document.to_string()).unwrap();
    let before = fs::read(paths.config_file()).unwrap();
    for archive in [true, false] {
        assert!(store.plan_retirement("default", archive, None).is_err());
    }
    for name in ["child", "archived", "missing"] {
        assert!(XanaConfig::set_default_profile(paths.config_file(), name).is_err());
    }
    assert_eq!(fs::read(paths.config_file()).unwrap(), before);
}

#[test]
fn replacement_order_is_stable_and_stale_reviews_cannot_commit() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    for name in ["zebra", "alpha", "work"] {
        store.create_from_defaults(name, None, None, false).unwrap();
    }
    let plan = store.plan_retirement("default", false, None).unwrap();
    assert_eq!(plan.candidates, ["work", "zebra", "alpha"]);
    assert_eq!(plan.replacement.as_deref(), Some("work"));
    assert!(
        store
            .plan_retirement("default", false, Some("default"))
            .is_err()
    );
    store
        .edit_global(
            "work",
            ProfileUpdate {
                model: Some("changed".into()),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    let before = fs::read(paths.config_file()).unwrap();
    assert!(
        store
            .retire(&plan)
            .unwrap_err()
            .to_string()
            .contains("changed since")
    );
    assert_eq!(fs::read(paths.config_file()).unwrap(), before);
    let plan = store
        .plan_retirement("default", false, Some("alpha"))
        .unwrap();
    store.retire(&plan).unwrap();
    assert_eq!(
        XanaConfig::load_registry_from(paths.config_file())
            .unwrap()
            .default_profile,
        "alpha"
    );
}

#[test]
fn renamed_legacy_identity_resumes_but_reused_name_never_claims_old_conversation() {
    let (_directory, paths) = fixture();
    let mut document = fs::read_to_string(paths.config_file())
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    document["profiles"]["default"]
        .as_table_mut()
        .unwrap()
        .remove("profile_id");
    fs::write(paths.config_file(), document.to_string()).unwrap();
    let original = execution::resolve_current(&paths, None).unwrap();
    let store = ProfileStore::open(&paths);
    store.rename_global("default", "personal").unwrap();
    let renamed = execution::resolve_current(&paths, Some(&original.profile)).unwrap();
    assert_eq!(renamed.profile.profile_id, original.profile.profile_id);
    assert_eq!(renamed.profile.name, "personal");
    store
        .create_from_defaults("work", None, Some("work-model"), true)
        .unwrap();
    let resumed = execution::resolve_current(&paths, Some(&renamed.profile)).unwrap();
    assert_eq!(resumed.profile.model.value, "qwen");
    store
        .retire(&store.plan_retirement("personal", false, None).unwrap())
        .unwrap();
    store
        .create_from_defaults("personal", None, None, false)
        .unwrap();
    assert!(
        execution::resolve_current(&paths, Some(&original.profile))
            .unwrap_err()
            .is::<execution::RetiredProfile>()
    );
    let view = execution::resolve_for_startup(&paths, Some(&original.profile)).unwrap();
    assert_eq!(view.profile, original.profile);
}

#[test]
fn changing_default_invalidates_old_model_selection_without_touching_that_file() {
    let (_directory, paths) = fixture();
    let selection = paths.data_dir().join("selection.toml");
    let old = "version = 2\nconnection = 'local'\nmodel = 'old-override'\n";
    fs::write(&selection, old).unwrap();
    let store = ProfileStore::open(&paths);
    store
        .create_from_defaults("work", None, Some("work-model"), true)
        .unwrap();
    let manager = crate::model_catalog::ModelManager::new(
        XanaConfig::load_registry_from(paths.config_file()).unwrap(),
        paths.cache_dir().into(),
        selection.clone(),
    );
    assert_eq!(manager.selected().unwrap().model, "work-model");
    assert_eq!(fs::read_to_string(&selection).unwrap(), old);
    manager.select("local", "qwen").unwrap();
    assert_eq!(manager.selected().unwrap().model, "qwen");
}

#[test]
fn invalid_creation_and_duplicate_identity_leave_live_config_unchanged() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let before = fs::read(paths.config_file()).unwrap();
    for (name, connection, model) in [
        ("Invalid Name", None, None),
        ("missing", Some("unknown"), Some("qwen")),
        ("missing", Some("unknown"), None),
    ] {
        assert!(
            store
                .create_from_defaults(name, connection, model, true)
                .is_err()
        );
        assert_eq!(fs::read(paths.config_file()).unwrap(), before);
    }
    let mut document = std::str::from_utf8(&before)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let duplicate = document["profiles"]["default"].clone();
    document["profiles"]["duplicate"] = duplicate;
    assert!(XanaConfig::parse_registry(&document.to_string()).is_err());
}

#[test]
fn managed_profile_reuses_vendor_connection_without_starting_codex() {
    let (_directory, paths) = fixture();
    let source = XanaConfig::render_initial(InitialConfig {
        connection: InitialConnection::Codex {
            name: "codex".into(),
            program: "not-launched-fixture".into(),
            home: None,
        },
        model: "managed-fixture".into(),
        reasoning_effort: Some("medium".into()),
        max_tool_rounds: 8,
        permission_mode: PermissionMode::Ask,
        shell: ShellConfig::default(),
    })
    .unwrap();
    let source = crate::config::profiles::name_initial_profile(&source, "personal").unwrap();
    fs::write(paths.config_file(), source).unwrap();
    let store = ProfileStore::open(&paths);
    let created = store
        .create_from_defaults("work", None, None, true)
        .unwrap();
    assert_eq!(created.connection, "codex");
    assert_eq!(created.model, "managed-fixture");
    assert_eq!(created.reasoning_effort.as_deref(), Some("medium"));
    let other = store
        .create_from_defaults("other-model", None, Some("different-model"), false)
        .unwrap();
    assert_eq!(other.reasoning_effort, None);
    assert_eq!(other.reasoning_summary, None);
    assert_eq!(
        XanaConfig::load_registry_from(paths.config_file())
            .unwrap()
            .connections
            .len(),
        1
    );
}

#[tokio::test]
async fn managed_new_turn_guard_rejects_retirement_without_a_vendor_call() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let guard = std::sync::Arc::new(crate::managed_execution::ManagedProfileGuard {
        paths: paths.clone(),
        profile: store.resolve_global("default").unwrap(),
    });
    guard.clone().validate().await.unwrap();
    store
        .create_from_defaults("work", None, None, false)
        .unwrap();
    store
        .retire(&store.plan_retirement("default", true, None).unwrap())
        .unwrap();
    assert!(
        guard
            .validate()
            .await
            .unwrap_err()
            .contains("no Codex turn was sent")
    );
}

#[test]
fn retired_model_controls_cannot_claim_a_reused_profile_name() {
    let (_directory, paths) = fixture();
    let store = ProfileStore::open(&paths);
    let retired = store
        .create_from_defaults("personal", None, None, false)
        .unwrap();
    store.delete_global("personal").unwrap();
    let replacement = store
        .create_from_defaults("personal", None, None, false)
        .unwrap();
    assert_ne!(retired.profile_id, replacement.profile_id);
    let make_manager = || {
        crate::model_catalog::ModelManager::new(
            XanaConfig::load_registry_from(paths.config_file()).unwrap(),
            paths.cache_dir().into(),
            paths.data_dir().join("selection.toml"),
        )
    };
    make_manager()
        .for_profile("personal", replacement.profile_id)
        .select("local", "qwen")
        .unwrap();
    let before = fs::read(paths.data_dir().join("selection.toml")).unwrap();
    let retired_manager = make_manager().for_profile("personal", retired.profile_id);
    assert!(retired_manager.selected().is_err());
    assert!(retired_manager.select("local", "qwen").is_err());
    assert_eq!(
        fs::read(paths.data_dir().join("selection.toml")).unwrap(),
        before
    );
}

#[test]
fn default_promotion_preserves_old_profile_override_without_leaking_it_to_new_profiles() {
    let (_directory, paths) = fixture();
    // The override stays in the connection catalog after its configured profile changes.
    let source = fs::read_to_string(paths.config_file()).unwrap();
    fs::write(
        paths.config_file(),
        format!("{source}\n[providers.local.models.qwen]\n"),
    )
    .unwrap();
    let store = ProfileStore::open(&paths);
    let manager = crate::model_catalog::ModelManager::new(
        XanaConfig::load_registry_from(paths.config_file()).unwrap(),
        paths.cache_dir().into(),
        paths.data_dir().join("selection.toml"),
    );
    manager.select("local", "qwen").unwrap();
    store
        .edit_global(
            "default",
            ProfileUpdate {
                model: Some("configured-model".into()),
                ..ProfileUpdate::default()
            },
        )
        .unwrap();
    store
        .create_from_defaults("work", None, Some("work-model"), true)
        .unwrap();
    let manager = crate::model_catalog::ModelManager::new(
        XanaConfig::load_registry_from(paths.config_file()).unwrap(),
        paths.cache_dir().into(),
        paths.data_dir().join("selection.toml"),
    );
    assert_eq!(manager.selected().unwrap().model, "work-model");
    assert_eq!(
        manager.selected_for_profile("default").unwrap().model,
        "qwen"
    );
    manager.select("local", "work-model").unwrap();
    assert_eq!(
        manager.selected_for_profile("default").unwrap().model,
        "configured-model"
    );
    let before = fs::read(paths.data_dir().join("selection.toml")).unwrap();
    assert!(
        manager
            .for_profile("missing", uuid::Uuid::new_v4())
            .select("local", "qwen")
            .is_err()
    );
    assert_eq!(
        fs::read(paths.data_dir().join("selection.toml")).unwrap(),
        before
    );
}
