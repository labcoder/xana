//! Provider-neutral usage inspection at the application edge.

use crate::{
    app::connections::model_manager,
    cli::UsageArgs,
    frontend::semantic::{AvailabilityV1, UsageScopeV1},
    paths::XanaPaths,
    usage_observation::{UsageCacheStatusV1, UsageObservationService, UsageReportV1},
};
use anyhow::{Context, Result};
use std::io::Write;
use tokio_util::sync::CancellationToken;

pub(super) fn compose_budget(
    paths: &XanaPaths,
    root: String,
    mut facts: crate::usage_budget::DispatchFacts,
) -> Result<Option<crate::usage_budget::UsageBudget>> {
    let Some(store) = crate::storage::ProtectedStore::configured(paths.data_dir())? else {
        return Ok(None);
    };
    facts.project = crate::project::ProjectStore::open(paths)?
        .membership(&root)?
        .map(|id| id.to_string());
    Ok(Some(
        crate::usage_budget::UsageBudget::new(store, root, "conversation".into(), 16_384)
            .with_facts(facts),
    ))
}

pub(super) async fn run<W: Write>(
    args: UsageArgs,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    if let Some(crate::cli::UsageCommand::Ledger { root, job, after }) = args.command {
        let store = crate::storage::ProtectedStore::configured(paths.data_dir())?
            .context("durable usage requires protected storage; inspect xana storage status")?;
        let records = store.usage_page(root.as_deref(), job.as_deref(), after)?;
        serde_json::to_writer_pretty(
            &mut *output,
            &serde_json::json!({
                "version": 1, "records": records,
                "next_after": records.last().map(|row| row.sequence),
                "notice": "Charges are local admission estimates until reported tokens reconcile them. Null cost/usage means unknown, not free. Managed requests count outer turns, not vendor inner calls. Provider quotas and account limits are separate."
            }),
        )?;
        writeln!(output)?;
        return Ok(());
    }
    let manager = model_manager(paths)?;
    let selected = manager.selected()?;
    let connection = args.connection.as_deref().unwrap_or(&selected.connection);
    let model = args
        .model
        .as_deref()
        .or_else(|| (connection == selected.connection).then_some(selected.model.as_str()));
    let cancellation = CancellationToken::new();
    let report = UsageObservationService::new(paths.cache_dir().to_owned())
        .query(&manager, connection, model, args.refresh, &cancellation)
        .await?;
    if args.json {
        serde_json::to_writer_pretty(&mut *output, &report)?;
        writeln!(output)?;
    } else {
        write_text(output, &report)?;
    }
    Ok(())
}

pub(super) fn budget<W: Write>(
    args: crate::cli::BudgetArgs,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    let store = crate::storage::ProtectedStore::configured(paths.data_dir())?
        .context("budgets require protected storage; no plaintext ledger is created")?;
    let policy = store.update_usage_policy(|policy| {
        if let Some(value) = args.daily_requests {
            policy.daily_requests = value;
        }
        if let Some(value) = args.root_requests {
            policy.root_requests = value;
        }
        if let Some(value) = args.foreground_request_reserve {
            policy.foreground_request_reserve = value;
        }
        if let Some(value) = args.daily_tokens {
            policy.daily_tokens = (value != 0).then_some(value);
        }
        if let Some(value) = args.root_tokens {
            policy.root_tokens = (value != 0).then_some(value);
        }
        if let Some(value) = args.background_daily_tokens {
            policy.background_daily_tokens = value;
        }
        if let Some(value) = args.background_job_tokens {
            policy.background_job_tokens = value;
        }
    })?;
    if args.accept_restored_usage {
        store.remove_document("usage/restore-review-required")?;
    }
    serde_json::to_writer_pretty(&mut *output, &policy)?;
    writeln!(
        output,
        "\nLocal admission policy; not a vendor billing ceiling. Existing reservations and receipts are not reset."
    )?;
    Ok(())
}

fn write_text<W: Write>(output: &mut W, report: &UsageReportV1) -> Result<()> {
    writeln!(
        output,
        "connection: {} ({})",
        report.connection,
        report.provider.as_str()
    )?;
    writeln!(output, "source: {}", cache_label(report.cache_status))?;
    if let Some(model) = &report.model {
        writeln!(output, "model: {}", model.model)?;
        writeln!(
            output,
            "capabilities: tools={} reasoning={} context={}",
            optional_bool(model.tools),
            optional_bool(model.reasoning),
            model
                .context_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        )?;
        writeln!(
            output,
            "pricing: {}",
            model
                .pricing
                .summary()
                .unwrap_or_else(|| "unavailable".to_owned())
        )?;
    }
    if report.observations.is_empty() {
        writeln!(output, "usage: unavailable")?;
    }
    for observation in &report.observations {
        writeln!(
            output,
            "{} [{}] {}",
            scope_label(&observation.scope),
            availability_label(&observation.availability),
            amounts_label(observation)
        )?;
    }
    for notice in &report.notices {
        writeln!(output, "notice: {notice}")?;
    }
    Ok(())
}

fn cache_label(status: UsageCacheStatusV1) -> &'static str {
    match status {
        UsageCacheStatusV1::Live => "live refresh",
        UsageCacheStatusV1::FreshCache => "fresh cache",
        UsageCacheStatusV1::RefreshLimitedCache => "fresh cache (refresh rate-limited)",
        UsageCacheStatusV1::StaleCache => "stale cache",
        UsageCacheStatusV1::Unavailable => "unavailable",
    }
}

fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn scope_label(scope: &UsageScopeV1) -> String {
    match scope {
        UsageScopeV1::Request { run_id } => format!("request {run_id}"),
        UsageScopeV1::Run { run_id } => format!("run {run_id}"),
        UsageScopeV1::Conversation { conversation_id } => {
            format!("conversation {conversation_id}")
        }
        UsageScopeV1::Connection { connection } => format!("connection {connection}"),
        UsageScopeV1::Model { connection, model } => format!("model {connection}/{model}"),
        UsageScopeV1::Account { connection, .. } => format!("account for {connection}"),
        UsageScopeV1::RateLimitBucket { connection, bucket } => {
            format!("rate limit {connection}/{bucket}")
        }
    }
}

fn availability_label(availability: &AvailabilityV1) -> String {
    match availability {
        AvailabilityV1::Available => "available".to_owned(),
        AvailabilityV1::Stale => "stale".to_owned(),
        AvailabilityV1::Unsupported => "unsupported".to_owned(),
        AvailabilityV1::Unavailable { code } => format!("unavailable: {code}"),
        AvailabilityV1::PermissionRequired { code } => {
            format!("permission required: {code}")
        }
    }
}

fn amounts_label(observation: &crate::frontend::semantic::UsageObservationV1) -> String {
    let mut parts = Vec::new();
    let amounts = &observation.amounts;
    for (label, value) in [
        ("input", amounts.input_tokens),
        ("cache-read", amounts.cached_input_tokens),
        ("cache-write", amounts.cache_write_input_tokens),
        ("output", amounts.output_tokens),
        ("reasoning", amounts.reasoning_tokens),
        ("tool", amounts.tool_tokens),
    ] {
        if let Some(value) = value {
            parts.push(format!("{label}={value}"));
        }
    }
    if let Some(cost) = amounts.cost_microunits {
        parts.push(format!("cost=${:.6}", cost as f64 / 1_000_000.0));
    }
    if let Some(limit) = &observation.rate_limit {
        if let Some(used) = limit.used_percent_basis_points {
            parts.push(format!("used={:.2}%", f64::from(used) / 100.0));
        }
        if let Some(reset) = limit.reset_at_unix_millis {
            parts.push(format!("reset_ms={reset}"));
        }
    }
    if let Some(quota) = &observation.quota
        && let Some(remaining) = quota.remaining
    {
        parts.push(format!("quota_remaining={remaining}"));
    }
    if let Some(credits) = &observation.credits
        && let Some(remaining) = credits.remaining_microunits
    {
        parts.push(format!(
            "credits_remaining={:.6} {}",
            remaining as f64 / 1_000_000.0,
            credits.currency
        ));
    }
    if parts.is_empty() {
        "no numeric value reported".to_owned()
    } else {
        parts.join(" · ")
    }
}
