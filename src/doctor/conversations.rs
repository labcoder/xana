//! Read-only retained-Conversation readiness, separate from new-chat defaults.

use super::{DoctorReport, Finding, Severity};
use crate::{
    config::{ProviderKind, XanaConfig},
    managed::thread_store::ManagedThreadStore,
    model_catalog::ModelManager,
    paths::XanaPaths,
    profile::ProfileStore,
    session::DurableSession,
};
use std::path::Path;

pub(super) async fn inspect(paths: &XanaPaths, report: &mut DoctorReport) {
    let Ok(workspace) = std::env::current_dir().and_then(|path| path.canonicalize()) else {
        return;
    };
    let paths = paths.clone();
    match tokio::task::spawn_blocking(move || inspect_at(&paths, &workspace)).await {
        Ok(Some(finding)) => report.push(finding),
        Ok(None) => {}
        Err(_) => report.push(unavailable()),
    }
}

fn unavailable() -> Finding {
    Finding::new(
        "conversation.profile.unavailable",
        Severity::Warning,
        "retained Conversation readiness could not be inspected",
        "No saved Profile or history was changed; this is not a healthy resume result.",
        Some("xana storage status; xana conversation list".into()),
    )
}

fn inspect_at(paths: &XanaPaths, workspace: &Path) -> Option<Finding> {
    let registry = XanaConfig::load_registry_from(paths.config_file()).ok()?;
    let manager = ModelManager::new(
        registry.clone(),
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    );
    // Selection/config failures already have their own diagnostic. This probe
    // never constructs a WorkspaceHost (which would create coordination files).
    let selection = manager.selected().ok()?;
    let kind = manager.connection(&selection.connection).ok()?.kind;
    let retained = if kind == ProviderKind::Codex {
        ManagedThreadStore::list_for_workspace(paths.data_dir(), workspace)
            .map(|rows| {
                rows.into_iter()
                    .find(|row| row.current && row.connection == selection.connection)
                    .map(|row| row.conversation_id.to_string())
            })
            .map_err(|_| ())
    } else {
        DurableSession::latest_for_workspace(paths.data_dir(), workspace)
            .map(|id| id.map(|id| id.to_string()))
            .map_err(|_| ())
    };
    let id = match retained {
        Ok(Some(id)) => id,
        Ok(None) => return None,
        Err(()) => return Some(unavailable()),
    };
    let store = ProfileStore::open(paths);
    // Older installations can legitimately have journals predating snapshots.
    if !paths.projects_file().exists() && !super::protected_home(paths) {
        return Some(legacy(&id));
    }
    let profile = match store.resolved_snapshot(&id) {
        Ok(Some(profile)) => profile,
        Ok(None) => return Some(legacy(&id)),
        Err(_) => {
            return Some(Finding::new(
                "conversation.profile.invalid",
                Severity::Error,
                "the retained Conversation's frozen Profile is unreadable or incompatible",
                format!(
                    "Conversation {id}; startup cannot safely replace saved authority with current defaults."
                ),
                Some("xana conversation new; preserve the original records for inspection".into()),
            ));
        }
    };
    let (retained, pending) = if kind == ProviderKind::Codex {
        (None, false)
    } else {
        match id
            .parse()
            .ok()
            .map(|id| DurableSession::inspect_execution_configuration(paths.data_dir(), id))
        {
            Some(Ok(value)) => value,
            _ => return Some(unavailable()),
        }
    };
    let prior = retained.as_ref().map_or(&profile, |config| &config.profile);
    let current = match crate::profile::execution::resolve_current(paths, Some(prior)) {
        Ok(current) => current,
        Err(_) => {
            return Some(Finding::new(
                "conversation.profile.defaults_unavailable",
                Severity::Error,
                "settings for the next turn could not be resolved",
                format!(
                    "Conversation {id}; fix configuration and retry this same Conversation. No history was changed."
                ),
                Some("xana profile resolve default; xana model list".into()),
            ));
        }
    };
    if pending {
        let changed = retained.as_ref() != Some(&current);
        return Some(Finding::new(
            if changed { "conversation.profile.pending" } else { "conversation.profile.ready" },
            if changed { Severity::Warning } else { Severity::Info },
            if changed { "unfinished work must be settled before newer settings can apply" }
                else { "unfinished work retains its original execution settings" },
            format!("Conversation {id}; no operation was continued, replayed or changed."),
            Some("Open this Conversation and choose Stop at its round-budget decision, or use xana operation for explicit recovery; then submit the next turn here.".into()),
        ));
    }
    let connection = match manager.connection(&current.profile.connection.value) {
        Ok(connection) => connection,
        Err(_) => return Some(unavailable()),
    };
    if (kind == ProviderKind::Codex) != (connection.kind == ProviderKind::Codex)
        || (kind != ProviderKind::Codex
            && manager
                .descriptor(
                    &current.profile.connection.value,
                    &current.profile.model.value,
                )
                .is_err())
    {
        return Some(Finding::new(
            "conversation.profile.route_unavailable",
            Severity::Error,
            "next-turn route is unavailable or changes execution owner",
            format!(
                "Conversation {id}; restore a compatible model selection. History is retained."
            ),
            Some("xana model list".into()),
        ));
    }
    Some(Finding::new(
        if current.profile != *prior {
            "conversation.profile.update_ready"
        } else {
            "conversation.profile.ready"
        },
        Severity::Ok,
        "the retained Conversation can use current settings without replacing its history",
        format!(
            "Conversation {id}; configuration is adopted at the next safe boundary. Provider reachability and account availability were not tested."
        ),
        None,
    ))
}

fn legacy(id: &str) -> Finding {
    Finding::new(
        "conversation.profile.legacy",
        Severity::Info,
        "the retained Conversation predates frozen Profiles",
        format!(
            "Conversation {id}; first resume freezes the current Profile. Doctor did not create a snapshot."
        ),
        Some("xana conversation new to leave the legacy Conversation untouched".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection, PermissionMode};
    use std::fs;

    #[test]
    fn readiness_is_read_only_and_distinguishes_legacy_and_missing_route() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(
            paths.config_file(),
            XanaConfig::render_initial(InitialConfig {
                connection: InitialConnection::Ollama {
                    name: "fixture".into(),
                    base_url: "http://127.0.0.1:9/v1".into(),
                },
                model: "fixture-model".into(),
                max_tool_rounds: 8,
                shell: crate::shell::ShellConfig::default(),
                permission_mode: PermissionMode::Ask,
                reasoning_effort: None,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(inspect_at(&paths, &workspace).is_none());
        assert!(!paths.data_dir().exists());
        let session = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let id = session.session_id().to_string();
        drop(session);
        assert_eq!(
            inspect_at(&paths, &workspace).unwrap().code,
            "conversation.profile.legacy"
        );
        assert!(!paths.projects_file().exists());
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        let store = ProfileStore::open(&paths);
        let mut frozen = store.resolve_global("default").unwrap();
        frozen.connection.value = "removed-connection".into();
        store.freeze(&id, &frozen).unwrap();
        let before = fs::read(paths.projects_file()).unwrap();
        let finding = inspect_at(&paths, &workspace).unwrap();
        assert_eq!(finding.code, "conversation.profile.update_ready");
        assert_eq!(finding.severity, Severity::Ok);
        assert_eq!(fs::read(paths.projects_file()).unwrap(), before);
        assert!(!paths.data_dir().join("workspace-hosts").exists());
        assert!(!paths.cache_dir().exists());
    }

    #[test]
    fn managed_model_changes_are_not_reported_as_authority_changes() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let mut config: toml_edit::DocumentMut = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Codex {
                name: "codex".into(),
                program: "never-spawn-fixture".into(),
                home: None,
            },
            model: "fixture-model".into(),
            max_tool_rounds: 8,
            shell: crate::shell::ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap()
        .parse()
        .unwrap();
        config["providers"]["codex"]["models"]["fixture-model"]["reasoning"] =
            toml_edit::value(true);
        fs::write(paths.config_file(), config.to_string()).unwrap();
        crate::private_state::ensure_interoperable_records(&paths).unwrap();
        let manager = ModelManager::new(
            XanaConfig::load_registry_from(paths.config_file()).unwrap(),
            paths.cache_dir().to_owned(),
            paths.data_dir().join("selection.toml"),
        );
        let profile_store = ProfileStore::open(&paths);
        let mut profile = profile_store
            .resolve_global_for_selection("default", &manager.selected().unwrap())
            .unwrap();
        profile.model.value = "previous-model".into();
        profile.reasoning_effort.value = Some("low".into());
        profile.reasoning_summary.value = Some("off".into());
        let id = crate::identity::ConversationId::new();
        let mut threads = ManagedThreadStore::open(paths.data_dir(), "codex", &workspace).unwrap();
        threads
            .set_thread(Some(id), Some("fixture-thread".into()), None)
            .unwrap();
        drop(threads);
        profile_store.freeze(&id.to_string(), &profile).unwrap();
        let before = fs::read(paths.projects_file()).unwrap();
        assert_eq!(
            inspect_at(&paths, &workspace).unwrap().code,
            "conversation.profile.update_ready"
        );
        profile_store
            .edit_global(
                "default",
                crate::config::ProfileUpdate {
                    permission_mode: Some(Some(PermissionMode::Allow)),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            inspect_at(&paths, &workspace).unwrap().code,
            "conversation.profile.update_ready"
        );
        assert_eq!(fs::read(paths.projects_file()).unwrap(), before);
        assert!(!paths.cache_dir().exists());
    }
}
