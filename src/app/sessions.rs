//! Read-only session inspection and managed-handle selection commands.

use crate::{
    cli::SessionCommand,
    conversation_branch::ConversationBranchService,
    managed::thread_store::ManagedThreadStore,
    paths::XanaPaths,
    session::DurableSession,
    terminal_productivity::search_transcript,
    workspace_host::{ConversationRef, ConversationState, WorkspaceHost},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Write;

#[derive(Debug, Serialize)]
struct ConversationPreview {
    version: u16,
    conversation: ConversationRef,
    start: usize,
    total: usize,
    has_older: bool,
    messages: Vec<crate::message::Message>,
}

pub(super) fn run_command<W: Write>(
    command: SessionCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        SessionCommand::EvaluateCompaction { .. } => {
            anyhow::bail!("semantic evaluation requires the asynchronous application route")
        }
        SessionCommand::New => {
            anyhow::bail!(
                "session new must be routed through the interactive application lifecycle"
            )
        }
        SessionCommand::Continue | SessionCommand::Attach { .. } => {
            anyhow::bail!(
                "conversation lifecycle command must be routed through the interactive application"
            )
        }
        SessionCommand::Preview {
            conversation,
            limit,
            json,
        } => {
            let workspace = current_workspace()?;
            let host = WorkspaceHost::open(paths.data_dir(), &workspace)?;
            let snapshot = host.snapshot()?;
            let conversation = resolve_conversation(&snapshot.conversations, Some(&conversation))?;
            let page = host
                .conversation_history_page(&conversation, None, limit)?
                .with_context(|| {
                    format!(
                        "{conversation} history is owned by its managed runtime; attach it to resume and inspect vendor-retained history"
                    )
                })?;
            let preview = ConversationPreview {
                version: 1,
                conversation,
                start: page.start,
                total: page.total,
                has_older: page.has_older,
                messages: page.messages,
            };
            if json {
                serde_json::to_writer(&mut *output, &preview)?;
                writeln!(output)?;
            } else {
                writeln!(output, "Conversation: {}", preview.conversation)?;
                writeln!(
                    output,
                    "Messages: {} of {}{}",
                    preview.messages.len(),
                    preview.total,
                    if preview.has_older {
                        " (older messages omitted)"
                    } else {
                        ""
                    }
                )?;
                for message in &preview.messages {
                    writeln!(
                        output,
                        "{}> {}",
                        role_label(message.role),
                        message_text(message)
                    )?;
                }
            }
            Ok(())
        }
        SessionCommand::List => {
            let workspace = current_workspace()?;
            let host = WorkspaceHost::open(paths.data_dir(), &workspace)?;
            let snapshot = host.snapshot()?;
            writeln!(output, "workspace: {}", snapshot.workspace.display())?;
            writeln!(output, "conversations: {}", snapshot.conversations.len())?;
            for conversation in snapshot.conversations {
                writeln!(
                    output,
                    "  {} state={}{}{}",
                    conversation.conversation,
                    conversation.state,
                    if conversation.selected {
                        " selected"
                    } else {
                        ""
                    },
                    conversation
                        .record_count
                        .map(|count| format!(" records={count}"))
                        .unwrap_or_default()
                )?;
            }
            if let Some(active) = snapshot.active {
                writeln!(
                    output,
                    "active root: {} process={} (descriptor is advisory; the OS lock is authoritative)",
                    active.conversation,
                    active.process_id()
                )?;
            } else {
                writeln!(output, "active root: none")?;
            }
            writeln!(
                output,
                "states: {}",
                ConversationState::all()
                    .into_iter()
                    .map(|state| state.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
            Ok(())
        }
        SessionCommand::Search {
            query,
            conversation,
            limit,
            json,
        } => {
            let workspace = std::env::current_dir()
                .context("could not resolve current workspace")?
                .canonicalize()
                .context("could not canonicalize current workspace")?;
            let host = WorkspaceHost::open(paths.data_dir(), &workspace)?;
            let snapshot = host.snapshot()?;
            let conversation =
                resolve_conversation(&snapshot.conversations, conversation.as_deref())?;
            let report = search_transcript(&host, &conversation, &query, limit)?;
            if json {
                serde_json::to_writer(&mut *output, &report)?;
                writeln!(output)?;
            } else {
                writeln!(
                    output,
                    "conversation: {}\nquery: {:?}\nmatches: {}{}",
                    report.conversation,
                    report.query,
                    report.matches.len(),
                    if report.truncated { " (truncated)" } else { "" }
                )?;
                for entry in report.matches {
                    writeln!(
                        output,
                        "  {} [{}] {}",
                        entry.index, entry.role, entry.excerpt
                    )?;
                }
            }
            Ok(())
        }
        SessionCommand::Inspect { session_id } => {
            let summary = DurableSession::inspect(paths.data_dir(), session_id)?;
            writeln!(output, "session: {}", summary.session_id)?;
            writeln!(output, "path: {}", summary.path.display())?;
            writeln!(output, "records: {}", summary.record_count)?;
            writeln!(
                output,
                "active history entries: {}",
                summary.active_entry_count
            )?;
            if !summary.recent_active_entry_ids.is_empty() {
                writeln!(output, "branch points (oldest to newest; at most 128):")?;
                for entry_id in &summary.recent_active_entry_ids {
                    writeln!(output, "  {entry_id}")?;
                }
            }
            writeln!(
                output,
                "unfinished operations: {}",
                summary.unfinished.len()
            )?;
            for (operation_id, state) in summary.unfinished {
                writeln!(output, "  {operation_id}: {state:?}")?;
            }
            writeln!(output, "artifacts: {}", summary.artifact_count)?;
            writeln!(output, "artifact bytes: {}", summary.artifact_bytes)?;
            writeln!(
                output,
                "context versions: {}",
                summary.context_version_count
            )?;
            for (context_id, version) in summary.context_versions {
                writeln!(output, "  {context_id} v{version}")?;
            }
            writeln!(output, "children: {}", summary.child_count)?;
            if summary.bounded_details {
                writeln!(
                    output,
                    "  Details are a bounded execution preview; older evidence remains available through exact object/history inspection."
                )?;
            }
            for child in summary.children {
                let attribution = &child.handle.admission.attribution;
                writeln!(
                    output,
                    "  {} parent={} route={} owner={} connection={} model={} state={:?} usage={:?} report={:?}{}",
                    attribution.agent_id,
                    attribution.parent_agent_id,
                    attribution.route,
                    attribution.owner.as_str(),
                    attribution.connection,
                    attribution.model,
                    child.handle.lifecycle,
                    child.handle.usage,
                    child.handle.report,
                    if child.projected_interruption {
                        " (projected after restart)"
                    } else {
                        ""
                    }
                )?;
            }
            writeln!(output, "compactions: {}", summary.compaction_count)?;
            for checkpoint in summary.compactions {
                writeln!(
                    output,
                    "  {} previous={} operation={} reason={:?} source={}..{} entries={} digest={} tail={} connection={} model={} context={} source={:?} route_ceiling={} input_budget={} threshold={} retained_tail={} usage=estimated",
                    checkpoint.id,
                    checkpoint
                        .previous_checkpoint
                        .map_or_else(|| "none".to_owned(), |id| id.to_string()),
                    checkpoint.operation_id,
                    checkpoint.reason,
                    checkpoint.source_start,
                    checkpoint.source_end,
                    checkpoint.source_entry_count,
                    checkpoint.source_digest,
                    checkpoint.retained_tail_start,
                    checkpoint.budget.connection,
                    checkpoint.budget.model,
                    checkpoint.budget.context_window_tokens,
                    checkpoint.budget.context_window_source,
                    checkpoint
                        .budget
                        .route_ceiling_tokens
                        .map_or_else(|| "none".to_owned(), |tokens| tokens.to_string()),
                    checkpoint.budget.input_budget_tokens,
                    checkpoint.budget.compaction_threshold_tokens,
                    checkpoint.budget.retained_tail_tokens,
                )?;
            }
            if let Some(branch) = summary.branch {
                writeln!(
                    output,
                    "branch: source={} point={} shared_entries={}",
                    branch.source_session_id, branch.source_entry_id, branch.shared_entry_count
                )?;
            } else {
                writeln!(output, "branch: none")?;
            }
            match summary.repair_truncate_to {
                Some(offset) => {
                    writeln!(output, "torn tail: repair would truncate to byte {offset}")?
                }
                None => writeln!(output, "torn tail: none")?,
            }
            Ok(())
        }
        SessionCommand::Branch {
            conversation_id,
            at,
        } => {
            let workspace = std::env::current_dir()
                .context("could not resolve current workspace")?
                .canonicalize()
                .context("could not canonicalize current workspace")?;
            let receipt =
                ConversationBranchService::open(paths, &workspace)?.branch(conversation_id, &at)?;
            writeln!(output, "source Conversation: {}", receipt.source)?;
            writeln!(output, "source point: {}", receipt.source_point)?;
            writeln!(output, "new Conversation: {}", receipt.target)?;
            writeln!(output, "continuation: {}", receipt.kind.as_str())?;
            writeln!(
                output,
                "shared native entries: {}",
                receipt.shared_entry_count
            )?;
            writeln!(output, "owner target: {}", receipt.target_ref)?;
            writeln!(
                output,
                "Continue with: `xana --resume {}` from {}",
                receipt.target,
                workspace.display()
            )?;
            Ok(())
        }
        SessionCommand::SelectManaged {
            connection,
            thread_id,
        } => {
            let workspace = std::env::current_dir()
                .context("could not resolve current workspace")?
                .canonicalize()
                .context("could not canonicalize current workspace")?;
            let mut store = ManagedThreadStore::open(paths.data_dir(), &connection, &workspace)?;
            store.select_thread(&thread_id)?;
            writeln!(
                output,
                "selected managed conversation {connection}/{thread_id} for {}",
                workspace.display()
            )?;
            Ok(())
        }
        SessionCommand::ArchiveManaged {
            connection,
            thread_id,
        } => {
            let workspace = std::env::current_dir()
                .context("could not resolve current workspace")?
                .canonicalize()
                .context("could not canonicalize current workspace")?;
            let host = WorkspaceHost::open(paths.data_dir(), &workspace)?;
            let conversation = host
                .snapshot()?
                .conversations
                .into_iter()
                .map(|projection| projection.conversation)
                .find(|conversation| {
                    matches!(
                        conversation,
                        ConversationRef::Managed {
                            connection: candidate_connection,
                            thread_id: candidate_thread,
                            ..
                        } if candidate_connection == &connection && candidate_thread == &thread_id
                    )
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "managed conversation {connection}/{thread_id} is not retained locally"
                    )
                })?;
            if host.archive_managed_conversation(&conversation)? {
                writeln!(
                    output,
                    "archived local handle {conversation}; the vendor-owned thread was not deleted"
                )?;
            } else {
                anyhow::bail!("managed conversation {conversation} is not retained locally");
            }
            Ok(())
        }
    }
}

pub(super) fn semantic_policy(
    paths: &XanaPaths,
    connection: &crate::config::ConnectionConfig,
    model: &str,
) -> Result<Option<crate::session::compaction::semantic::HelperPolicy>> {
    let Some(store) = crate::storage::ProtectedStore::configured(paths.data_dir())? else {
        return Ok(None);
    };
    match crate::session::compaction::semantic::HelperPolicy::load(
        &store,
        &crate::session::compaction::semantic::route_digest(connection, model),
    ) {
        Ok(policy) => Ok(policy.map(|policy| {
            policy.with_route_validator(live_semantic_route(
                paths.config_file().to_path_buf(),
                connection.id.clone(),
                model.to_owned(),
            ))
        })),
        Err(_) => {
            eprintln!(
                "xana: semantic helper approval is unavailable or stale; using deterministic compaction until the exact route is re-evaluated"
            );
            Ok(None)
        }
    }
}

fn live_semantic_route(
    path: std::path::PathBuf,
    connection: String,
    model: String,
) -> std::sync::Arc<crate::session::compaction::semantic::RouteValidator> {
    std::sync::Arc::new(move |expected| {
        let registry = crate::config::XanaConfig::load_registry_from(&path)?;
        let current = registry
            .connections
            .get(&connection)
            .context("semantic helper connection is no longer configured")?;
        anyhow::ensure!(
            current.kind != crate::config::ProviderKind::Codex
                && crate::session::compaction::semantic::route_digest(current, &model) == expected,
            "semantic helper route changed; re-evaluate and authorize the current recipient"
        );
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)] // One CLI command with explicit disclosure/activation choices.
pub(super) async fn evaluate_compaction<W: Write>(
    paths: &XanaPaths,
    connection: &str,
    model: &str,
    yes: bool,
    enable: bool,
    disable: bool,
    output: &mut W,
) -> Result<()> {
    use crate::session::compaction::{evaluation, semantic};
    anyhow::ensure!(
        yes,
        "pass --yes to authorize synthetic helper evaluation or approval revocation"
    );
    let registry = crate::config::XanaConfig::load_registry_from(paths.config_file())?;
    let connection = registry
        .connections
        .get(connection)
        .context("unknown compaction helper connection")?;
    anyhow::ensure!(
        connection.kind != crate::config::ProviderKind::Codex,
        "Codex owns managed compaction; select a native helper connection"
    );
    anyhow::ensure!(
        !model.trim().is_empty() && model.len() <= 256 && !model.chars().any(char::is_control),
        "invalid helper model"
    );
    let store = crate::storage::ProtectedStore::configured(paths.data_dir())?
        .context("semantic evaluation requires protected storage and durable accounting")?;
    let digest = semantic::route_digest(connection, model);
    if disable {
        semantic::HelperPolicy::revoke(&store, &digest)?;
        writeln!(
            output,
            "Semantic helper disabled; native compaction uses the deterministic baseline."
        )?;
        return Ok(());
    }
    let (provider, _) = crate::orchestration::compose_native_provider(
        connection,
        model,
        crate::artifact::ArtifactStore::protected(store.clone()),
        true,
    )
    .map_err(anyhow::Error::msg)?;
    let budget = crate::usage_budget::UsageBudget::new(
        store.clone(),
        uuid::Uuid::new_v4().to_string(),
        "semantic-evaluation".into(),
        semantic::OUTPUT_RESERVE,
    )
    .with_facts(crate::usage_budget::DispatchFacts {
        connection: Some(connection.id.clone()),
        model: Some(model.into()),
        owner: Some("semantic-evaluation".into()),
        ..Default::default()
    });
    let cancellation = tokio_util::sync::CancellationToken::new();
    let report = tokio::select! {
        result = evaluation::evaluate(provider.as_ref(), &budget, digest.clone(), &cancellation) => result?,
        _ = tokio::signal::ctrl_c() => {
            cancellation.cancel();
            anyhow::bail!("semantic evaluation cancelled; no helper approval changed; unfinished usage remains reserved");
        }
    };
    store.set_document(
        &format!("compaction/evaluations/{digest}"),
        &serde_json::to_vec(&report)?,
        64 * 1024,
    )?;
    if enable {
        semantic::HelperPolicy::approve(&store, digest, &report)?;
    }
    serde_json::to_writer_pretty(&mut *output, &report)?;
    writeln!(
        output,
        "\nPromotion gate: {}. Synthetic measured cases are not universal semantic accuracy; review the recorded route and failures. {}",
        if report.passes() { "PASS" } else { "FAIL" },
        if enable {
            "Exact helper enabled for future native launches."
        } else {
            "No runtime policy changed; --enable is separate explicit authorization."
        }
    )?;
    Ok(())
}

fn resolve_conversation(
    conversations: &[crate::workspace_host::ConversationProjection],
    selector: Option<&str>,
) -> Result<ConversationRef> {
    if selector.is_none() {
        return conversations
            .iter()
            .find(|candidate| candidate.selected)
            .or_else(|| conversations.first())
            .map(|candidate| candidate.conversation.clone())
            .context("no retained Conversation is available to search");
    }
    let selector = selector.expect("checked above");
    let matches = conversations
        .iter()
        .filter(|candidate| {
            candidate.conversation.to_string() == selector
                || candidate
                    .conversation
                    .conversation_id()
                    .is_some_and(|id| id.to_string() == selector)
                || matches!(
                    &candidate.conversation,
                    ConversationRef::Native { session_id } if session_id.to_string() == selector
                )
                || matches!(
                    &candidate.conversation,
                    ConversationRef::Managed {
                        connection,
                        thread_id,
                        ..
                    } if thread_id == selector || format!("{connection}/{thread_id}") == selector
                )
        })
        .map(|candidate| candidate.conversation.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [conversation] => Ok(conversation.clone()),
        [] => anyhow::bail!(
            "no retained Conversation matches {selector:?}; run `xana conversation list`"
        ),
        _ => anyhow::bail!(
            "Conversation selector {selector:?} is ambiguous; use its exact canonical ID"
        ),
    }
}

pub(super) fn resolve_attach_target(paths: &XanaPaths, selector: &str) -> Result<ConversationRef> {
    let workspace = current_workspace()?;
    let host = WorkspaceHost::open(paths.data_dir(), &workspace)?;
    let snapshot = host.snapshot()?;
    let conversation = resolve_conversation(&snapshot.conversations, Some(selector))?;
    let state = snapshot
        .conversations
        .iter()
        .find(|candidate| candidate.conversation == conversation)
        .map(|candidate| candidate.state)
        .context("resolved Conversation disappeared from the workspace snapshot")?;
    if state != ConversationState::Inactive {
        anyhow::bail!(
            "Conversation {conversation} is {state}; attach requires an idle retained Conversation"
        );
    }
    Ok(conversation)
}

fn current_workspace() -> Result<std::path::PathBuf> {
    std::env::current_dir()
        .context("could not resolve current workspace")?
        .canonicalize()
        .context("could not canonicalize current workspace")
}

fn role_label(role: crate::message::Role) -> &'static str {
    match role {
        crate::message::Role::System => "system",
        crate::message::Role::User => "you",
        crate::message::Role::Assistant => "xana",
        crate::message::Role::Tool => "tool",
    }
}

fn message_text(message: &crate::message::Message) -> String {
    message
        .content
        .iter()
        .map(|block| match block {
            crate::message::ContentBlock::Text(text) => text.clone(),
            crate::message::ContentBlock::Image(image) => {
                format!("[image: {} bytes]", image.byte_len)
            }
            crate::message::ContentBlock::ToolCall(call) => {
                format!("[tool call: {}]", call.name)
            }
            crate::message::ContentBlock::ToolResult(result) => result.output.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod semantic_route_tests {
    use super::*;
    #[test]
    fn live_semantic_guard_rechecks_endpoint_and_removed_connection() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let input = "version=1\ndefault_profile='default'\npermission_mode='ask'\n[providers.local]\nkind='openai_compat'\nbase_url='http://localhost:11434/v1'\n[profiles.default]\nprovider='local'\nmodel='synthetic'\n";
        std::fs::write(&path, input).unwrap();
        let registry = crate::config::XanaConfig::load_registry_from(&path).unwrap();
        let digest = crate::session::compaction::semantic::route_digest(
            &registry.connections["local"],
            "synthetic",
        );
        let validate = live_semantic_route(path.clone(), "local".into(), "synthetic".into());
        validate(&digest).unwrap();
        std::fs::write(&path, input.replace("11434", "11435")).unwrap();
        assert!(validate(&digest).is_err());
        std::fs::write(&path, input.replace("local", "renamed")).unwrap();
        assert!(validate(&digest).is_err());
    }
}
