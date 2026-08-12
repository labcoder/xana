//! Recovery and configuration commands at the application edge.

use super::load_config;
use crate::{
    cli::{self, ConfigCommand, OutputChoice},
    config::{CredentialReference, XanaConfig},
    credential::delete_secret,
    paths::XanaPaths,
    reset::{ResetPlan, ResetScope},
};
use anyhow::{Context, Result};
use std::io::{self, BufRead, IsTerminal, Write};

pub(super) fn run_config_command<W: Write>(
    command: ConfigCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            writeln!(output, "{}", paths.config_file().display())?;
            Ok(())
        }
        ConfigCommand::Check => {
            load_config(paths)?;
            let plan = crate::config_migration::ConfigMigrationPlan::build(paths)?;
            writeln!(
                output,
                "configuration is valid: {}",
                paths.config_file().display()
            )?;
            writeln!(
                output,
                "schema: {} (current {}){}",
                plan.source_version,
                plan.target_version,
                if plan.requires_apply() {
                    "; migration pending"
                } else {
                    ""
                }
            )?;
            for record in &plan.private_records {
                writeln!(
                    output,
                    "private state: {} = {}{}",
                    record.name,
                    record.status.as_str(),
                    record
                        .version
                        .map(|version| format!(" (version {version})"))
                        .unwrap_or_default()
                )?;
            }
            Ok(())
        }
        ConfigCommand::Migrate { apply } => {
            let plan = crate::config_migration::ConfigMigrationPlan::build(paths)?;
            writeln!(output, "Xana configuration migration")?;
            writeln!(
                output,
                "  Schema: {} -> {}",
                plan.source_version, plan.target_version
            )?;
            writeln!(
                output,
                "  Config: {}",
                if plan.source_version == plan.target_version {
                    "current"
                } else {
                    "comment-preserving version update"
                }
            )?;
            for record in &plan.private_records {
                writeln!(output, "  {}: {}", record.name, record.status.as_str())?;
            }
            if !apply {
                writeln!(
                    output,
                    "review only; run `xana config migrate --apply` to commit"
                )?;
                return Ok(());
            }
            let outcome = plan.apply(paths)?;
            writeln!(output, "migration committed atomically")?;
            writeln!(output, "  Backup: {}", outcome.backup_path.display())?;
            writeln!(
                output,
                "  Private records initialized: {}",
                outcome.initialized_private_records
            )?;
            Ok(())
        }
        ConfigCommand::Edit { editor } => {
            let editor = resolve_editor(editor)?;
            crate::config_edit::edit(paths.config_file(), &editor)?;
            writeln!(
                output,
                "validated configuration installed atomically: {}",
                paths.config_file().display()
            )?;
            writeln!(
                output,
                "backup: {}",
                paths.config_file().with_extension("toml.bak").display()
            )?;
            writeln!(
                output,
                "changes apply to a new conversation; active conversations are never mutated"
            )?;
            Ok(())
        }
    }
}

fn resolve_editor(requested: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    if let Some(editor) = requested {
        return Ok(editor);
    }
    for variable in ["XANA_EDITOR", "VISUAL", "EDITOR"] {
        let Some(value) = std::env::var_os(variable) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        return Ok(value.into());
    }
    Ok(if cfg!(windows) {
        "notepad.exe".into()
    } else {
        "vi".into()
    })
}

pub(super) fn run_reset_with_io<R: BufRead, W: Write>(
    args: &cli::ResetArgs,
    paths: &XanaPaths,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<()> {
    let scopes = resolve_reset_scopes(args, is_terminal, input, output)?;
    if args.credentials_yes && !scopes.contains(&ResetScope::Credentials) {
        anyhow::bail!("--credentials-yes requires --scope credentials or --scope all")
    }
    let credential_ids = reset_credential_ids(paths, &scopes)?;
    let plan = ResetPlan::inspect(paths, scopes, credential_ids)
        .context("could not inspect selected Xana reset scopes")?;
    writeln!(
        output,
        "Reset scopes: {}",
        plan.scopes()
            .iter()
            .map(|scope| scope.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    if plan.is_empty() {
        writeln!(output, "Selected Xana state is already clear.")?;
        return Ok(());
    }

    writeln!(output, "Reset will remove:")?;
    for target in plan.targets() {
        writeln!(output, "  {}: {}", target.label, target.path.display())?;
    }
    for credential_id in plan.credential_ids() {
        writeln!(
            output,
            "  referenced OS credential: {credential_id} (separate confirmation required)"
        )?;
    }
    write_reset_preservation(output, &plan)?;
    if args.dry_run {
        writeln!(output, "Dry run only; no state or credentials changed.")?;
        return Ok(());
    }

    if !args.yes && !plan.targets().is_empty() {
        if !is_terminal {
            anyhow::bail!("reset requires --yes when stdin is not a terminal")
        }
        write!(output, "Remove the listed Xana filesystem state? [y/N]: ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0
            || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        {
            writeln!(output, "No changes made.")?;
            return Ok(());
        }
    }

    let delete_credentials = if plan.credential_ids().is_empty() {
        false
    } else if args.credentials_yes {
        true
    } else {
        if !is_terminal {
            anyhow::bail!(
                "credential reset requires separate --credentials-yes confirmation when stdin is not a terminal"
            )
        }
        write!(
            output,
            "Delete {} referenced credential(s) from the OS credential store? [y/N]: ",
            plan.credential_ids().len()
        )?;
        output.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)? != 0
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };
    if delete_credentials {
        plan.validate_execution()
            .context("reset safety check found active workspace state")?;
        for id in plan.credential_ids() {
            let removed = delete_secret(id)?;
            writeln!(
                output,
                "  credential {id}: {}",
                if removed { "removed" } else { "already absent" }
            )?;
        }
    } else if !plan.credential_ids().is_empty() {
        writeln!(output, "Referenced credentials preserved.")?;
    }

    let setup_removed = plan.scopes().contains(&ResetScope::Setup);
    let removed = plan
        .execute_files()
        .context("could not reset selected Xana state")?;
    writeln!(output, "Selected Xana state reset.")?;
    for target in removed {
        writeln!(
            output,
            "  removed {}: {}",
            target.label,
            target.path.display()
        )?;
    }
    if setup_removed {
        writeln!(output, "Next: xana setup")?;
        writeln!(output, "Source checkout: cargo run -- setup")?;
    }
    Ok(())
}

fn resolve_reset_scopes<R: BufRead, W: Write>(
    args: &cli::ResetArgs,
    is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<std::collections::BTreeSet<ResetScope>> {
    let selected = if args.scope.is_empty() && is_terminal && !args.yes && !args.dry_run {
        write!(
            output,
            "Reset scope [setup/sessions/caches/credentials/all] (setup): "
        )?;
        output.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        match answer.trim() {
            "" | "setup" => vec![cli::ResetScopeChoice::Setup],
            "sessions" => vec![cli::ResetScopeChoice::Sessions],
            "caches" => vec![cli::ResetScopeChoice::Caches],
            "credentials" => vec![cli::ResetScopeChoice::Credentials],
            "all" => vec![cli::ResetScopeChoice::All],
            value => anyhow::bail!(
                "unknown reset scope {value:?}; use setup, sessions, caches, credentials, or all"
            ),
        }
    } else if args.scope.is_empty() {
        vec![cli::ResetScopeChoice::Setup]
    } else {
        args.scope.clone()
    };
    let mut scopes = std::collections::BTreeSet::new();
    for scope in selected {
        match scope {
            cli::ResetScopeChoice::Setup => {
                scopes.insert(ResetScope::Setup);
            }
            cli::ResetScopeChoice::Sessions => {
                scopes.insert(ResetScope::Sessions);
            }
            cli::ResetScopeChoice::Caches => {
                scopes.insert(ResetScope::Caches);
            }
            cli::ResetScopeChoice::Credentials => {
                scopes.insert(ResetScope::Credentials);
            }
            cli::ResetScopeChoice::All => {
                scopes.extend([
                    ResetScope::Setup,
                    ResetScope::Sessions,
                    ResetScope::Caches,
                    ResetScope::Credentials,
                ]);
            }
        }
    }
    Ok(scopes)
}

fn reset_credential_ids(
    paths: &XanaPaths,
    scopes: &std::collections::BTreeSet<ResetScope>,
) -> Result<Vec<String>> {
    if !scopes.contains(&ResetScope::Credentials) {
        return Ok(Vec::new());
    }
    let registry = XanaConfig::load_registry_from(paths.config_file()).context(
        "credential reset requires a valid config so Xana can enumerate only referenced stored credentials",
    )?;
    Ok(registry
        .connections
        .values()
        .filter_map(|connection| match &connection.credential {
            Some(CredentialReference::Stored { id }) => Some(id.clone()),
            Some(CredentialReference::Environment { .. }) | None => None,
        })
        .collect())
}

fn write_reset_preservation<W: Write>(output: &mut W, plan: &ResetPlan) -> Result<()> {
    writeln!(output, "Reset preserves:")?;
    if !plan.scopes().contains(&ResetScope::Sessions) {
        writeln!(output, "  native sessions and artifacts")?;
        if !plan.scopes().contains(&ResetScope::Setup) {
            writeln!(output, "  managed-thread handles")?;
        }
    }
    if !plan.scopes().contains(&ResetScope::Credentials) {
        writeln!(output, "  API keys in the OS credential store")?;
    }
    if !plan.scopes().contains(&ResetScope::Setup) {
        writeln!(output, "  configuration and presentation preferences")?;
    }
    writeln!(
        output,
        "  Codex authentication and Codex-owned conversations in every scope"
    )?;
    writeln!(
        output,
        "  active/unverified runtime descriptors (doctor repairs only unlocked stale descriptors)"
    )?;
    Ok(())
}

pub(super) fn run_reset_command(args: &cli::ResetArgs, paths: &XanaPaths) -> Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    run_reset_with_io(args, paths, stdin.is_terminal(), &mut input, &mut output)
}

pub(super) async fn run_doctor_command(args: &cli::DoctorArgs, paths: &XanaPaths) -> Result<()> {
    if args.fix && args.output == OutputChoice::Json {
        anyhow::bail!("doctor --fix uses an interactive text receipt; omit --output json")
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = anstream::stdout().lock();
    let report = crate::doctor::inspect(
        paths,
        crate::doctor::TerminalHealth {
            input_is_terminal: stdin.is_terminal(),
            output_is_terminal: io::stdout().is_terminal(),
            dumb: std::env::var("TERM").is_ok_and(|value| value.eq_ignore_ascii_case("dumb")),
        },
    )
    .await;
    if args.output == OutputChoice::Json {
        serde_json::to_writer(&mut output, &report)?;
        writeln!(output)?;
        return Ok(());
    }
    write_doctor_report(&mut output, &report)?;
    if !args.fix {
        return Ok(());
    }
    let repair_count = report.repairs().count();
    if repair_count == 0 {
        writeln!(output, "No deterministic repairs are available.")?;
        return Ok(());
    }
    writeln!(output, "Deterministic repair preview:")?;
    for finding in report.repairs() {
        writeln!(
            output,
            "  {}: {} ({})",
            finding.code, finding.summary, finding.evidence
        )?;
    }
    if !args.yes {
        if !stdin.is_terminal() {
            anyhow::bail!("doctor --fix requires --yes when stdin is not a terminal")
        }
        write!(output, "Apply these {repair_count} repair(s)? [y/N]: ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0
            || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        {
            writeln!(output, "No changes made.")?;
            return Ok(());
        }
    }
    let mut failed = false;
    for (code, result) in report.apply_repairs(paths) {
        match result {
            Ok(true) => writeln!(output, "fixed {code}")?,
            Ok(false) => writeln!(output, "already fixed {code}")?,
            Err(reason) => {
                failed = true;
                writeln!(output, "could not fix {code}: {reason}")?;
            }
        }
    }
    if failed {
        anyhow::bail!("one or more deterministic repairs failed; rerun xana doctor")
    }
    Ok(())
}

fn write_doctor_report<W: Write>(
    output: &mut W,
    report: &crate::doctor::DoctorReport,
) -> Result<()> {
    writeln!(output, "Xana doctor (read-only)")?;
    for finding in &report.findings {
        writeln!(
            output,
            "[{}] {}: {}",
            finding.severity.as_str(),
            finding.code,
            finding.summary
        )?;
        writeln!(output, "  evidence: {}", finding.evidence)?;
        if let Some(action) = &finding.action {
            writeln!(output, "  next: {action}")?;
        }
    }
    Ok(())
}
