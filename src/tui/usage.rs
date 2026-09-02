//! Honest, frontend-only usage and execution-fact presentation.

use super::state::TuiState;
use crate::frontend::semantic::{
    AvailabilityV1, FactAuthorityV1, FactSourceV1, FreshnessV1, UsageAccountingV1, UsageAmountsV1,
    UsageObservationV1, UsageScopeV1,
};
use std::fmt::Write as _;

pub(super) fn compact(state: &TuiState) -> String {
    let mut lines = vec![format!(
        "{} / {}{}",
        state.connection,
        state.model,
        state
            .prompt_plans
            .last()
            .map(|(_, ledger)| format!(
                " · context ~{} / {} input tokens",
                ledger.estimated_input_tokens, ledger.budget.input_budget_tokens
            ))
            .unwrap_or_default()
    )];

    if let Some(observation) = state.semantic.usage.last() {
        lines.push(format!(
            "{} · {} · {}",
            scope_label(&observation.scope),
            amounts_label(&observation.amounts),
            availability_label(&observation.availability)
        ));
        if let Some(context) = &observation.context {
            lines.push(context.capacity_tokens.map_or_else(
                || {
                    format!(
                        "Context: {} input tokens; capacity unknown",
                        context.input_tokens
                    )
                },
                |capacity| {
                    format!(
                        "Context: {} / {capacity} input tokens",
                        context.input_tokens
                    )
                },
            ));
        }
    } else {
        lines.push(state.managed_usage.map_or_else(
            || state.native_usage.render(),
            |(input, output, total)| {
                format!("Current managed thread: input {input} · output {output} · total {total}")
            },
        ));
    }

    lines.push(
        "Use /usage details for scopes, periods, provenance, limits, prompt categories, and completion facts."
            .to_owned(),
    );
    lines.join("\n")
}

pub(super) fn details(state: &TuiState) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Connection: {}", state.connection);
    let _ = writeln!(output, "Model: {}", state.model);
    let _ = writeln!(output, "Execution owner: {}", owner_label(state));
    let _ = writeln!(
        output,
        "\nProcess-local accounting\n{}",
        state.native_usage.render()
    );
    if let Some((input, output_tokens, total)) = state.managed_usage {
        let _ = writeln!(
            output,
            "Managed cumulative snapshot: input {input} · output {output_tokens} · total {total}"
        );
    }

    output.push_str("\nProvider-neutral observations\n");
    if state.semantic.usage.is_empty() {
        output.push_str(
            "No scoped observation is available. Unknown quota, rate-limit, credit, or cost values are not zero.\n",
        );
    } else {
        for observation in &state.semantic.usage {
            render_observation(&mut output, observation);
        }
    }

    output.push_str("\nPrompt plan\n");
    if let Some((run_id, ledger)) = state.prompt_plans.last() {
        let _ = writeln!(output, "Run: {run_id}");
        let _ = writeln!(
            output,
            "Estimated input: {} / {} tokens (context window {})",
            ledger.estimated_input_tokens,
            ledger.budget.input_budget_tokens,
            ledger.budget.context_window_tokens
        );
        for category in &ledger.categories {
            let _ = writeln!(
                output,
                "  {:?}: ~{} tokens",
                category.kind, category.estimated_tokens
            );
        }
        let _ = writeln!(
            output,
            "Attachments: {} item(s), {} byte(s)",
            ledger.attachment_count, ledger.attachment_bytes
        );
        let _ = writeln!(
            output,
            "Omitted sources: {} · cache read/write facts: unavailable",
            ledger.omitted_source_ids.len()
        );
    } else {
        output.push_str("Unavailable until Xana plans a native prompt for this process.\n");
    }

    output.push_str("\nRun execution facts\n");
    if state.semantic.execution_facts.is_empty() {
        output.push_str("No authoritative execution-fact receipt has been emitted.\n");
    } else {
        for facts in state.semantic.execution_facts.iter().rev().take(8).rev() {
            let _ = writeln!(
                output,
                "{} · owner {:?} · host {:?} · workspace {:?} · connection {} · model {} · approval {} · source {} · {}",
                facts.run_id,
                facts.owner,
                facts.host,
                facts.workspace_authority,
                facts.connection.as_deref().unwrap_or("unknown"),
                facts.model.as_deref().unwrap_or("unknown"),
                facts.approval_policy,
                source_label(facts.source),
                freshness_label(&facts.freshness),
            );
        }
    }

    output.push_str("\nCompletion receipts\n");
    if state.semantic.completion_receipts.is_empty() {
        output.push_str("No bounded completion receipt has been emitted.\n");
    } else {
        for receipt in state
            .semantic
            .completion_receipts
            .iter()
            .rev()
            .take(8)
            .rev()
        {
            let _ = writeln!(
                output,
                "{} · run {} · {:?} · {} artifact(s) · {} check(s) · {} warning(s) · {}",
                receipt.id,
                receipt.run_id,
                receipt.status,
                receipt.artifacts.len(),
                receipt.checks.len(),
                receipt.unresolved_warnings.len(),
                freshness_label(&receipt.freshness),
            );
        }
    }

    output.push_str("\nSurface capabilities\n");
    let _ = writeln!(
        output,
        "interrupt={} · steer={} · model={} · reasoning={} · compact={}",
        state.capabilities.interrupt,
        state.capabilities.steer,
        state.capabilities.model,
        state.capabilities.reasoning,
        state.capabilities.compact,
    );
    let _ = writeln!(output, "inline image: {}", state.inline_image_capability);
    output.push_str(
        "Availability is not permission. Missing facts remain unavailable and do not imply zero usage or authority.\n",
    );
    output
}

fn render_observation(output: &mut String, observation: &UsageObservationV1) {
    let accounting = match observation.accounting {
        UsageAccountingV1::Delta => "delta".to_owned(),
        UsageAccountingV1::CumulativeSnapshot { sequence } => {
            format!("cumulative snapshot #{sequence}")
        }
    };
    let _ = writeln!(
        output,
        "{} · period {} · {accounting}",
        scope_label(&observation.scope),
        observation.period
    );
    let _ = writeln!(
        output,
        "  {} · source {} · authority {} · {}",
        availability_label(&observation.availability),
        source_label(observation.source),
        authority_label(observation.authority),
        freshness_label(&observation.freshness),
    );
    let _ = writeln!(output, "  {}", amounts_label(&observation.amounts));
    if let Some(context) = &observation.context {
        let capacity = context
            .capacity_tokens
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
        let _ = writeln!(
            output,
            "  context input={} capacity={} compacted={} summary={} retrieval={}",
            context.input_tokens,
            capacity,
            optional(context.compacted_tokens),
            optional(context.derived_summary_tokens),
            optional(context.retrieval_tokens),
        );
    }
    render_limit(output, "rate limit", observation.rate_limit.as_ref());
    render_limit(output, "quota", observation.quota.as_ref());
    if let Some(credits) = &observation.credits {
        let _ = writeln!(
            output,
            "  credits {}: purchased={} used={} remaining={}",
            credits.currency,
            optional(credits.purchased_microunits),
            optional(credits.used_microunits),
            optional(credits.remaining_microunits),
        );
    } else {
        output.push_str("  credits: unavailable\n");
    }
}

fn render_limit(
    output: &mut String,
    label: &str,
    limit: Option<&crate::frontend::semantic::LimitObservationV1>,
) {
    if let Some(limit) = limit {
        let used = limit.used_percent_basis_points.map_or_else(
            || "unknown".to_owned(),
            |value| format!("{:.2}%", f64::from(value) / 100.0),
        );
        let _ = writeln!(
            output,
            "  {label}: used={used} remaining={} limit={} window_ms={} reset_unix_ms={}",
            optional(limit.remaining),
            optional(limit.limit),
            optional(limit.window_millis),
            optional(limit.reset_at_unix_millis),
        );
    } else {
        let _ = writeln!(output, "  {label}: unavailable");
    }
}

fn amounts_label(amounts: &UsageAmountsV1) -> String {
    let mut values = Vec::new();
    for (label, value) in [
        ("input", amounts.input_tokens),
        ("cache-read", amounts.cached_input_tokens),
        ("cache-write", amounts.cache_write_input_tokens),
        ("output", amounts.output_tokens),
        ("reasoning", amounts.reasoning_tokens),
        ("tool", amounts.tool_tokens),
        ("requests", amounts.request_count),
        ("prompt-bytes", amounts.prompt_bytes),
        ("tool-schema-bytes", amounts.tool_schema_bytes),
    ] {
        if let Some(value) = value {
            values.push(format!("{label} {value}"));
        }
    }
    if let Some(cost) = amounts.cost_microunits {
        values.push(format!("cost ${:.6}", cost as f64 / 1_000_000.0));
    }
    if values.is_empty() {
        "numeric amounts unavailable".to_owned()
    } else {
        values.join(" · ")
    }
}

fn scope_label(scope: &UsageScopeV1) -> String {
    match scope {
        UsageScopeV1::Request { run_id } => format!("Request {run_id}"),
        UsageScopeV1::Run { run_id } => format!("Run {run_id}"),
        UsageScopeV1::Conversation { conversation_id } => {
            format!("Conversation {conversation_id}")
        }
        UsageScopeV1::Connection { connection } => format!("Connection {connection}"),
        UsageScopeV1::Model { connection, model } => format!("Model {connection}/{model}"),
        UsageScopeV1::Account {
            connection,
            account,
        } => {
            format!("Account {connection}/{account}")
        }
        UsageScopeV1::RateLimitBucket { connection, bucket } => {
            format!("Rate-limit bucket {connection}/{bucket}")
        }
    }
}

fn availability_label(value: &AvailabilityV1) -> String {
    match value {
        AvailabilityV1::Available => "available".to_owned(),
        AvailabilityV1::Stale => "stale".to_owned(),
        AvailabilityV1::Unsupported => "unsupported".to_owned(),
        AvailabilityV1::Unavailable { code } => format!("unavailable ({code})"),
        AvailabilityV1::PermissionRequired { code } => {
            format!("permission required ({code})")
        }
    }
}

fn source_label(value: FactSourceV1) -> &'static str {
    match value {
        FactSourceV1::Runtime => "runtime",
        FactSourceV1::Surface => "surface",
        FactSourceV1::Connection => "connection",
        FactSourceV1::Model => "model",
        FactSourceV1::Route => "route",
        FactSourceV1::Adapter => "adapter",
        FactSourceV1::Provider => "provider",
        FactSourceV1::ManagedRuntime => "managed runtime",
        FactSourceV1::Mcp => "MCP",
        FactSourceV1::A2a => "A2A",
        FactSourceV1::Measured => "measured",
        FactSourceV1::Estimated => "estimated",
        FactSourceV1::Cache => "cache",
    }
}

fn authority_label(value: FactAuthorityV1) -> &'static str {
    match value {
        FactAuthorityV1::Authoritative => "authoritative",
        FactAuthorityV1::ProviderReported => "provider reported",
        FactAuthorityV1::Measured => "measured",
        FactAuthorityV1::Estimated => "estimated",
    }
}

fn freshness_label(value: &FreshnessV1) -> String {
    value.max_age_millis.map_or_else(
        || format!("observed {} ms", value.observed_at_unix_millis),
        |age| {
            format!(
                "observed {} ms; max age {} ms",
                value.observed_at_unix_millis, age
            )
        },
    )
}

fn optional(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn owner_label(state: &TuiState) -> &'static str {
    if state.capabilities.reasoning {
        "managed runtime"
    } else {
        "Xana native"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frontend::semantic::{
            AvailabilityV1, FactAuthorityV1, FactSourceV1, FreshnessV1, UsageAccountingV1,
            UsageAmountsV1, UsageObservationV1, UsageScopeV1,
        },
        identity::OperationId,
        presentation::ComposerPreset,
    };
    use uuid::Uuid;

    #[test]
    fn detailed_usage_preserves_unknown_values_and_provenance() {
        let mut state = TuiState::starting(ComposerPreset::Submit);
        state.connection = "test".to_owned();
        state.model = "model".to_owned();
        state.semantic.usage.push(UsageObservationV1 {
            id: Uuid::new_v4(),
            scope: UsageScopeV1::Run {
                run_id: OperationId::new(),
            },
            period: "request-1".to_owned(),
            accounting: UsageAccountingV1::Delta,
            amounts: UsageAmountsV1 {
                input_tokens: Some(42),
                ..UsageAmountsV1::default()
            },
            context: None,
            rate_limit: None,
            quota: None,
            credits: None,
            request_affinity_digest: None,
            availability: AvailabilityV1::Available,
            source: FactSourceV1::Provider,
            authority: FactAuthorityV1::ProviderReported,
            freshness: FreshnessV1 {
                observed_at_unix_millis: 10,
                max_age_millis: None,
            },
        });

        let rendered = details(&state);

        assert!(rendered.contains("input 42"));
        assert!(rendered.contains("source provider"));
        assert!(rendered.contains("quota: unavailable"));
        assert!(rendered.contains("No authoritative execution-fact receipt"));
    }
}
