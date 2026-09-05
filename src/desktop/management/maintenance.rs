//! Typed diagnosis, migration, recovery, support, and reset control plane.
//!
//! The Desktop reviews bounded immutable projections, then returns the exact
//! projection for conflict checking before any repair or destructive change.

use super::{DesktopControlPlane, control_error};
use crate::{
    credential::delete_secret,
    desktop::{DesktopError, DesktopErrorCode},
    diagnostics, doctor,
    reset::{ResetPlan, ResetScope, referenced_credential_ids},
};
use std::{collections::BTreeSet, path::PathBuf};

const MAINTENANCE_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopDoctorSeverity {
    Ok,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDoctorFinding {
    pub code: String,
    pub severity: DesktopDoctorSeverity,
    pub summary: String,
    pub evidence: String,
    pub action: Option<String>,
    pub repairable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDoctorSnapshot {
    pub version: u16,
    pub findings: Vec<DesktopDoctorFinding>,
    pub repairable_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDoctorRepairResult {
    pub code: String,
    pub changed: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDoctorRepairReceipt {
    pub semantic_code: String,
    pub results: Vec<DesktopDoctorRepairResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPrivateMigrationRecord {
    pub name: String,
    pub version: Option<u32>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMigrationSnapshot {
    pub version: u16,
    pub source_version: u32,
    pub target_version: u32,
    pub requires_apply: bool,
    pub private_recovery_pending: bool,
    pub private_records: Vec<DesktopPrivateMigrationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMigrationReceipt {
    pub semantic_code: String,
    pub changed_config: bool,
    pub initialized_private_records: usize,
    pub migrated_private_records: usize,
    pub recovered_private_transaction: bool,
    pub backup_path: String,
    pub private_backup_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DesktopResetScope {
    Setup,
    Sessions,
    Caches,
    Credentials,
}

impl DesktopResetScope {
    pub const ALL: [Self; 4] = [Self::Setup, Self::Sessions, Self::Caches, Self::Credentials];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Sessions => "sessions",
            Self::Caches => "caches",
            Self::Credentials => "credentials",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResetTarget {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResetPlan {
    pub version: u16,
    pub scopes: Vec<DesktopResetScope>,
    pub targets: Vec<DesktopResetTarget>,
    pub credential_ids: Vec<String>,
    pub preserved: Vec<String>,
    pub requires_files_confirmation: bool,
    pub requires_credentials_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResetReceipt {
    pub semantic_code: String,
    pub removed: Vec<DesktopResetTarget>,
    pub removed_credentials: Vec<String>,
    pub absent_credentials: Vec<String>,
    pub preserved: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDiagnosticEntry {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub modified_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopDiagnosticsSnapshot {
    pub version: u16,
    pub enabled: bool,
    pub log_directory: String,
    pub crash_directory: String,
    pub retained_files: usize,
    pub retained_bytes: u64,
    pub retention_compliant: bool,
    pub permissions_private: bool,
    pub stale_markers: usize,
    pub invalid_reports: usize,
    pub dropped_events: u64,
    pub writer_faults: u64,
    pub entries: Vec<DesktopDiagnosticEntry>,
}

impl DesktopControlPlane {
    pub async fn doctor_snapshot(
        &self,
        probe_connections: bool,
    ) -> Result<DesktopDoctorSnapshot, DesktopError> {
        let report = doctor::inspect(
            &self.paths,
            doctor::TerminalHealth {
                input_is_terminal: false,
                output_is_terminal: false,
                dumb: false,
            },
            probe_connections,
        )
        .await;
        Ok(project_doctor(report))
    }

    pub async fn apply_doctor_repairs(
        &self,
        reviewed_codes: &[String],
    ) -> Result<DesktopDoctorRepairReceipt, DesktopError> {
        let report = doctor::inspect(
            &self.paths,
            doctor::TerminalHealth {
                input_is_terminal: false,
                output_is_terminal: false,
                dumb: false,
            },
            false,
        )
        .await;
        let mut actual = report
            .repairs()
            .map(|finding| finding.code.clone())
            .collect::<Vec<_>>();
        actual.sort();
        actual.dedup();
        let mut reviewed = reviewed_codes.to_vec();
        reviewed.sort();
        reviewed.dedup();
        if actual != reviewed {
            return Err(conflict(
                "Doctor findings changed after review; diagnose again before applying repairs",
            ));
        }
        let results = report
            .apply_repairs(&self.paths)
            .into_iter()
            .map(|(code, result)| match result {
                Ok(changed) => DesktopDoctorRepairResult {
                    code,
                    changed,
                    failure: None,
                },
                Err(reason) => DesktopDoctorRepairResult {
                    code,
                    changed: false,
                    failure: Some(reason),
                },
            })
            .collect::<Vec<_>>();
        let partial = results.iter().any(|result| result.failure.is_some());
        Ok(DesktopDoctorRepairReceipt {
            semantic_code: if partial {
                "doctor.repair.partial.v1"
            } else {
                "doctor.repair.completed.v1"
            }
            .to_owned(),
            results,
        })
    }

    pub fn migration_snapshot(&self) -> Result<DesktopMigrationSnapshot, DesktopError> {
        crate::config_migration::ConfigMigrationPlan::build(&self.paths)
            .map(project_migration)
            .map_err(control_error)
    }

    pub fn apply_migration(
        &self,
        reviewed: &DesktopMigrationSnapshot,
    ) -> Result<DesktopMigrationReceipt, DesktopError> {
        let plan = crate::config_migration::ConfigMigrationPlan::build(&self.paths)
            .map_err(control_error)?;
        if project_migration_ref(&plan) != *reviewed {
            return Err(conflict(
                "Migration state changed after review; preview it again before applying",
            ));
        }
        plan.apply(&self.paths)
            .map(|outcome| DesktopMigrationReceipt {
                semantic_code: "configuration.migration.completed.v1".to_owned(),
                changed_config: outcome.changed_config,
                initialized_private_records: outcome.initialized_private_records,
                migrated_private_records: outcome.migrated_private_records,
                recovered_private_transaction: outcome.recovered_private_transaction,
                backup_path: outcome.backup_path.display().to_string(),
                private_backup_path: outcome
                    .private_backup_path
                    .map(|path| path.display().to_string()),
            })
            .map_err(control_error)
    }

    pub fn reset_plan(
        &self,
        scopes: &[DesktopResetScope],
    ) -> Result<DesktopResetPlan, DesktopError> {
        let core_scopes = scopes
            .iter()
            .copied()
            .map(core_reset_scope)
            .collect::<BTreeSet<_>>();
        let credential_ids = if core_scopes.contains(&ResetScope::Credentials) {
            referenced_credential_ids(&self.paths).map_err(control_error)?
        } else {
            Vec::new()
        };
        ResetPlan::inspect(&self.paths, core_scopes, credential_ids)
            .map(|plan| project_reset(&plan))
            .map_err(control_error)
    }

    pub fn execute_reset(
        &self,
        reviewed: &DesktopResetPlan,
        files_confirmed: bool,
        credentials_confirmed: bool,
    ) -> Result<DesktopResetReceipt, DesktopError> {
        let current = self.reset_plan(&reviewed.scopes)?;
        if current != *reviewed {
            return Err(conflict(
                "Reset targets changed after review; preview them again before continuing",
            ));
        }
        if current.requires_files_confirmation && !files_confirmed {
            return Err(conflict(
                "Filesystem reset requires an explicit confirmation after reviewing exact targets",
            ));
        }
        if current.requires_credentials_confirmation && !credentials_confirmed {
            return Err(conflict(
                "Credential deletion requires its own explicit confirmation",
            ));
        }

        let core_scopes = current
            .scopes
            .iter()
            .copied()
            .map(core_reset_scope)
            .collect();
        let core = ResetPlan::inspect(&self.paths, core_scopes, current.credential_ids.clone())
            .map_err(control_error)?;
        core.validate_execution().map_err(control_error)?;

        let mut removed_credentials = Vec::new();
        let mut absent_credentials = Vec::new();
        for id in core.credential_ids() {
            match delete_secret(id).map_err(control_error)? {
                true => removed_credentials.push(id.clone()),
                false => absent_credentials.push(id.clone()),
            }
        }
        let removed = core
            .execute_files()
            .map_err(control_error)?
            .into_iter()
            .map(|target| DesktopResetTarget {
                label: target.label.to_owned(),
                path: target.path.display().to_string(),
            })
            .collect();
        Ok(DesktopResetReceipt {
            semantic_code: "reset.completed.v1".to_owned(),
            removed,
            removed_credentials,
            absent_credentials,
            preserved: current.preserved,
        })
    }

    pub fn diagnostics_snapshot(&self) -> Result<DesktopDiagnosticsSnapshot, DesktopError> {
        let health = diagnostics::inspect(&self.paths);
        let entries = diagnostics::list(&self.paths)
            .map_err(control_error)?
            .into_iter()
            .map(|entry| DesktopDiagnosticEntry {
                name: entry.name,
                kind: entry.kind.to_owned(),
                bytes: entry.bytes,
                modified_ms: entry.modified_ms,
            })
            .collect();
        Ok(DesktopDiagnosticsSnapshot {
            version: MAINTENANCE_VERSION,
            enabled: health.enabled,
            log_directory: health.log_dir.display().to_string(),
            crash_directory: health.crash_dir.display().to_string(),
            retained_files: health.retained_files,
            retained_bytes: health.retained_bytes,
            retention_compliant: health.retention_compliant,
            permissions_private: health.permissions_private,
            stale_markers: health.stale_markers,
            invalid_reports: health.invalid_reports,
            dropped_events: health.dropped_events,
            writer_faults: health.writer_faults,
            entries,
        })
    }

    pub fn export_support_bundle(&self, output: PathBuf) -> Result<String, DesktopError> {
        diagnostics::export_support_bundle(&self.paths, &output)
            .map(|()| output.display().to_string())
            .map_err(control_error)
    }
}

fn project_doctor(report: doctor::DoctorReport) -> DesktopDoctorSnapshot {
    let findings = report
        .findings
        .into_iter()
        .map(|finding| {
            let repairable = finding.repairable();
            DesktopDoctorFinding {
                code: finding.code,
                severity: match finding.severity {
                    doctor::Severity::Ok => DesktopDoctorSeverity::Ok,
                    doctor::Severity::Info => DesktopDoctorSeverity::Info,
                    doctor::Severity::Warning => DesktopDoctorSeverity::Warning,
                    doctor::Severity::Error => DesktopDoctorSeverity::Error,
                },
                summary: finding.summary,
                evidence: finding.evidence,
                action: finding.action,
                repairable,
            }
        })
        .collect::<Vec<_>>();
    let repairable_codes = findings
        .iter()
        .filter(|finding| finding.repairable)
        .map(|finding| finding.code.clone())
        .collect();
    DesktopDoctorSnapshot {
        version: MAINTENANCE_VERSION,
        findings,
        repairable_codes,
    }
}

fn project_migration(
    plan: crate::config_migration::ConfigMigrationPlan,
) -> DesktopMigrationSnapshot {
    project_migration_ref(&plan)
}

fn project_migration_ref(
    plan: &crate::config_migration::ConfigMigrationPlan,
) -> DesktopMigrationSnapshot {
    DesktopMigrationSnapshot {
        version: MAINTENANCE_VERSION,
        source_version: plan.source_version,
        target_version: plan.target_version,
        requires_apply: plan.requires_apply(),
        private_recovery_pending: plan.private_recovery_pending,
        private_records: plan
            .private_records
            .iter()
            .map(|record| DesktopPrivateMigrationRecord {
                name: record.name.to_owned(),
                version: record.version,
                status: record.status.as_str().to_owned(),
            })
            .collect(),
    }
}

fn core_reset_scope(scope: DesktopResetScope) -> ResetScope {
    match scope {
        DesktopResetScope::Setup => ResetScope::Setup,
        DesktopResetScope::Sessions => ResetScope::Sessions,
        DesktopResetScope::Caches => ResetScope::Caches,
        DesktopResetScope::Credentials => ResetScope::Credentials,
    }
}

fn project_reset(plan: &ResetPlan) -> DesktopResetPlan {
    let scopes = plan
        .scopes()
        .iter()
        .map(|scope| match scope {
            ResetScope::Setup => DesktopResetScope::Setup,
            ResetScope::Sessions => DesktopResetScope::Sessions,
            ResetScope::Caches => DesktopResetScope::Caches,
            ResetScope::Credentials => DesktopResetScope::Credentials,
        })
        .collect::<Vec<_>>();
    let preserved = reset_preservation(plan.scopes());
    DesktopResetPlan {
        version: MAINTENANCE_VERSION,
        scopes,
        targets: plan
            .targets()
            .iter()
            .map(|target| DesktopResetTarget {
                label: target.label.to_owned(),
                path: target.path.display().to_string(),
            })
            .collect(),
        credential_ids: plan.credential_ids().to_vec(),
        preserved,
        requires_files_confirmation: !plan.targets().is_empty(),
        requires_credentials_confirmation: !plan.credential_ids().is_empty(),
    }
}

fn reset_preservation(scopes: &BTreeSet<ResetScope>) -> Vec<String> {
    let mut preserved = vec![
        "Workspace files and directories".to_owned(),
        "Provider-owned authentication and conversation history".to_owned(),
        "Unrelated files under Xana directories".to_owned(),
    ];
    if !scopes.contains(&ResetScope::Sessions) {
        preserved.push("Native conversations and artifacts".to_owned());
    }
    if !scopes.contains(&ResetScope::Credentials) {
        preserved.push("API keys in the operating-system credential store".to_owned());
    }
    if !scopes.contains(&ResetScope::Setup) {
        preserved.push("Configuration and presentation preferences".to_owned());
    }
    preserved
}

fn conflict(message: &str) -> DesktopError {
    DesktopError::new(DesktopErrorCode::StateInvalid, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};
    use tempfile::tempdir;

    fn control() -> (tempfile::TempDir, DesktopControlPlane) {
        let directory = tempdir().unwrap();
        let paths =
            crate::paths::XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        (directory, DesktopControlPlane { paths })
    }

    fn write_v3(control: &DesktopControlPlane) {
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        let text = XanaConfig::render_initial(InitialConfig {
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
        .unwrap()
        .replacen("version = 5", "version = 3", 1);
        fs::write(control.paths.config_file(), text).unwrap();
    }

    #[tokio::test]
    async fn doctor_remains_available_without_configuration() {
        let (_directory, control) = control();
        let snapshot = control.doctor_snapshot(false).await.unwrap();
        assert!(
            snapshot
                .findings
                .iter()
                .any(|finding| finding.code == "config.missing")
        );
        assert!(snapshot.repairable_codes.iter().all(|code| {
            snapshot
                .findings
                .iter()
                .any(|finding| finding.code == *code && finding.repairable)
        }));
    }

    #[test]
    fn migration_requires_the_exact_review_and_creates_a_backup() {
        let (_directory, control) = control();
        write_v3(&control);
        let preview = control.migration_snapshot().unwrap();
        assert!(preview.requires_apply);
        let receipt = control.apply_migration(&preview).unwrap();
        assert!(receipt.changed_config);
        assert!(PathBuf::from(receipt.backup_path).is_file());
        assert!(!control.migration_snapshot().unwrap().requires_apply);
    }

    #[test]
    fn reset_requires_exact_confirmation_and_preserves_unselected_state() {
        let (_directory, control) = control();
        write_v3(&control);
        let sessions = control.paths.data_dir().join("sessions/conversation.jsonl");
        fs::create_dir_all(sessions.parent().unwrap()).unwrap();
        fs::write(&sessions, b"retained").unwrap();
        let preview = control.reset_plan(&[DesktopResetScope::Setup]).unwrap();
        assert!(preview.requires_files_confirmation);
        assert!(
            control
                .execute_reset(&preview, false, false)
                .unwrap_err()
                .message
                .contains("explicit confirmation")
        );
        let receipt = control.execute_reset(&preview, true, false).unwrap();
        assert_eq!(receipt.semantic_code, "reset.completed.v1");
        assert!(sessions.is_file());
        assert!(!control.paths.config_file().exists());
    }

    #[test]
    fn diagnostics_and_support_export_are_bounded_and_secret_free() {
        let (directory, control) = control();
        let snapshot = control.diagnostics_snapshot().unwrap();
        assert_eq!(snapshot.retained_files, 0);
        let output = directory.path().join("support.json");
        let rendered = control.export_support_bundle(output.clone()).unwrap();
        assert_eq!(PathBuf::from(rendered), output);
        let contents = fs::read_to_string(output).unwrap();
        assert!(contents.contains("metadata-only"));
        assert!(!contents.contains("super-secret-fixture-value"));
    }
}
