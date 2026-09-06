use super::*;
use crate::{
    native_runtime::OperationOutcome,
    private_state::{FrozenProfileSnapshot, ProjectRegistryDocument},
    session::SessionRecord,
    storage::{RecoveryIdentity, TestCustody, backup::BackupPolicy},
};

struct Fixture {
    paths: XanaPaths,
    workspace: PathBuf,
    home: ProtectedStore,
    recovery: RecoveryIdentity,
    session: DurableSession,
    reader: DesktopCommandOutcomes,
    // Drop database/session leases before Windows temporary-directory cleanup.
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let recovery = RecoveryIdentity::generate();
        let home = ProtectedStore::initialize(paths.data_dir(), &recovery, &TestCustody::default())
            .unwrap();
        let session =
            DurableSession::create_protected(home.clone(), workspace.clone(), SessionId::new())
                .unwrap();
        let mut profiles = ProjectRegistryDocument::default();
        profiles.conversation_profiles.insert(
            session.session_id().to_string(),
            FrozenProfileSnapshot {
                profile_id: Uuid::new_v4().to_string(),
                profile_name: "fixture".into(),
                scope: "user".into(),
                digest: "a".repeat(64),
                resolved: serde_json::json!({}),
            },
        );
        home.set_document(
            "interoperable/projects.json",
            &serde_json::to_vec(&profiles).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap();
        let reader =
            DesktopCommandOutcomes::new(paths.clone(), workspace.clone(), Some(home.clone()))
                .with_namespace(Uuid::new_v4());
        reader
            .selection
            .lock()
            .unwrap()
            .select(Some((session.session_id(), true, None)));
        Self {
            directory,
            paths,
            workspace,
            home,
            recovery,
            session,
            reader,
        }
    }

    fn admit(&mut self, text: &str) -> DesktopCommandKey {
        let key = self.reader.prepare(text, &[]).unwrap();
        let input_entry_id = self
            .session
            .append_message(Message::text(Role::User, text))
            .unwrap();
        self.session
            .append_record(SessionRecord::AdapterOperationAccepted {
                operation_id: key.operation(),
                thread_id: self.session.thread_id(),
                input_entry_id,
                binding: key.clone(),
            })
            .unwrap();
        key
    }
    fn complete(&mut self, key: &DesktopCommandKey) {
        self.session
            .append_message(Message::text(Role::Assistant, "exact result"))
            .unwrap();
        let record = self
            .session
            .finish_record(key.operation(), OperationOutcome::Completed)
            .unwrap();
        self.session.append_record(record).unwrap();
    }
}

#[test]
fn selection_generation_survives_deselect_reselect_same_conversation() {
    let mut state = SelectionState::default();
    let session = SessionId::new();
    state.select(Some((session, true, None)));
    let first = state.current.clone().unwrap();
    state.select(None);
    state.select(Some((session, true, None)));
    let next = state.current.clone().unwrap();
    assert_eq!(next.session, first.session);
    assert!(
        next.revision > first.revision,
        "a lookup cannot survive A→none→A"
    );
    state.select(Some((session, true, Some(OperationId::new()))));
    assert_eq!(
        state.current.unwrap().revision,
        next.revision,
        "ordinary progress does not revoke the Conversation"
    );
}

#[test]
fn closed_reader_cannot_be_revived_by_a_queued_snapshot() {
    let mut fixture = Fixture::new();
    let key = fixture.admit("closed reader");
    fixture.complete(&key);
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::Completed
    );
    fixture.reader.revoke();
    fixture.reader.selection.lock().unwrap().select(Some((
        key.session(),
        true,
        Some(key.operation()),
    )));
    assert!(fixture.reader.selected().is_err());
    assert_eq!(
        fixture.reader.lookup(&key).unavailable,
        Some(DesktopCommandUnavailable::ScopeChanged)
    );
}

#[test]
fn durable_adapter_states_and_exact_terminal_binding_survive_projection_eviction() {
    let mut fixture = Fixture::new();
    let key = fixture.reader.prepare("first", &[]).unwrap();
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::NotFound
    );
    let key = fixture.admit("first");
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::Unknown
    );
    fixture.reader.selection.lock().unwrap().select(Some((
        fixture.session.session_id(),
        true,
        Some(key.operation()),
    )));
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::Pending
    );
    fixture.complete(&key);
    let outcome = fixture.reader.lookup(&key);
    assert_eq!(outcome.state, DesktopCommandState::Completed);
    assert!(outcome.result.is_some());
    fixture
        .session
        .append_message(Message::text(Role::User, "new retained tail"))
        .unwrap();
    let budget = crate::prompt::PromptBudgetPlan::derive(
        &crate::prompt::PromptBudgetPolicy {
            retained_tail_tokens: 1,
            ..Default::default()
        },
        crate::prompt::ModelBudgetFacts {
            connection: "fixture".into(),
            model: "fixture".into(),
            context_tokens: None,
            max_output_tokens: None,
            reasoning: false,
        },
    )
    .unwrap();
    fixture
        .session
        .compact_conversation(
            OperationId::new(),
            crate::session::CompactionReason::Manual,
            &budget,
        )
        .unwrap();
    assert_eq!(
        fixture.reader.lookup(&key),
        outcome,
        "completed receipts survive real compaction"
    );
    fixture.session.clear_conversation().unwrap(); // archives entries and checkpoints execution
    assert_eq!(fixture.reader.lookup(&key), outcome);
    assert!(
        fixture
            .session
            .inspect_stored_operation(key.operation())
            .unwrap()
            .unwrap()
            .adapter
            .is_some()
    );
    fixture
        .home
        .verify_historical_transitions(fixture.session.session_id())
        .unwrap();
}

#[test]
fn adapter_finish_rejects_stale_other_turn_result_and_unbound_terminal() {
    let mut fixture = Fixture::new();
    let prior = fixture.admit("prior");
    fixture.complete(&prior);
    let prior_ref = fixture.reader.lookup(&prior).result.unwrap();
    let key = fixture.admit("current");
    assert!(
        fixture
            .session
            .append_record(SessionRecord::AdapterOperationFinished {
                operation_id: key.operation(),
                outcome: OperationOutcome::Completed,
                result_entry: Some(prior_ref)
            })
            .is_err()
    );
    assert!(
        fixture
            .session
            .append_record(SessionRecord::OperationFinished {
                operation_id: key.operation(),
                outcome: OperationOutcome::Completed
            })
            .is_err()
    );
    assert!(
        fixture
            .session
            .append_record(SessionRecord::AdapterOperationFinished {
                operation_id: key.operation(),
                outcome: OperationOutcome::Completed,
                result_entry: None
            })
            .is_err()
    );
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::Unknown
    );
    fixture.complete(&key);
    assert_eq!(
        fixture.reader.lookup(&key).state,
        DesktopCommandState::Completed
    );
}

#[test]
fn owner_scope_namespace_collision_profile_version_and_revocation_fail_closed() {
    let mut fixture = Fixture::new();
    let key = fixture.admit("secret output");
    fixture.complete(&key);
    assert_eq!(
        fixture
            .reader
            .with_namespace(Uuid::new_v4())
            .lookup(&key)
            .unavailable,
        Some(DesktopCommandUnavailable::ScopeChanged)
    );
    let mut altered = serde_json::to_value(&key).unwrap();
    altered["payload"] = serde_json::Value::String("b".repeat(64));
    let collision: DesktopCommandKey = serde_json::from_value(altered).unwrap();
    assert_eq!(
        fixture.reader.lookup(&collision).unavailable,
        Some(DesktopCommandUnavailable::CorruptOrIncompatible)
    );
    let profile = fixture
        .home
        .document("interoperable/projects.json", 4 * 1024 * 1024)
        .unwrap()
        .unwrap();
    let mut incompatible: serde_json::Value = serde_json::from_slice(&profile).unwrap();
    incompatible["version"] = 99.into();
    fixture
        .home
        .set_document(
            "interoperable/projects.json",
            &serde_json::to_vec(&incompatible).unwrap(),
            4 * 1024 * 1024,
        )
        .unwrap();
    assert_eq!(
        fixture.reader.lookup(&key).unavailable,
        Some(DesktopCommandUnavailable::CorruptOrIncompatible)
    );
    fixture
        .home
        .set_document("interoperable/projects.json", &profile, 4 * 1024 * 1024)
        .unwrap();
    fixture.home.lock().unwrap();
    assert!(fixture.reader.lookup(&key).unavailable.is_some());
    let unlocked = ProtectedStore::recover(fixture.paths.data_dir(), &fixture.recovery).unwrap();
    assert!(
        fixture.reader.lookup(&key).unavailable.is_some(),
        "unlock must not revive a cached reader"
    );
    let fresh = DesktopCommandOutcomes::new(
        fixture.paths.clone(),
        fixture.workspace.clone(),
        Some(unlocked),
    )
    .with_namespace(fixture.reader.namespace);
    fresh
        .selection
        .lock()
        .unwrap()
        .select(Some((key.session(), true, None)));
    assert_eq!(fresh.lookup(&key).state, DesktopCommandState::Completed);
}

#[test]
fn compatible_backup_restore_retains_identity_without_replaying_and_other_store_is_unavailable() {
    let mut fixture = Fixture::new();
    let key = fixture.admit("backup fixture");
    fixture.complete(&key);
    fixture.session.clear_conversation().unwrap();
    let outcome = fixture.reader.lookup(&key);
    let snapshot = fixture
        .home
        .backup(&BackupPolicy::default(), 1000, false)
        .unwrap()
        .snapshot
        .unwrap();
    let target = XanaPaths::resolve(Some(
        fixture.directory.path().join("restored").into_os_string(),
    ))
    .unwrap();
    let plan = crate::storage::restore::preview(&target, &snapshot, &fixture.recovery).unwrap();
    crate::storage::restore::apply(&target, &snapshot, &fixture.recovery, &plan.review).unwrap();
    let restored = ProtectedStore::recover(target.data_dir(), &fixture.recovery).unwrap();
    let reader = DesktopCommandOutcomes::new(target, fixture.workspace.clone(), Some(restored))
        .with_namespace(fixture.reader.namespace);
    reader
        .selection
        .lock()
        .unwrap()
        .select(Some((key.session(), true, None)));
    assert_eq!(reader.lookup(&key), outcome);
    let other = Fixture::new();
    assert!(other.reader.lookup(&key).unavailable.is_some());
}

#[test]
fn interrupted_and_failed_admissions_are_terminal_not_replay_advice() {
    for (terminal, expected) in [
        (
            OperationOutcome::Interrupted,
            DesktopCommandState::Interrupted,
        ),
        (OperationOutcome::Failed, DesktopCommandState::Failed),
        (OperationOutcome::Declined, DesktopCommandState::Declined),
    ] {
        let mut fixture = Fixture::new();
        let key = fixture.admit("failure fixture");
        let record = fixture
            .session
            .finish_record(key.operation(), terminal)
            .unwrap();
        fixture.session.append_record(record).unwrap();
        let result = fixture.reader.lookup(&key);
        assert_eq!(result.state, expected);
        assert!(result.result.is_none());
    }
}

#[test]
fn known_committed_answer_remains_inspectable_without_upgrading_failed_or_interrupted_work() {
    for (terminal, expected) in [
        (OperationOutcome::Failed, DesktopCommandState::Failed),
        (
            OperationOutcome::Interrupted,
            DesktopCommandState::Interrupted,
        ),
    ] {
        let mut fixture = Fixture::new();
        let key = fixture.admit("partially delivered fixture");
        let entry = fixture
            .session
            .append_message(Message::text(
                Role::Assistant,
                "answer committed before terminal failure",
            ))
            .unwrap();
        let record = fixture
            .session
            .finish_record(key.operation(), terminal)
            .unwrap();
        fixture.session.append_record(record).unwrap();
        let result = fixture.reader.lookup(&key);
        assert_eq!(result.state, expected);
        assert_eq!(
            result.result.unwrap().entry_id.to_string(),
            entry.to_string()
        );
    }
}
