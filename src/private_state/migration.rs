//! Transactional migration for Xana-owned interoperable private records.
//!
//! A migration holds one lock shared by every private-state mutation, writes
//! exact source backups, then installs all target records behind a small
//! recovery journal. The config transaction remains the authoritative final
//! marker. Recovery rolls back when the source config is still present and
//! finalizes when the target config was committed.

use super::{
    schema::{
        EndpointTrustDocument, ExternalAgentStateDocument, OutboundAuditDocument,
        OutboundDecisionDocument, PRIVATE_RECORD_VERSION, PackageStateDocument,
        ProjectBindingsDocument, ProjectRegistryDocument,
    },
    store::{PrivateRecordInspection, PrivateRecordStatus, PrivateStateError},
};
use crate::{bounded_file, paths::XanaPaths};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const LEGACY_PRIVATE_RECORD_VERSION: u32 = 1;
const JOURNAL_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "private-state-migration.json";
const LOCK_FILE: &str = "private-state.lock";
const BACKUP_DIRECTORY: &str = "migration-backups";
const MAX_PRIVATE_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_JOURNAL_BYTES: usize = 128 * 1024;

#[derive(Debug)]
pub(crate) struct PrivateMigrationPlan {
    records: Vec<PlannedRecord>,
    inspections: Vec<PrivateRecordInspection>,
    recovery_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrivateMigrationOutcome {
    pub(crate) initialized_records: usize,
    pub(crate) migrated_records: usize,
    pub(crate) backup_path: Option<PathBuf>,
    pub(crate) recovered_transaction: bool,
}

#[derive(Debug)]
pub(crate) struct PrivateMigrationTransaction {
    lock: Option<MigrationLock>,
    journal: Option<MigrationJournal>,
    journal_path: PathBuf,
    backup_path: Option<PathBuf>,
    outcome: PrivateMigrationOutcome,
}

#[derive(Debug)]
struct PlannedRecord {
    kind: RecordKind,
    path: PathBuf,
    original: Option<Vec<u8>>,
    target: Vec<u8>,
    action: RecordAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordAction {
    None,
    Initialize,
    Migrate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RecordKind {
    Projects,
    ProjectBindings,
    Packages,
    EndpointTrust,
    ExternalAgents,
    OutboundDecisions,
    OutboundAudit,
}

const RECORD_KINDS: [RecordKind; 7] = [
    RecordKind::Projects,
    RecordKind::ProjectBindings,
    RecordKind::Packages,
    RecordKind::EndpointTrust,
    RecordKind::ExternalAgents,
    RecordKind::OutboundDecisions,
    RecordKind::OutboundAudit,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Prepared,
    RecordsApplied,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationJournal {
    version: u32,
    id: String,
    phase: JournalPhase,
    source_config_digest: String,
    target_config_digest: String,
    records: Vec<JournalRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalRecord {
    kind: RecordKind,
    source_digest: Option<String>,
    target_digest: String,
}

#[derive(Debug)]
pub(super) struct MigrationLock {
    file: fs::File,
}

impl PrivateMigrationPlan {
    pub(crate) fn build(paths: &XanaPaths) -> Result<Self, PrivateStateError> {
        let recovery_pending = private_migration_pending(paths)?;
        let records = if recovery_pending {
            Vec::new()
        } else {
            plan_records(paths)?
        };
        let inspections = if recovery_pending {
            super::store::inspect_interoperable_records(paths)
        } else {
            records.iter().map(PlannedRecord::inspection).collect()
        };
        Ok(Self {
            records,
            inspections,
            recovery_pending,
        })
    }

    pub(crate) fn inspections(&self) -> &[PrivateRecordInspection] {
        &self.inspections
    }

    pub(crate) fn recovery_pending(&self) -> bool {
        self.recovery_pending
    }

    pub(crate) fn requires_apply(&self) -> bool {
        self.recovery_pending
            || self
                .records
                .iter()
                .any(|record| record.action != RecordAction::None)
    }

    pub(crate) fn begin(
        self,
        paths: &XanaPaths,
        source_config: &[u8],
        target_config: &[u8],
    ) -> Result<PrivateMigrationTransaction, PrivateStateError> {
        self.begin_with(paths, source_config, target_config, None)
    }

    fn begin_with(
        self,
        paths: &XanaPaths,
        source_config: &[u8],
        target_config: &[u8],
        fail_after_writes: Option<usize>,
    ) -> Result<PrivateMigrationTransaction, PrivateStateError> {
        let lock = MigrationLock::acquire(paths)?;
        let recovered_transaction = recover_if_needed_locked(paths)?;
        let records = if recovered_transaction {
            plan_records(paths)?
        } else {
            verify_reviewed_records(&self.records)?;
            self.records
        };
        let changed = records
            .into_iter()
            .filter(|record| record.action != RecordAction::None)
            .collect::<Vec<_>>();
        let initialized_records = changed
            .iter()
            .filter(|record| record.action == RecordAction::Initialize)
            .count();
        let migrated_records = changed
            .iter()
            .filter(|record| record.action == RecordAction::Migrate)
            .count();
        let journal_path = journal_path(paths);
        if changed.is_empty() {
            return Ok(PrivateMigrationTransaction {
                lock: Some(lock),
                journal: None,
                journal_path,
                backup_path: None,
                outcome: PrivateMigrationOutcome {
                    initialized_records,
                    migrated_records,
                    backup_path: None,
                    recovered_transaction,
                },
            });
        }

        let id = Uuid::new_v4().to_string();
        let backup_path = backup_root(paths).join(&id);
        create_private_directory(&backup_path)?;
        let prepared = prepare_journal(&changed, id, source_config, target_config, &backup_path);
        let mut journal = match prepared {
            Ok(journal) => journal,
            Err(error) => {
                let _ = fs::remove_dir_all(&backup_path);
                return Err(error);
            }
        };
        write_journal(&journal_path, &journal)?;

        if let Err(operation) = install_targets(&changed, fail_after_writes) {
            return Err(rollback_after_failure(
                paths,
                &journal_path,
                &backup_path,
                &journal,
                operation,
            ));
        }
        journal.phase = JournalPhase::RecordsApplied;
        if let Err(operation) = write_journal(&journal_path, &journal) {
            return Err(rollback_after_failure(
                paths,
                &journal_path,
                &backup_path,
                &journal,
                operation,
            ));
        }
        if journal.source_config_digest == journal.target_config_digest {
            journal.phase = JournalPhase::Committed;
            if let Err(operation) = write_journal(&journal_path, &journal) {
                return Err(rollback_after_failure(
                    paths,
                    &journal_path,
                    &backup_path,
                    &journal,
                    operation,
                ));
            }
        }

        Ok(PrivateMigrationTransaction {
            lock: Some(lock),
            journal: Some(journal),
            journal_path,
            backup_path: Some(backup_path.clone()),
            outcome: PrivateMigrationOutcome {
                initialized_records,
                migrated_records,
                backup_path: Some(backup_path),
                recovered_transaction,
            },
        })
    }
}

impl PrivateMigrationTransaction {
    pub(crate) fn commit(mut self) -> Result<PrivateMigrationOutcome, PrivateStateError> {
        if let Some(mut journal) = self.journal.take() {
            let backup_path = self.backup_path.as_deref().ok_or_else(|| {
                PrivateStateError::Invalid("private-state migration lost its backup path".into())
            })?;
            validate_installed_targets(&journal, backup_path)?;
            if journal.phase != JournalPhase::Committed {
                journal.phase = JournalPhase::Committed;
                write_journal(&self.journal_path, &journal)?;
            }
            remove_file_if_present(&self.journal_path)?;
        }
        self.lock.take();
        Ok(self.outcome.clone())
    }

    pub(crate) fn rollback(mut self) -> Vec<String> {
        let mut failures = Vec::new();
        if let (Some(journal), Some(backup_path)) =
            (self.journal.take(), self.backup_path.as_deref())
        {
            failures.extend(restore_records(&journal, backup_path));
            if failures.is_empty()
                && let Err(error) = remove_file_if_present(&self.journal_path)
            {
                failures.push(error.to_string());
            }
        }
        self.lock.take();
        failures
    }
}

pub(crate) fn private_migration_pending(paths: &XanaPaths) -> Result<bool, PrivateStateError> {
    match read_journal(paths)? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

impl MigrationLock {
    pub(super) fn acquire(paths: &XanaPaths) -> Result<Self, PrivateStateError> {
        Self::acquire_path(&lock_path(paths))
    }

    pub(super) fn acquire_for_document(path: &Path) -> Result<Self, PrivateStateError> {
        let parent = path.parent().ok_or_else(|| {
            PrivateStateError::Invalid(format!("{} has no parent directory", path.display()))
        })?;
        let lock = Self::acquire_path(&parent.join(LOCK_FILE))?;
        let journal = parent.join(JOURNAL_FILE);
        match fs::symlink_metadata(&journal) {
            Ok(_) => Err(PrivateStateError::RecoveryRequired(journal)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(lock),
            Err(source) => Err(PrivateStateError::Io {
                path: journal,
                source,
            }),
        }
    }

    fn acquire_path(path: &Path) -> Result<Self, PrivateStateError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| PrivateStateError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| PrivateStateError::Io {
                path: path.to_owned(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(fs::TryLockError::WouldBlock) => Err(PrivateStateError::Busy(path.to_owned())),
            Err(fs::TryLockError::Error(source)) => Err(PrivateStateError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl PlannedRecord {
    fn inspection(&self) -> PrivateRecordInspection {
        let (version, status) = match self.action {
            RecordAction::None => (Some(PRIVATE_RECORD_VERSION), PrivateRecordStatus::Healthy),
            RecordAction::Initialize => (None, PrivateRecordStatus::Missing),
            RecordAction::Migrate => (
                Some(LEGACY_PRIVATE_RECORD_VERSION),
                PrivateRecordStatus::Migratable,
            ),
        };
        PrivateRecordInspection {
            name: self.kind.display_name(),
            version,
            status,
        }
    }
}

impl RecordKind {
    fn display_name(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::ProjectBindings => "project bindings",
            Self::Packages => "package state",
            Self::EndpointTrust => "endpoint trust",
            Self::ExternalAgents => "external agents",
            Self::OutboundDecisions => "outbound decisions",
            Self::OutboundAudit => "outbound audit",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Projects => "projects.json",
            Self::ProjectBindings => "project-bindings.json",
            Self::Packages => "packages.json",
            Self::EndpointTrust => "endpoint-trust.json",
            Self::ExternalAgents => "external-agents.json",
            Self::OutboundDecisions => "outbound-decisions.json",
            Self::OutboundAudit => "outbound-audit.json",
        }
    }

    fn path(self, paths: &XanaPaths) -> PathBuf {
        match self {
            Self::Projects => paths.projects_file(),
            Self::ProjectBindings => paths.project_bindings_file(),
            Self::Packages => paths.package_state_file(),
            Self::EndpointTrust => paths.endpoint_trust_file(),
            Self::ExternalAgents => paths.external_agent_state_file(),
            Self::OutboundDecisions => paths.outbound_decisions_file(),
            Self::OutboundAudit => paths.outbound_audit_file(),
        }
    }

    fn empty(self, path: &Path) -> Result<Vec<u8>, PrivateStateError> {
        match self {
            Self::Projects => encode_document(path, &ProjectRegistryDocument::default()),
            Self::ProjectBindings => encode_document(path, &ProjectBindingsDocument::default()),
            Self::Packages => encode_document(path, &PackageStateDocument::default()),
            Self::EndpointTrust => encode_document(path, &EndpointTrustDocument::default()),
            Self::ExternalAgents => encode_document(path, &ExternalAgentStateDocument::default()),
            Self::OutboundDecisions => encode_document(path, &OutboundDecisionDocument::default()),
            Self::OutboundAudit => encode_document(path, &OutboundAuditDocument::default()),
        }
    }

    fn validate(self, path: &Path, bytes: &[u8]) -> Result<(), PrivateStateError> {
        match self {
            Self::Projects => validate_document::<ProjectRegistryDocument>(path, bytes),
            Self::ProjectBindings => validate_document::<ProjectBindingsDocument>(path, bytes),
            Self::Packages => validate_document::<PackageStateDocument>(path, bytes),
            Self::EndpointTrust => validate_document::<EndpointTrustDocument>(path, bytes),
            Self::ExternalAgents => validate_document::<ExternalAgentStateDocument>(path, bytes),
            Self::OutboundDecisions => validate_document::<OutboundDecisionDocument>(path, bytes),
            Self::OutboundAudit => validate_document::<OutboundAuditDocument>(path, bytes),
        }
    }

    fn migrate(self, path: &Path, bytes: &[u8]) -> Result<Vec<u8>, PrivateStateError> {
        match self {
            Self::Projects => migrate_document::<ProjectRegistryDocument>(path, bytes),
            Self::ProjectBindings => migrate_document::<ProjectBindingsDocument>(path, bytes),
            Self::Packages => migrate_document::<PackageStateDocument>(path, bytes),
            Self::EndpointTrust => migrate_document::<EndpointTrustDocument>(path, bytes),
            Self::ExternalAgents => migrate_document::<ExternalAgentStateDocument>(path, bytes),
            Self::OutboundDecisions => migrate_document::<OutboundDecisionDocument>(path, bytes),
            Self::OutboundAudit => migrate_document::<OutboundAuditDocument>(path, bytes),
        }
    }
}

fn plan_records(paths: &XanaPaths) -> Result<Vec<PlannedRecord>, PrivateStateError> {
    RECORD_KINDS
        .into_iter()
        .map(|kind| plan_record(paths, kind))
        .collect()
}

fn plan_record(paths: &XanaPaths, kind: RecordKind) -> Result<PlannedRecord, PrivateStateError> {
    let path = kind.path(paths);
    let original = read_optional(&path)?;
    let (target, action) = match original.as_deref() {
        None => (kind.empty(&path)?, RecordAction::Initialize),
        Some(bytes) => {
            let version = document_version(&path, bytes)?;
            match version {
                PRIVATE_RECORD_VERSION => {
                    kind.validate(&path, bytes)?;
                    (bytes.to_vec(), RecordAction::None)
                }
                LEGACY_PRIVATE_RECORD_VERSION => {
                    (kind.migrate(&path, bytes)?, RecordAction::Migrate)
                }
                found => {
                    return Err(PrivateStateError::UnsupportedVersion { path, found });
                }
            }
        }
    };
    Ok(PlannedRecord {
        kind,
        path,
        original,
        target,
        action,
    })
}

fn verify_reviewed_records(records: &[PlannedRecord]) -> Result<(), PrivateStateError> {
    for record in records {
        if read_optional(&record.path)? != record.original {
            return Err(PrivateStateError::Changed(record.path.clone()));
        }
    }
    Ok(())
}

fn validate_document<T: DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
) -> Result<(), PrivateStateError> {
    serde_json::from_slice::<T>(bytes)
        .map(|_| ())
        .map_err(|error| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: error.to_string(),
        })
}

fn migrate_document<T: DeserializeOwned + Serialize>(
    path: &Path,
    bytes: &[u8],
) -> Result<Vec<u8>, PrivateStateError> {
    let document =
        serde_json::from_slice::<T>(bytes).map_err(|error| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    let mut value = serde_json::to_value(document).map_err(PrivateStateError::Encode)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: "private record must be a JSON object".to_owned(),
        })?;
    object.insert(
        "version".to_owned(),
        serde_json::Value::from(PRIVATE_RECORD_VERSION),
    );
    encode_document(path, &value)
}

fn encode_document<T: Serialize>(path: &Path, value: &T) -> Result<Vec<u8>, PrivateStateError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(PrivateStateError::Encode)?;
    if bytes.len() > MAX_PRIVATE_RECORD_BYTES {
        return Err(PrivateStateError::TooLarge {
            path: path.to_owned(),
            actual: bytes.len() as u64,
        });
    }
    Ok(bytes)
}

fn document_version(path: &Path, bytes: &[u8]) -> Result<u32, PrivateStateError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| PrivateStateError::Decode {
            path: path.to_owned(),
            reason: "private record has no valid version".to_owned(),
        })
}

fn prepare_journal(
    records: &[PlannedRecord],
    id: String,
    source_config: &[u8],
    target_config: &[u8],
    backup_path: &Path,
) -> Result<MigrationJournal, PrivateStateError> {
    let mut journal_records = Vec::with_capacity(records.len());
    for record in records {
        if let Some(original) = record.original.as_deref() {
            atomic_write_bytes(&backup_path.join(record.kind.file_name()), original)?;
        }
        journal_records.push(JournalRecord {
            kind: record.kind,
            source_digest: record.original.as_deref().map(digest),
            target_digest: digest(&record.target),
        });
    }
    Ok(MigrationJournal {
        version: JOURNAL_VERSION,
        id,
        phase: JournalPhase::Prepared,
        source_config_digest: digest(source_config),
        target_config_digest: digest(target_config),
        records: journal_records,
    })
}

fn install_targets(
    records: &[PlannedRecord],
    fail_after_writes: Option<usize>,
) -> Result<(), PrivateStateError> {
    for (index, record) in records.iter().enumerate() {
        atomic_write_bytes(&record.path, &record.target)?;
        if fail_after_writes == Some(index + 1) {
            return Err(PrivateStateError::Invalid(format!(
                "injected private-state failure after {} record write(s)",
                index + 1
            )));
        }
    }
    Ok(())
}

fn recover_if_needed_locked(paths: &XanaPaths) -> Result<bool, PrivateStateError> {
    let Some(mut journal) = read_journal(paths)? else {
        return Ok(false);
    };
    validate_journal(paths, &journal)?;
    let config = bounded_file::read(paths.config_file(), MAX_PRIVATE_RECORD_BYTES)
        .map_err(map_read_error)?;
    let config_digest = digest(&config);
    if config_digest == journal.target_config_digest
        && (journal.phase == JournalPhase::Committed
            || journal.source_config_digest != journal.target_config_digest)
    {
        let backup_path = backup_root(paths).join(&journal.id);
        validate_installed_targets(&journal, &backup_path)?;
        if journal.phase != JournalPhase::Committed {
            journal.phase = JournalPhase::Committed;
            write_journal(&journal_path(paths), &journal)?;
        }
        remove_file_if_present(&journal_path(paths))?;
        return Ok(true);
    }
    if journal.phase == JournalPhase::Committed || config_digest != journal.source_config_digest {
        return Err(PrivateStateError::RecoveryRequired(journal_path(paths)));
    }
    let backup_path = backup_root(paths).join(&journal.id);
    let failures = restore_records(&journal, &backup_path);
    if !failures.is_empty() {
        return Err(PrivateStateError::Rollback {
            operation: "recover interrupted private-state migration".to_owned(),
            failures,
        });
    }
    remove_file_if_present(&journal_path(paths))?;
    Ok(true)
}

fn validate_journal(
    paths: &XanaPaths,
    journal: &MigrationJournal,
) -> Result<(), PrivateStateError> {
    if journal.version != JOURNAL_VERSION || Uuid::parse_str(&journal.id).is_err() {
        return Err(PrivateStateError::RecoveryRequired(journal_path(paths)));
    }
    let kinds = journal
        .records
        .iter()
        .map(|record| record.kind)
        .collect::<BTreeSet<_>>();
    if kinds.len() != journal.records.len()
        || journal.records.is_empty()
        || journal
            .records
            .iter()
            .any(|record| !RECORD_KINDS.contains(&record.kind))
    {
        return Err(PrivateStateError::RecoveryRequired(journal_path(paths)));
    }
    Ok(())
}

fn validate_installed_targets(
    journal: &MigrationJournal,
    backup_path: &Path,
) -> Result<(), PrivateStateError> {
    let interoperable = backup_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| PrivateStateError::Invalid("migration backup path is invalid".into()))?;
    for record in &journal.records {
        let path = interoperable.join(record.kind.file_name());
        let bytes = read_required(&path)?;
        if digest(&bytes) != record.target_digest {
            return Err(PrivateStateError::Changed(path));
        }
    }
    Ok(())
}

fn restore_records(journal: &MigrationJournal, backup_path: &Path) -> Vec<String> {
    let Some(interoperable) = backup_path.parent().and_then(Path::parent) else {
        return vec!["migration backup path is invalid".to_owned()];
    };
    let mut failures = Vec::new();
    for record in journal.records.iter().rev() {
        let target = interoperable.join(record.kind.file_name());
        let result = match record.source_digest.as_deref() {
            Some(expected) => {
                read_required(&backup_path.join(record.kind.file_name())).and_then(|bytes| {
                    if digest(&bytes) != expected {
                        return Err(PrivateStateError::Changed(
                            backup_path.join(record.kind.file_name()),
                        ));
                    }
                    atomic_write_bytes(&target, &bytes)
                })
            }
            None => remove_file_if_present(&target),
        };
        if let Err(error) = result {
            failures.push(error.to_string());
        }
    }
    failures
}

fn rollback_after_failure(
    _paths: &XanaPaths,
    journal_path: &Path,
    backup_path: &Path,
    journal: &MigrationJournal,
    operation: PrivateStateError,
) -> PrivateStateError {
    let mut failures = restore_records(journal, backup_path);
    if failures.is_empty()
        && let Err(error) = remove_file_if_present(journal_path)
    {
        failures.push(error.to_string());
    }
    if failures.is_empty() {
        operation
    } else {
        PrivateStateError::Rollback {
            operation: operation.to_string(),
            failures,
        }
    }
}

fn read_journal(paths: &XanaPaths) -> Result<Option<MigrationJournal>, PrivateStateError> {
    let path = journal_path(paths);
    match bounded_file::read(&path, MAX_JOURNAL_BYTES) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| PrivateStateError::Decode {
                    path,
                    reason: error.to_string(),
                })
        }
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(map_read_error(error)),
    }
}

fn write_journal(path: &Path, journal: &MigrationJournal) -> Result<(), PrivateStateError> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(PrivateStateError::Encode)?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(PrivateStateError::TooLarge {
            path: path.to_owned(),
            actual: bytes.len() as u64,
        });
    }
    atomic_write_bytes(path, &bytes)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, PrivateStateError> {
    match bounded_file::read(path, MAX_PRIVATE_RECORD_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(map_read_error(error)),
    }
}

fn read_required(path: &Path) -> Result<Vec<u8>, PrivateStateError> {
    bounded_file::read(path, MAX_PRIVATE_RECORD_BYTES).map_err(map_read_error)
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), PrivateStateError> {
    let parent = path.parent().ok_or_else(|| {
        PrivateStateError::Invalid(format!("{} has no parent directory", path.display()))
    })?;
    create_private_directory(parent)?;
    let mut file =
        atomic_write_file::AtomicWriteFile::open(path).map_err(|source| PrivateStateError::Io {
            path: path.to_owned(),
            source,
        })?;
    protect_open_file(file.as_file()).map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.write_all(bytes)
        .map_err(|source| PrivateStateError::Io {
            path: path.to_owned(),
            source,
        })?;
    file.commit().map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })
}

fn remove_file_if_present(path: &Path) -> Result<(), PrivateStateError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PrivateStateError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn create_private_directory(path: &Path) -> Result<(), PrivateStateError> {
    fs::create_dir_all(path).map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })?;
    protect_directory(path).map_err(|source| PrivateStateError::Io {
        path: path.to_owned(),
        source,
    })
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn interoperable_root(paths: &XanaPaths) -> PathBuf {
    paths.data_dir().join("interoperable")
}

fn lock_path(paths: &XanaPaths) -> PathBuf {
    interoperable_root(paths).join(LOCK_FILE)
}

fn journal_path(paths: &XanaPaths) -> PathBuf {
    interoperable_root(paths).join(JOURNAL_FILE)
}

fn backup_root(paths: &XanaPaths) -> PathBuf {
    interoperable_root(paths).join(BACKUP_DIRECTORY)
}

fn map_read_error(error: bounded_file::BoundedReadError) -> PrivateStateError {
    match error {
        bounded_file::BoundedReadError::TooLarge { path, actual, .. } => {
            PrivateStateError::TooLarge { path, actual }
        }
        bounded_file::BoundedReadError::Io { path, source } => {
            PrivateStateError::Io { path, source }
        }
    }
}

#[cfg(unix)]
fn protect_open_file(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn protect_open_file(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::ProjectId,
        private_state::{
            FrozenProfileSnapshot, ProjectLifecycle, ProjectRecord, read_document, update_document,
        },
    };
    use std::{collections::BTreeMap, ffi::OsString};
    use tempfile::tempdir;

    fn test_paths() -> (tempfile::TempDir, XanaPaths) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), b"source-config").unwrap();
        (directory, paths)
    }

    fn write_v1_records(paths: &XanaPaths) -> BTreeMap<RecordKind, Vec<u8>> {
        let workspace = paths.data_dir().join("preserved-workspace");
        let project_id = ProjectId::new();
        let mut projects = ProjectRegistryDocument::default();
        projects.projects.insert(
            project_id,
            ProjectRecord {
                id: project_id,
                name: "Preserved project".into(),
                canonical_workspace: workspace,
                lifecycle: ProjectLifecycle::Archived,
                created_unix_ms: 10,
                updated_unix_ms: 20,
            },
        );
        projects
            .conversation_memberships
            .insert("child-conversation".into(), project_id);
        projects.conversation_profiles.insert(
            "child-conversation".into(),
            FrozenProfileSnapshot {
                profile_id: "profile-id".into(),
                profile_name: "preserved-profile".into(),
                scope: "global".into(),
                digest: "profile-digest".into(),
                resolved: serde_json::json!({"model": "preserved-model"}),
            },
        );
        projects
            .conversation_predecessors
            .insert("child-conversation".into(), "source-conversation".into());

        let mut originals = BTreeMap::new();
        for kind in RECORD_KINDS {
            let path = kind.path(paths);
            let bytes = if kind == RecordKind::Projects {
                encode_document(&path, &projects).unwrap()
            } else {
                kind.empty(&path).unwrap()
            };
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value["version"] = serde_json::Value::from(LEGACY_PRIVATE_RECORD_VERSION);
            let legacy = serde_json::to_vec_pretty(&value).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &legacy).unwrap();
            originals.insert(kind, legacy);
        }
        originals
    }

    fn assert_all_versions(paths: &XanaPaths, version: u32) {
        for kind in RECORD_KINDS {
            let bytes = fs::read(kind.path(paths)).unwrap();
            assert_eq!(
                document_version(&kind.path(paths), &bytes).unwrap(),
                version
            );
        }
    }

    #[test]
    fn v1_records_migrate_transactionally_without_losing_relationships() {
        let (_directory, paths) = test_paths();
        let originals = write_v1_records(&paths);
        let run_sentinel = paths.data_dir().join("sessions/run-owned-state.json");
        fs::create_dir_all(run_sentinel.parent().unwrap()).unwrap();
        fs::write(&run_sentinel, b"not-owned-by-private-migration").unwrap();
        let plan = PrivateMigrationPlan::build(&paths).unwrap();
        assert!(plan.requires_apply());
        assert!(
            plan.inspections()
                .iter()
                .all(|record| record.status == PrivateRecordStatus::Migratable)
        );

        let outcome = plan
            .begin(&paths, b"source-config", b"source-config")
            .unwrap()
            .commit()
            .unwrap();

        assert_eq!(outcome.initialized_records, 0);
        assert_eq!(outcome.migrated_records, RECORD_KINDS.len());
        let backup = outcome.backup_path.unwrap();
        assert_eq!(
            fs::read(backup.join(RecordKind::Projects.file_name())).unwrap(),
            originals[&RecordKind::Projects]
        );
        assert_all_versions(&paths, PRIVATE_RECORD_VERSION);
        let projects: ProjectRegistryDocument = read_document(&paths.projects_file()).unwrap();
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(
            projects.conversation_predecessors["child-conversation"],
            "source-conversation"
        );
        assert_eq!(
            projects.conversation_profiles["child-conversation"].profile_name,
            "preserved-profile"
        );
        assert_eq!(
            fs::read(run_sentinel).unwrap(),
            b"not-owned-by-private-migration"
        );
        assert!(!private_migration_pending(&paths).unwrap());
        assert!(
            !PrivateMigrationPlan::build(&paths)
                .unwrap()
                .requires_apply()
        );
    }

    #[test]
    fn mixed_missing_legacy_and_current_records_have_exact_actions() {
        let (_directory, paths) = test_paths();
        write_v1_records(&paths);
        fs::remove_file(paths.outbound_audit_file()).unwrap();
        let current = ProjectBindingsDocument::default();
        fs::write(
            paths.project_bindings_file(),
            serde_json::to_vec_pretty(&current).unwrap(),
        )
        .unwrap();

        let outcome = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"source-config", b"source-config")
            .unwrap()
            .commit()
            .unwrap();

        assert_eq!(outcome.initialized_records, 1);
        assert_eq!(outcome.migrated_records, 5);
        assert_all_versions(&paths, PRIVATE_RECORD_VERSION);
    }

    #[test]
    fn reviewed_source_changes_fail_before_any_migration_mutation() {
        let (_directory, paths) = test_paths();
        let originals = write_v1_records(&paths);
        let plan = PrivateMigrationPlan::build(&paths).unwrap();
        fs::write(paths.projects_file(), b"changed-after-review").unwrap();

        assert!(matches!(
            plan.begin(&paths, b"source-config", b"target-config"),
            Err(PrivateStateError::Changed(_))
        ));
        assert_eq!(
            fs::read(paths.package_state_file()).unwrap(),
            originals[&RecordKind::Packages]
        );
        assert!(!private_migration_pending(&paths).unwrap());
    }

    #[test]
    fn injected_partial_write_restores_every_exact_source_record() {
        let (_directory, paths) = test_paths();
        let originals = write_v1_records(&paths);
        let error = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin_with(&paths, b"source-config", b"target-config", Some(3))
            .unwrap_err();

        assert!(error.to_string().contains("injected private-state failure"));
        for kind in RECORD_KINDS {
            assert_eq!(fs::read(kind.path(&paths)).unwrap(), originals[&kind]);
        }
        assert!(!private_migration_pending(&paths).unwrap());
        assert!(backup_root(&paths).read_dir().unwrap().next().is_some());
    }

    #[test]
    fn crash_before_config_commit_is_recovered_and_can_finish_on_retry() {
        let (_directory, paths) = test_paths();
        write_v1_records(&paths);
        let interrupted = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"source-config", b"target-config")
            .unwrap();
        drop(interrupted);
        assert!(private_migration_pending(&paths).unwrap());
        assert!(matches!(
            super::super::store::ensure_interoperable_records(&paths),
            Err(PrivateStateError::RecoveryRequired(_))
        ));

        let retry = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"source-config", b"target-config")
            .unwrap();
        fs::write(paths.config_file(), b"target-config").unwrap();
        let outcome = retry.commit().unwrap();

        assert!(outcome.recovered_transaction);
        assert_all_versions(&paths, PRIVATE_RECORD_VERSION);
        assert!(!private_migration_pending(&paths).unwrap());
    }

    #[test]
    fn crash_after_config_commit_finalizes_without_downgrading_records() {
        let (_directory, paths) = test_paths();
        write_v1_records(&paths);
        let interrupted = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"source-config", b"target-config")
            .unwrap();
        fs::write(paths.config_file(), b"target-config").unwrap();
        drop(interrupted);

        let retry = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"target-config", b"target-config")
            .unwrap();
        let outcome = retry.commit().unwrap();

        assert!(outcome.recovered_transaction);
        assert_eq!(outcome.migrated_records, 0);
        assert_all_versions(&paths, PRIVATE_RECORD_VERSION);
        assert!(!private_migration_pending(&paths).unwrap());
    }

    #[test]
    fn global_lock_blocks_migration_and_pending_journal_blocks_updates() {
        let (_directory, paths) = test_paths();
        write_v1_records(&paths);
        let plan = PrivateMigrationPlan::build(&paths).unwrap();
        let held = MigrationLock::acquire(&paths).unwrap();
        assert!(matches!(
            plan.begin(&paths, b"source-config", b"target-config"),
            Err(PrivateStateError::Busy(_))
        ));
        drop(held);

        let interrupted = PrivateMigrationPlan::build(&paths)
            .unwrap()
            .begin(&paths, b"source-config", b"target-config")
            .unwrap();
        drop(interrupted);
        let update =
            update_document::<ProjectRegistryDocument, _, ()>(&paths.projects_file(), |_| Ok(()));
        assert!(matches!(
            update,
            Err(super::super::store::UpdateDocumentError::State(
                PrivateStateError::RecoveryRequired(_)
            ))
        ));
    }
}
