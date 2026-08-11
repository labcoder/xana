//! Read-only session inspection and managed-handle selection commands.

use crate::{
    cli::SessionCommand,
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
            match summary.repair_truncate_to {
                Some(offset) => {
                    writeln!(output, "torn tail: repair would truncate to byte {offset}")?
                }
                None => writeln!(output, "torn tail: none")?,
            }
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
            let conversation = ConversationRef::Managed {
                connection,
                thread_id,
            };
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
