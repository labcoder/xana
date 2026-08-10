//! Read-only installation diagnostics and explicitly bounded repairs.

mod connections;

use crate::{
    config::XanaConfig,
    local_host::{DescriptorHealth, inspect_descriptor_health, remove_stale_descriptor},
    paths::XanaPaths,
    presentation::PresentationPreferences,
};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const REPORT_VERSION: u16 = 1;
const MAX_FIELD_CHARS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Ok,
    Info,
    Warning,
    Error,
}

impl Severity {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Repair {
    #[cfg(unix)]
    ProtectPath {
        path: PathBuf,
        directory: bool,
    },
    RemoveStaleDescriptor {
        workspace: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Finding {
    pub(crate) code: String,
    pub(crate) severity: Severity,
    pub(crate) summary: String,
    pub(crate) evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<String>,
    #[serde(skip)]
    repair: Option<Repair>,
}

impl Finding {
    fn new(
        code: impl Into<String>,
        severity: Severity,
        summary: impl Into<String>,
        evidence: impl Into<String>,
        action: Option<String>,
    ) -> Self {
        Self {
            code: bounded(code.into()),
            severity,
            summary: bounded(summary.into()),
            evidence: bounded(evidence.into()),
            action: action.map(bounded),
            repair: None,
        }
    }

    fn with_repair(mut self, repair: Repair) -> Self {
        self.repair = Some(repair);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DoctorReport {
    version: u16,
    pub(crate) findings: Vec<Finding>,
}

impl DoctorReport {
    fn new() -> Self {
        Self {
            version: REPORT_VERSION,
            findings: Vec::new(),
        }
    }

    fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    pub(crate) fn repairs(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.repair.is_some())
    }

    pub(crate) fn apply_repairs(&self, paths: &XanaPaths) -> Vec<(String, Result<bool, String>)> {
        self.repairs()
            .map(|finding| {
                let result = match finding.repair.as_ref().expect("repair was filtered") {
                    #[cfg(unix)]
                    Repair::ProtectPath { path, directory } => {
                        protect_path(path, *directory).map(|()| true)
                    }
                    Repair::RemoveStaleDescriptor { workspace } => {
                        remove_stale_descriptor(paths.runtime_dir(), workspace)
                    }
                };
                (finding.code.clone(), result)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TerminalHealth {
    pub(crate) input_is_terminal: bool,
    pub(crate) output_is_terminal: bool,
    pub(crate) dumb: bool,
}

pub(crate) async fn inspect(paths: &XanaPaths, terminal: TerminalHealth) -> DoctorReport {
    let mut report = DoctorReport::new();
    inspect_configuration(paths, &mut report);
    inspect_owned_paths(paths, &mut report);
    inspect_presentation(paths, &mut report);
    inspect_terminal(terminal, &mut report);
    inspect_descriptor(paths, &mut report);
    connections::inspect(paths, &mut report).await;
    report
}

fn inspect_configuration(paths: &XanaPaths, report: &mut DoctorReport) {
    match XanaConfig::load_from(paths.config_file()) {
        Ok(_) => report.push(Finding::new(
            "config.valid",
            Severity::Ok,
            "configuration parsed and passed the current schema",
            paths.config_file().display().to_string(),
            None,
        )),
        Err(error) if error.is_missing_config() => report.push(Finding::new(
            "config.missing",
            Severity::Error,
            "Xana has no installed configuration",
            paths.config_file().display().to_string(),
            Some("xana setup".into()),
        )),
        Err(_) => report.push(Finding::new(
            "config.invalid",
            Severity::Error,
            "configuration could not be parsed or validated; source text is omitted to protect secrets",
            paths.config_file().display().to_string(),
            Some("xana config edit".into()),
        )),
    }
}

fn inspect_owned_paths(paths: &XanaPaths, report: &mut DoctorReport) {
    let config_parent = paths
        .config_file()
        .parent()
        .unwrap_or_else(|| Path::new("."));
    for (code, label, path, directory) in [
        (
            "path.config_dir",
            "configuration directory",
            config_parent,
            true,
        ),
        ("path.data", "data directory", paths.data_dir(), true),
        ("path.cache", "cache directory", paths.cache_dir(), true),
        (
            "path.runtime",
            "runtime directory",
            paths.runtime_dir(),
            true,
        ),
        (
            "path.config",
            "configuration file",
            paths.config_file(),
            false,
        ),
    ] {
        inspect_owned_path(code, label, path, directory, report);
    }
}

fn inspect_owned_path(
    code: &str,
    label: &str,
    path: &Path,
    directory: bool,
    report: &mut DoctorReport,
) {
    if !path.is_absolute() {
        report.push(Finding::new(
            code,
            Severity::Error,
            format!("{label} is not absolute"),
            path.display().to_string(),
            Some("set XANA_HOME to an absolute native path or remove the override".into()),
        ));
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            report.push(Finding::new(
                code,
                Severity::Info,
                format!("{label} does not exist yet"),
                path.display().to_string(),
                None,
            ));
            return;
        }
        Err(_) => {
            report.push(Finding::new(
                code,
                Severity::Error,
                format!("{label} metadata is unreadable"),
                path.display().to_string(),
                Some("repair filesystem access before running Xana".into()),
            ));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        report.push(Finding::new(
            code,
            Severity::Warning,
            format!("{label} is a symbolic link and will never be followed by repair/reset"),
            path.display().to_string(),
            Some("inspect the link target manually".into()),
        ));
        return;
    }
    if directory != metadata.is_dir() {
        report.push(Finding::new(
            code,
            Severity::Error,
            format!("{label} has the wrong filesystem type"),
            path.display().to_string(),
            Some("move the conflicting entry and rerun xana setup".into()),
        ));
        return;
    }
    inspect_permissions(code, label, path, directory, &metadata, report);
}

#[cfg(unix)]
fn inspect_permissions(
    code: &str,
    label: &str,
    path: &Path,
    directory: bool,
    metadata: &fs::Metadata,
    report: &mut DoctorReport,
) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        report.push(Finding::new(
            code,
            Severity::Ok,
            format!("{label} is owner-only"),
            format!("{} mode {mode:o}", path.display()),
            None,
        ));
    } else {
        report.push(
            Finding::new(
                code,
                Severity::Warning,
                format!("{label} permits group or other access"),
                format!("{} mode {mode:o}", path.display()),
                Some("xana doctor --fix".into()),
            )
            .with_repair(Repair::ProtectPath {
                path: path.to_owned(),
                directory,
            }),
        );
    }
}

#[cfg(not(unix))]
fn inspect_permissions(
    code: &str,
    label: &str,
    path: &Path,
    _directory: bool,
    _metadata: &fs::Metadata,
    report: &mut DoctorReport,
) {
    report.push(Finding::new(
        code,
        Severity::Info,
        format!("{label} is governed by Windows ACLs that Xana does not rewrite or fully audit"),
        path.display().to_string(),
        Some("inspect ACLs manually when XANA_HOME points at shared storage".into()),
    ));
}

fn inspect_presentation(paths: &XanaPaths, report: &mut DoctorReport) {
    let loaded = PresentationPreferences::load(&paths.presentation_file());
    report.push(if loaded.warning.is_some() {
        Finding::new(
            "presentation.invalid",
            Severity::Warning,
            "presentation preferences are invalid; Xana is using safe defaults",
            paths.presentation_file().display().to_string(),
            Some("xana setup --section appearance".into()),
        )
    } else {
        Finding::new(
            "presentation.valid",
            Severity::Ok,
            "presentation preferences are valid or absent",
            paths.presentation_file().display().to_string(),
            None,
        )
    });
}

fn inspect_terminal(terminal: TerminalHealth, report: &mut DoctorReport) {
    let severity = if terminal.dumb || !(terminal.input_is_terminal && terminal.output_is_terminal)
    {
        Severity::Info
    } else {
        Severity::Ok
    };
    let surface = if terminal.dumb {
        "plain fallback because TERM is dumb"
    } else if terminal.input_is_terminal && terminal.output_is_terminal {
        "interactive terminal supports adaptive TUI selection"
    } else {
        "noninteractive stream uses permanent plain output"
    };
    report.push(Finding::new(
        "terminal.surface",
        severity,
        surface,
        format!(
            "stdin_tty={} stdout_tty={}",
            terminal.input_is_terminal, terminal.output_is_terminal
        ),
        None,
    ));
}

fn inspect_descriptor(paths: &XanaPaths, report: &mut DoctorReport) {
    let workspace = match std::env::current_dir().and_then(|path| path.canonicalize()) {
        Ok(path) => path,
        Err(_) => {
            report.push(Finding::new(
                "host.workspace",
                Severity::Error,
                "current workspace cannot be resolved",
                "process current directory",
                Some("change to an accessible workspace and rerun xana doctor".into()),
            ));
            return;
        }
    };
    match inspect_descriptor_health(paths.runtime_dir(), &workspace) {
        Ok(DescriptorHealth::Absent) => report.push(Finding::new(
            "host.descriptor",
            Severity::Ok,
            "no foreground-host descriptor is present for this workspace",
            workspace.display().to_string(),
            None,
        )),
        Ok(DescriptorHealth::Active {
            process_id,
            endpoint,
        }) => report.push(Finding::new(
            "host.descriptor",
            Severity::Ok,
            "a lock-backed foreground-host descriptor is active",
            format!("process={process_id} endpoint={endpoint}"),
            None,
        )),
        Ok(DescriptorHealth::Stale { path, reason }) => report.push(
            Finding::new(
                "host.descriptor_stale",
                Severity::Warning,
                "the workspace host descriptor has no active lock owner",
                format!("{} ({reason})", path.display()),
                Some("xana doctor --fix".into()),
            )
            .with_repair(Repair::RemoveStaleDescriptor { workspace }),
        ),
        Ok(DescriptorHealth::InvalidActive { path, reason }) => report.push(Finding::new(
            "host.descriptor_invalid_active",
            Severity::Error,
            "a locked or unsafe foreground-host descriptor cannot be repaired automatically",
            format!("{} ({reason})", path.display()),
            Some("stop the owning Xana process and rerun xana doctor".into()),
        )),
        Err(_) => report.push(Finding::new(
            "host.descriptor_unreadable",
            Severity::Error,
            "foreground-host state could not be inspected",
            paths.runtime_dir().display().to_string(),
            Some("inspect the Xana runtime directory manually".into()),
        )),
    }
}

fn bounded(value: String) -> String {
    if value.chars().count() <= MAX_FIELD_CHARS {
        return value;
    }
    let mut shortened = value.chars().take(MAX_FIELD_CHARS - 1).collect::<String>();
    shortened.push('…');
    shortened
}

#[cfg(unix)]
fn protect_path(path: &Path, directory: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect repair target: {error}"))?;
    if metadata.file_type().is_symlink() || metadata.is_dir() != directory {
        return Err("repair target changed type or became a symbolic link".into());
    }
    let mode = if directory { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("could not protect repair target: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection, PermissionMode};
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, XanaPaths) {
        let directory = tempdir().unwrap();
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).unwrap();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
            },
            model: "fixture".into(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
            shell: crate::shell::ShellConfig::default(),
            max_tool_rounds: 8,
        })
        .unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), rendered).unwrap();
        (directory, paths)
    }

    #[tokio::test]
    async fn report_is_read_only_redacted_and_versioned() {
        let (_directory, paths) = fixture();
        let before = fs::read(paths.config_file()).unwrap();
        let report = inspect(
            &paths,
            TerminalHealth {
                input_is_terminal: false,
                output_is_terminal: false,
                dumb: false,
            },
        )
        .await;
        let json = serde_json::to_string(&report).unwrap();

        assert!(json.contains("\"version\":1"));
        assert!(json.contains("connection.local.catalog"));
        assert!(!json.contains("api_key"));
        assert_eq!(fs::read(paths.config_file()).unwrap(), before);
        assert!(!paths.cache_dir().exists());
        assert!(!paths.runtime_dir().exists());
    }

    #[test]
    fn invalid_config_does_not_echo_untrusted_source_text() {
        let (_directory, paths) = fixture();
        fs::write(paths.config_file(), "api_key = \"seeded-secret\"\n").unwrap();
        let mut report = DoctorReport::new();
        inspect_configuration(&paths, &mut report);
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(encoded.contains("config.invalid"));
        assert!(!encoded.contains("seeded-secret"));
    }

    #[test]
    fn stale_descriptor_repair_is_exact_and_idempotent() {
        let (_directory, paths) = fixture();
        let workspace = tempdir().unwrap();
        let canonical = workspace.path().canonicalize().unwrap();
        crate::local_host::write_stale_for_test(paths.runtime_dir(), &canonical);

        assert!(remove_stale_descriptor(paths.runtime_dir(), &canonical).unwrap());
        assert!(!remove_stale_descriptor(paths.runtime_dir(), &canonical).unwrap());
    }

    #[test]
    fn bounded_fields_do_not_grow_without_limit() {
        let value = bounded("x".repeat(MAX_FIELD_CHARS * 2));
        assert_eq!(value.chars().count(), MAX_FIELD_CHARS);
        assert!(value.ends_with('…'));
    }
}
