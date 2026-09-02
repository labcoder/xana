//! Read-only session inspection and managed-handle selection commands.

use crate::{
    cli::SessionCommand,
    conversation_branch::ConversationBranchService,
    managed::thread_store::ManagedThreadStore,
    paths::XanaPaths,
    session::DurableSession,
    workspace_host::{ConversationRef, ConversationState, WorkspaceHost},
};
use anyhow::{Context, Result};
use std::io::Write;

pub(super) fn run_command<W: Write>(
    command: SessionCommand,
    paths: &XanaPaths,
    output: &mut W,
) -> Result<()> {
    match command {
        SessionCommand::New => {
            anyhow::bail!(
                "session new must be routed through the interactive application lifecycle"
            )
        }
        SessionCommand::List => {
            let workspace = std::env::current_dir()
                .context("could not resolve current workspace")?
                .canonicalize()
                .context("could not canonicalize current workspace")?;
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
                summary.context_versions.len()
            )?;
            for (context_id, version) in summary.context_versions {
                writeln!(output, "  {context_id} v{version}")?;
            }
            writeln!(output, "children: {}", summary.children.len())?;
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
