use super::*;
use crate::{presentation::ResolvedPresentation, storage::TestCustody};

fn ui() -> SetupUi {
    SetupUi {
        rich: false,
        profile: ResolvedPresentation::test_plain(),
    }
}

#[test]
fn later_is_explicit_and_planning_never_creates_files_or_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    let mut input = std::io::Cursor::new("2\n");
    let mut output = Vec::new();
    let plan = fresh_plan(&SetupArgs::default(), &paths, &mut input, &mut output, ui()).unwrap();
    assert!(matches!(plan, FreshPlan::Protect { export: None }));
    assert!(!paths.data_dir().exists());
    assert!(String::from_utf8(output).unwrap().contains("unrecoverable"));
    plan.apply(&paths, &TestCustody::default()).unwrap();
    assert_eq!(
        recovery::status(paths.data_dir()).unwrap(),
        recovery::RecoveryStatus::Pending
    );
}

#[test]
fn existing_history_is_not_implicitly_migrated_by_connection_setup() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    fs::create_dir_all(paths.data_dir()).unwrap();
    fs::write(paths.data_dir().join("existing"), "retain").unwrap();
    let plan = fresh_plan(
        &SetupArgs::default(),
        &paths,
        &mut std::io::Cursor::new(""),
        &mut Vec::new(),
        ui(),
    )
    .unwrap();
    assert!(matches!(plan, FreshPlan::Keep));
    assert_eq!(
        fs::read_to_string(paths.data_dir().join("existing")).unwrap(),
        "retain"
    );
}

#[test]
fn cancellation_during_backup_selection_does_not_initialize() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    assert!(
        fresh_plan(
            &SetupArgs::default(),
            &paths,
            &mut std::io::Cursor::new(""),
            &mut Vec::new(),
            ui()
        )
        .is_err()
    );
    assert!(!paths.data_dir().exists());
}

#[test]
fn save_now_generates_and_verifies_a_private_backup_without_showing_the_key() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    let destination = temp.path().join("recovery.key");
    let mut input = std::io::Cursor::new(format!("1\n{}\n", destination.display()));
    let mut output = Vec::new();
    let plan = fresh_plan(&SetupArgs::default(), &paths, &mut input, &mut output, ui()).unwrap();
    assert!(!destination.exists());
    plan.apply(&paths, &TestCustody::default()).unwrap();
    let mut receipts = Vec::new();
    plan.record_receipt(&mut receipts);
    ui::write_receipts(&mut output, &receipts).unwrap();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].contains(&destination.display().to_string()));
    assert!(plan.review().contains(&destination.display().to_string()));
    let identity = crate::storage::read_recovery_identity(&destination).unwrap();
    assert!(ProtectedStore::recover(paths.data_dir(), &identity).is_ok());
    assert_eq!(
        recovery::status(paths.data_dir()).unwrap(),
        recovery::RecoveryStatus::Exported
    );
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("AGE-SECRET-KEY")
    );
}

#[test]
fn invalid_export_flags_fail_before_creating_protected_state() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    for path in [
        PathBuf::from("relative.key"),
        temp.path().join("missing/key"),
    ] {
        let args = SetupArgs {
            non_interactive: true,
            recovery_output: Some(path),
            ..Default::default()
        };
        assert!(
            fresh_plan(
                &args,
                &paths,
                &mut std::io::Cursor::new(""),
                &mut Vec::new(),
                ui()
            )
            .is_err()
        );
        assert!(!paths.data_dir().exists());
    }
}

#[test]
fn automatic_desktop_plan_and_dry_run_do_not_create_keys_and_preserve_existing_protection() {
    let temp = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(temp.path().join("home").into_os_string())).unwrap();
    let plan = FreshPlan::automatic(&paths).unwrap();
    assert!(matches!(plan, FreshPlan::Protect { export: None }));
    let preview = fresh_plan(
        &SetupArgs {
            dry_run: true,
            ..Default::default()
        },
        &paths,
        &mut std::io::Cursor::new(""),
        &mut Vec::new(),
        ui(),
    )
    .unwrap();
    assert!(matches!(preview, FreshPlan::Protect { export: None }));
    assert!(!paths.data_dir().exists());
    plan.apply(&paths, &TestCustody::default()).unwrap();
    let original = ProtectedStore::status(paths.data_dir()).unwrap();
    assert!(matches!(
        FreshPlan::automatic(&paths).unwrap(),
        FreshPlan::Keep
    ));
    assert_eq!(ProtectedStore::status(paths.data_dir()).unwrap(), original);
}
