//! Live provider and managed-runtime health probes.

use super::{DoctorReport, Finding, Severity};
use crate::{
    config::{ConnectionConfig, CredentialReference, ProviderKind, XanaConfig},
    credential::CredentialAvailability,
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig},
    model_catalog::ModelManager,
    paths::XanaPaths,
};
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) async fn inspect(paths: &XanaPaths, report: &mut DoctorReport) {
    let registry = match XanaConfig::load_registry_from(paths.config_file()) {
        Ok(registry) => registry,
        Err(_) => return,
    };
    let manager = ModelManager::new(
        registry.clone(),
        paths.cache_dir().to_owned(),
        paths.data_dir().join("selection.toml"),
    );
    match manager.selected() {
        Ok(selection) => report.push(Finding::new(
            "model.selection",
            Severity::Ok,
            "the selected connection/model resolves locally",
            format!("{}/{}", selection.connection, selection.model),
            None,
        )),
        Err(_) => report.push(Finding::new(
            "model.selection",
            Severity::Error,
            "the selected connection/model does not resolve against configured and cached metadata",
            paths
                .data_dir()
                .join("selection.toml")
                .display()
                .to_string(),
            Some("xana setup --section connection".into()),
        )),
    }

    for connection in registry.connections.values() {
        inspect_credential(&manager, connection, report);
        if connection.kind == ProviderKind::Codex {
            inspect_codex(connection, report).await;
        } else if !credential_is_missing(&manager, connection) {
            inspect_native(&manager, connection, report).await;
        }
    }
}

fn inspect_credential(
    manager: &ModelManager,
    connection: &ConnectionConfig,
    report: &mut DoctorReport,
) {
    let code = format!("connection.{}.credential", connection.id);
    if connection.kind == ProviderKind::Codex {
        report.push(Finding::new(
            code,
            Severity::Info,
            "Codex owns account credentials outside Xana",
            format!("connection={} kind=codex", connection.id),
            Some(format!("xana connection status {}", connection.id)),
        ));
        return;
    }
    match manager.credential_availability(connection) {
        Ok(CredentialAvailability::Available) => report.push(Finding::new(
            code,
            Severity::Ok,
            "the declared credential source is available",
            credential_evidence(connection),
            None,
        )),
        Ok(CredentialAvailability::Missing) => report.push(Finding::new(
            code,
            Severity::Error,
            "the declared credential source is missing or empty",
            credential_evidence(connection),
            Some(credential_action(connection)),
        )),
        Err(_) => report.push(Finding::new(
            code,
            Severity::Error,
            "the declared credential backend could not be inspected",
            credential_evidence(connection),
            Some(credential_action(connection)),
        )),
    }
}

fn credential_is_missing(manager: &ModelManager, connection: &ConnectionConfig) -> bool {
    !matches!(
        manager.credential_availability(connection),
        Ok(CredentialAvailability::Available)
    )
}

fn credential_evidence(connection: &ConnectionConfig) -> String {
    match &connection.credential {
        Some(CredentialReference::Environment { variable }) => {
            format!("connection={} environment={variable}", connection.id)
        }
        Some(CredentialReference::Stored { id }) => {
            format!("connection={} stored_reference={id}", connection.id)
        }
        None => format!("connection={} no credential required", connection.id),
    }
}

fn credential_action(connection: &ConnectionConfig) -> String {
    match &connection.credential {
        Some(CredentialReference::Environment { variable }) => {
            format!("set {variable} and rerun xana doctor")
        }
        Some(CredentialReference::Stored { .. }) => {
            format!("xana connection set-key {}", connection.id)
        }
        None => format!("xana setup --section connection # {}", connection.id),
    }
}

async fn inspect_native(
    manager: &ModelManager,
    connection: &ConnectionConfig,
    report: &mut DoctorReport,
) {
    let result =
        tokio::time::timeout(PROBE_TIMEOUT, manager.probe_native(&connection.id, None)).await;
    match result {
        Ok(Ok(models)) => report.push(Finding::new(
            format!("connection.{}.catalog", connection.id),
            Severity::Ok,
            "the live connection catalog is reachable",
            format!(
                "connection={} kind={} models={}",
                connection.id,
                connection.kind.as_str(),
                models.len()
            ),
            None,
        )),
        Ok(Err(_)) | Err(_) => report.push(Finding::new(
            format!("connection.{}.catalog", connection.id),
            Severity::Error,
            "the live connection catalog probe failed; response contents are omitted",
            format!(
                "connection={} kind={}",
                connection.id,
                connection.kind.as_str()
            ),
            Some(format!("xana model refresh {}", connection.id)),
        )),
    }
}

async fn inspect_codex(connection: &ConnectionConfig, report: &mut DoctorReport) {
    let launch = CodexLaunchConfig {
        program: connection
            .codex_program
            .clone()
            .unwrap_or_else(|| "codex".into()),
        home: connection.codex_home.clone(),
    };
    let spawn = tokio::time::timeout(PROBE_TIMEOUT, CodexAppServer::spawn(&launch)).await;
    let mut server = match spawn {
        Ok(Ok(server)) => server,
        Ok(Err(_)) | Err(_) => {
            report.push(Finding::new(
                format!("connection.{}.runtime", connection.id),
                Severity::Error,
                "Codex executable/version/app-server handshake failed",
                format!("connection={} program={}", connection.id, launch.program),
                Some(format!("xana connection status {}", connection.id)),
            ));
            return;
        }
    };
    report.push(Finding::new(
        format!("connection.{}.runtime", connection.id),
        Severity::Ok,
        "Codex executable and app-server handshake succeeded",
        format!(
            "connection={} runtime={} home={}",
            connection.id,
            server.version,
            server.codex_home.display()
        ),
        None,
    ));

    let account = tokio::time::timeout(PROBE_TIMEOUT, server.account_status()).await;
    let chatgpt = matches!(&account, Ok(Ok(AccountStatus::ChatGpt { .. })));
    match account {
        Ok(Ok(AccountStatus::LoggedOut)) => report.push(Finding::new(
            format!("connection.{}.account", connection.id),
            Severity::Warning,
            "Codex reports no active account",
            format!("connection={}", connection.id),
            Some(format!("xana connection login {}", connection.id)),
        )),
        Ok(Ok(AccountStatus::ChatGpt { plan })) => report.push(Finding::new(
            format!("connection.{}.account", connection.id),
            Severity::Ok,
            "Codex reports an active ChatGPT account",
            format!("connection={} plan={plan}", connection.id),
            None,
        )),
        Ok(Ok(AccountStatus::ApiKey)) => report.push(Finding::new(
            format!("connection.{}.account", connection.id),
            Severity::Ok,
            "Codex reports an app-server-managed API key",
            format!("connection={}", connection.id),
            None,
        )),
        Ok(Ok(AccountStatus::Other { kind })) => report.push(Finding::new(
            format!("connection.{}.account", connection.id),
            Severity::Info,
            "Codex reports another supported account kind",
            format!("connection={} account_kind={kind}", connection.id),
            None,
        )),
        Ok(Err(_)) | Err(_) => report.push(Finding::new(
            format!("connection.{}.account", connection.id),
            Severity::Error,
            "Codex account status could not be read",
            format!("connection={}", connection.id),
            Some(format!("xana connection status {}", connection.id)),
        )),
    }

    match tokio::time::timeout(PROBE_TIMEOUT, server.models()).await {
        Ok(Ok(models)) => report.push(Finding::new(
            format!("connection.{}.catalog", connection.id),
            Severity::Ok,
            "Codex advertised a live model catalog",
            format!("connection={} models={}", connection.id, models.len()),
            None,
        )),
        Ok(Err(_)) | Err(_) => report.push(Finding::new(
            format!("connection.{}.catalog", connection.id),
            Severity::Error,
            "Codex live model catalog could not be read",
            format!("connection={}", connection.id),
            Some(format!("xana model refresh {}", connection.id)),
        )),
    }
    if chatgpt {
        match tokio::time::timeout(PROBE_TIMEOUT, server.rate_limits()).await {
            Ok(Ok(limits)) => {
                let primary = limits
                    .pointer("/rateLimits/primary/usedPercent")
                    .and_then(serde_json::Value::as_f64);
                let secondary = limits
                    .pointer("/rateLimits/secondary/usedPercent")
                    .and_then(serde_json::Value::as_f64);
                report.push(Finding::new(
                    format!("connection.{}.rate_limits", connection.id),
                    Severity::Info,
                    "Codex rate-limit telemetry is available",
                    format!(
                        "connection={} primary={} secondary={}",
                        connection.id,
                        percent(primary),
                        percent(secondary)
                    ),
                    None,
                ));
            }
            Ok(Err(_)) | Err(_) => report.push(Finding::new(
                format!("connection.{}.rate_limits", connection.id),
                Severity::Warning,
                "Codex rate-limit telemetry could not be read",
                format!("connection={}", connection.id),
                Some(format!("xana connection status {}", connection.id)),
            )),
        }
    }
    let _ = tokio::time::timeout(PROBE_TIMEOUT, server.shutdown()).await;
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| format!("{value:.0}%"))
}
