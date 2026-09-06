use super::*;
use crate::{
    autonomy::{Action, Schedule},
    config::{InitialConfig, InitialConnection, PermissionMode, ProfileUpdate, XanaConfig},
    profile::ProfileStore,
    shell::ShellConfig,
    storage::{RecoveryIdentity, TestCustody},
};
use std::fs;

// Inject a disposable protected owner; do not install keys in OS custody or
// modify process-wide recovery variables just to exercise the Desktop seam.
fn fixture() -> (
    tempfile::TempDir,
    XanaPaths,
    ProtectedStore,
    DesktopTaskDraft,
) {
    let directory = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
    fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    fs::write(
        paths.config_file(),
        XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
            },
            model: "fixture-model".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap(),
    )
    .unwrap();
    crate::private_state::ensure_interoperable_records(&paths).unwrap();
    ProfileStore::open(&paths)
        .create_global("fixture".into(), "local".into(), "fixture-model".into())
        .unwrap();
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(workspace.join("source")).unwrap();
    fs::create_dir_all(paths.runtime_dir()).unwrap();
    let store = ProtectedStore::initialize(
        &directory.path().join("vault"),
        &RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let draft = DesktopTaskDraft {
        name: "Scoped fixture".into(),
        workspace: workspace.display().to_string(),
        profile: "fixture".into(),
        project: None,
        text: "Inspect the authorized change".into(),
        reminder: false,
        workspace_reads: true,
        trigger: DesktopTaskTrigger::Files {
            root: "source".into(),
        },
        expires: "2040-01-01T00:00:00Z".into(),
    };
    (directory, paths, store, draft)
}

#[test]
fn desktop_file_and_ci_drafts_use_shared_preparation_and_protected_creation() {
    let (_directory, paths, store, mut draft) = fixture();
    for trigger in [
        DesktopTaskTrigger::Files {
            root: "source".into(),
        },
        DesktopTaskTrigger::GithubRun {
            run: "fixture/project/42".into(),
            credential: DesktopGithubCredential::Environment {
                variable: "XANA_TEST_UNRESOLVED_TOKEN".into(),
            },
        },
        DesktopTaskTrigger::GithubRun {
            run: "fixture/project/43".into(),
            credential: DesktopGithubCredential::Stored {
                id: "not-installed-token".into(),
            },
        },
    ] {
        draft.trigger = trigger.clone();
        let before = store.autonomy_page(0).unwrap().len();
        let job = prepare_draft(&paths, &store, &draft).unwrap();
        assert_eq!(
            store.autonomy_page(0).unwrap().len(),
            before,
            "preview must not create work"
        );
        assert!(matches!(
            job.action,
            Action::NativeTask {
                workspace_reads: true,
                ..
            }
        ));
        let reviewed = preview(&job).unwrap();
        assert!(reviewed.text.contains("Inspect the authorized change"));
        match &job.trigger {
            Some(Trigger::Files(watch)) => {
                assert_eq!(
                    watch.root,
                    fs::canonicalize(std::path::Path::new(&draft.workspace).join("source"))
                        .unwrap()
                );
                assert_eq!(job.schedule, Schedule::Triggered { poll_seconds: 5 });
                assert!(reviewed.text.contains("selected_files"));
            }
            Some(Trigger::Github(watch)) => {
                assert_eq!(watch.repository, "fixture/project");
                assert!(matches!(watch.run, 42 | 43));
                assert_eq!(job.schedule, Schedule::Triggered { poll_seconds: 60 });
                assert!(reviewed.text.contains("credential_source"));
                assert!(reviewed.text.contains("api.github.com"));
            }
            None => panic!("Desktop discarded the selected source"),
        }
        let saved = commit_reviewed(&store, job, &reviewed).unwrap();
        assert_eq!(store.autonomy_job(saved.id).unwrap(), saved);
        assert_eq!(store.autonomy_page(0).unwrap().len(), before + 1);
    }
}

#[test]
fn desktop_trigger_validation_rejects_unsafe_or_invalid_sources_before_saving() {
    let (directory, paths, store, mut draft) = fixture();
    for trigger in [
        DesktopTaskTrigger::Files {
            root: directory.path().display().to_string(),
        },
        DesktopTaskTrigger::GithubRun {
            run: "https://github.com/fixture/project/actions/runs/42".into(),
            credential: DesktopGithubCredential::Environment {
                variable: "TOKEN".into(),
            },
        },
        DesktopTaskTrigger::GithubRun {
            run: "fixture/project/0".into(),
            credential: DesktopGithubCredential::Stored { id: "token".into() },
        },
        DesktopTaskTrigger::GithubRun {
            run: "fixture/project/42".into(),
            credential: DesktopGithubCredential::Environment {
                variable: "".into(),
            },
        },
        DesktopTaskTrigger::Daily {
            time: "25:00".into(),
            timezone: "UTC".into(),
        },
        DesktopTaskTrigger::Once {
            at: "not-a-timestamp".into(),
        },
    ] {
        draft.trigger = trigger;
        assert!(prepare_draft(&paths, &store, &draft).is_err());
    }
    draft.trigger = DesktopTaskTrigger::Files {
        root: "x".repeat(4097),
    };
    assert!(creation_args(&draft).is_err());
    assert!(store.autonomy_page(0).unwrap().is_empty());
}

#[test]
fn exact_review_fences_action_route_credential_and_source_identity_changes() {
    let (_directory, paths, store, mut draft) = fixture();
    let original = prepare_draft(&paths, &store, &draft).unwrap();
    let reviewed = preview(&original).unwrap();
    draft.text = "Different task".into();
    assert!(
        commit_reviewed(
            &store,
            prepare_draft(&paths, &store, &draft).unwrap(),
            &reviewed
        )
        .is_err()
    );
    draft.text = "Inspect the authorized change".into();
    let source = std::path::Path::new(&draft.workspace).join("source");
    fs::rename(&source, source.with_file_name("previous-source")).unwrap();
    fs::create_dir(&source).unwrap();
    assert!(
        commit_reviewed(
            &store,
            prepare_draft(&paths, &store, &draft).unwrap(),
            &reviewed
        )
        .is_err()
    );
    draft.trigger = DesktopTaskTrigger::GithubRun {
        run: "fixture/project/42".into(),
        credential: DesktopGithubCredential::Environment {
            variable: "FIRST_TOKEN".into(),
        },
    };
    let ci = preview(&prepare_draft(&paths, &store, &draft).unwrap()).unwrap();
    draft.trigger = DesktopTaskTrigger::GithubRun {
        run: "fixture/project/42".into(),
        credential: DesktopGithubCredential::Stored {
            id: "FIRST_TOKEN".into(),
        },
    };
    assert!(commit_reviewed(&store, prepare_draft(&paths, &store, &draft).unwrap(), &ci).is_err());
    let current = preview(&prepare_draft(&paths, &store, &draft).unwrap()).unwrap();
    ProfileStore::open(&paths)
        .edit_global(
            "fixture",
            ProfileUpdate {
                model: Some("new-model".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        commit_reviewed(
            &store,
            prepare_draft(&paths, &store, &draft).unwrap(),
            &current
        )
        .is_err()
    );
    assert!(store.autonomy_page(0).unwrap().is_empty());
}

#[test]
fn fresh_file_baseline_is_not_a_new_authority_and_calendar_mapping_stays_exact() {
    let (_directory, paths, store, mut draft) = fixture();
    let reviewed = preview(&prepare_draft(&paths, &store, &draft).unwrap()).unwrap();
    fs::write(
        std::path::Path::new(&draft.workspace).join("source/note.txt"),
        "updated between review and create",
    )
    .unwrap();
    let job = prepare_draft(&paths, &store, &draft).unwrap();
    assert_eq!(grant_digest(&job).unwrap(), reviewed.grant_digest);
    commit_reviewed(&store, job, &reviewed).unwrap();
    for trigger in [
        DesktopTaskTrigger::Once {
            at: "2030-01-01T09:00:00-07:00".into(),
        },
        DesktopTaskTrigger::Daily {
            time: "09:30".into(),
            timezone: "America/Los_Angeles".into(),
        },
    ] {
        draft.trigger = trigger.clone();
        let job = prepare_draft(&paths, &store, &draft).unwrap();
        assert!(job.trigger.is_none());
        match trigger {
            DesktopTaskTrigger::Once { at } => assert_eq!(
                job.schedule,
                Schedule::Once {
                    at: at.parse::<jiff::Timestamp>().unwrap().as_second()
                }
            ),
            DesktopTaskTrigger::Daily { timezone, .. } => assert_eq!(
                job.schedule,
                Schedule::Daily {
                    timezone,
                    hour: 9,
                    minute: 30
                }
            ),
            _ => unreachable!(),
        }
    }
}
