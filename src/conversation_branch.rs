//! Explicit, source-preserving Conversation branching across execution owners.

use crate::{
    identity::{ConversationEntryId, ConversationId},
    managed::thread_store::ManagedThreadStore,
    paths::XanaPaths,
    private_state::{ConversationBranchContinuation, ConversationBranchRecord},
    profile::ProfileStore,
    session::DurableSession,
    workspace_host::{ConversationRef, WorkspaceHost},
};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::private_state::{ProjectRegistryDocument, read_document};

pub(crate) trait ManagedBranchOwner {
    /// Returns a provider-owned target thread when native forking is supported.
    /// `None` means the owner requires a fresh, explicitly bounded continuation.
    fn fork_thread(&mut self, connection: &str, source_thread_id: &str) -> Result<Option<String>>;
}

#[derive(Debug, Default)]
pub(crate) struct FreshManagedContinuation;

impl ManagedBranchOwner for FreshManagedContinuation {
    fn fork_thread(
        &mut self,
        _connection: &str,
        _source_thread_id: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchKind {
    NativeHistory,
    ManagedNativeFork,
    ManagedFreshContinuation,
}

impl BranchKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NativeHistory => "native shared history",
            Self::ManagedNativeFork => "managed owner-native fork",
            Self::ManagedFreshContinuation => "managed fresh continuation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationBranchReceipt {
    pub(crate) source: ConversationId,
    pub(crate) source_point: String,
    pub(crate) target: ConversationId,
    pub(crate) target_ref: ConversationRef,
    pub(crate) kind: BranchKind,
    pub(crate) shared_entry_count: usize,
}

pub(crate) struct ConversationBranchService<'a> {
    paths: &'a XanaPaths,
    workspace: PathBuf,
}

impl<'a> ConversationBranchService<'a> {
    pub(crate) fn open(paths: &'a XanaPaths, workspace: &Path) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .with_context(|| format!("could not canonicalize {}", workspace.display()))?;
        Ok(Self { paths, workspace })
    }

    pub(crate) fn branch(
        &self,
        source: ConversationId,
        source_point: &str,
    ) -> Result<ConversationBranchReceipt> {
        self.branch_with_owner(source, source_point, &mut FreshManagedContinuation)
    }

    fn branch_with_owner(
        &self,
        source: ConversationId,
        source_point: &str,
        owner: &mut dyn ManagedBranchOwner,
    ) -> Result<ConversationBranchReceipt> {
        let host = WorkspaceHost::open(self.paths.data_dir(), &self.workspace)?;
        let source_ref = host
            .snapshot()?
            .conversations
            .into_iter()
            .map(|projection| projection.conversation)
            .find(|conversation| conversation.conversation_id() == Some(source))
            .with_context(|| {
                format!(
                    "Conversation {source} is not retained in workspace {}",
                    self.workspace.display()
                )
            })?;
        match source_ref {
            ConversationRef::Native { session_id } => {
                self.branch_native(source, session_id, source_point)
            }
            ConversationRef::Managed {
                connection,
                thread_id,
                ..
            } => self.branch_managed(source, &connection, &thread_id, source_point, owner),
            ConversationRef::NewManaged { .. } => bail!(
                "pending managed Conversation {source} has no provider-owned history point to branch"
            ),
            ConversationRef::NewNative => {
                unreachable!("workspace snapshots never retain NewNative")
            }
        }
    }

    fn branch_native(
        &self,
        source: ConversationId,
        session_id: crate::identity::SessionId,
        source_point: &str,
    ) -> Result<ConversationBranchReceipt> {
        let entry_id = source_point
            .parse::<ConversationEntryId>()
            .with_context(|| {
                format!("native branch point {source_point:?} is not a Conversation entry id")
            })?;
        let (target, native) =
            DurableSession::branch_at(self.paths.data_dir(), session_id, entry_id)?;
        let target_id = ConversationId::for_native(native.target_session_id);
        let record = ConversationBranchRecord {
            source_conversation: source,
            source_point: native.source_entry_id.to_string(),
            workspace_root: self.workspace.clone(),
            continuation: ConversationBranchContinuation::NativeHistory {
                source_session_id: native.source_session_id,
                source_entry_id: native.source_entry_id,
            },
            shared_entry_count: native.shared_entry_count,
        };
        if let Err(error) = ProfileStore::open(self.paths).commit_branch(target_id, record) {
            target.discard_staged_branch().with_context(|| {
                format!("branch profile commit failed ({error}); target cleanup also failed")
            })?;
            return Err(error).context("could not preserve the branch Profile and lineage");
        }
        drop(target);
        Ok(ConversationBranchReceipt {
            source,
            source_point: native.source_entry_id.to_string(),
            target: target_id,
            target_ref: ConversationRef::Native {
                session_id: native.target_session_id,
            },
            kind: BranchKind::NativeHistory,
            shared_entry_count: native.shared_entry_count,
        })
    }

    fn branch_managed(
        &self,
        source: ConversationId,
        connection: &str,
        thread_id: &str,
        source_point: &str,
        owner: &mut dyn ManagedBranchOwner,
    ) -> Result<ConversationBranchReceipt> {
        if source_point != "current" && source_point != thread_id {
            bail!(
                "managed branch point must be `current` or the exact retained provider thread id"
            );
        }
        let target = ConversationId::new();
        let provider_target = owner.fork_thread(connection, thread_id)?;
        let (continuation, target_ref, kind) = match provider_target {
            Some(target_thread_id) => {
                let mut store =
                    ManagedThreadStore::open(self.paths.data_dir(), connection, &self.workspace)?;
                let identity_version = store.identity_version_for(thread_id).map(str::to_owned);
                store.retain_thread(
                    target,
                    target_thread_id.clone(),
                    identity_version.as_deref(),
                )?;
                (
                    ConversationBranchContinuation::ManagedNativeFork {
                        connection: connection.to_owned(),
                        source_thread_id: thread_id.to_owned(),
                        target_thread_id: target_thread_id.clone(),
                    },
                    ConversationRef::Managed {
                        conversation_id: target,
                        connection: connection.to_owned(),
                        thread_id: target_thread_id,
                    },
                    BranchKind::ManagedNativeFork,
                )
            }
            None => (
                ConversationBranchContinuation::ManagedFreshContinuation {
                    connection: connection.to_owned(),
                    source_thread_id: thread_id.to_owned(),
                },
                ConversationRef::NewManaged {
                    conversation_id: target,
                    connection: connection.to_owned(),
                },
                BranchKind::ManagedFreshContinuation,
            ),
        };
        let record = ConversationBranchRecord {
            source_conversation: source,
            source_point: format!("managed-thread:{thread_id}"),
            workspace_root: self.workspace.clone(),
            continuation,
            shared_entry_count: 0,
        };
        if let Err(error) = ProfileStore::open(self.paths).commit_branch(target, record) {
            if let ConversationRef::Managed {
                connection,
                thread_id,
                ..
            } = &target_ref
            {
                let mut store =
                    ManagedThreadStore::open(self.paths.data_dir(), connection, &self.workspace)?;
                let removed = store.archive_thread(thread_id).with_context(|| {
                    format!("branch profile commit failed ({error}); local handle cleanup failed")
                })?;
                if !removed {
                    bail!(
                        "branch profile commit failed ({error}); retained managed target disappeared before cleanup"
                    );
                }
            }
            return Err(error).context("could not preserve the branch Profile and lineage");
        }
        Ok(ConversationBranchReceipt {
            source,
            source_point: format!("managed-thread:{thread_id}"),
            target,
            target_ref,
            kind,
            shared_entry_count: 0,
        })
    }
}

#[cfg(test)]
fn branch_record(
    paths: &XanaPaths,
    target: ConversationId,
) -> Result<Option<ConversationBranchRecord>> {
    Ok(
        read_document::<ProjectRegistryDocument>(&paths.projects_file())?
            .conversation_branches
            .remove(&target),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode, XanaConfig},
        message::{Message, Role},
        private_state::ensure_interoperable_records,
        profile::ProfileStore,
        session::{SessionRecord, SessionStore, reduce},
        shell::ShellConfig,
    };
    use std::{ffi::OsString, fs};
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, XanaPaths, PathBuf) {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(OsString::from(directory.path()))).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(
            paths.config_file(),
            XanaConfig::render_initial(InitialConfig {
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
            .unwrap(),
        )
        .unwrap();
        ensure_interoperable_records(&paths).unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        (directory, paths, workspace.canonicalize().unwrap())
    }

    fn freeze_default(paths: &XanaPaths, conversation: ConversationId) {
        let store = ProfileStore::open(paths);
        let profile = store.resolve_global("default").unwrap();
        store.freeze(&conversation.to_string(), &profile).unwrap();
    }

    #[test]
    fn native_branch_reuses_immutable_history_and_preserves_the_source() {
        let (_directory, paths, workspace) = fixture();
        let mut source = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let source_id = ConversationId::for_native(source.session_id());
        freeze_default(&paths, source_id);
        let source_profile = ProfileStore::open(&paths)
            .snapshot(&source_id.to_string())
            .unwrap()
            .unwrap();
        let first = source
            .append_message(Message::text(Role::User, "first"))
            .unwrap();
        let second = source
            .append_message(Message::text(Role::Assistant, "second"))
            .unwrap();
        source
            .append_message(Message::text(Role::User, "third"))
            .unwrap();
        let source_session_id = source.session_id();
        drop(source);

        let receipt = ConversationBranchService::open(&paths, &workspace)
            .unwrap()
            .branch(source_id, &second.to_string())
            .unwrap();
        let ConversationRef::Native { session_id: target } = receipt.target_ref else {
            panic!("expected native branch target");
        };
        let (_, source_after) =
            DurableSession::inspect_restored(paths.data_dir(), source_session_id).unwrap();
        let (_, target_after) = DurableSession::inspect_restored(paths.data_dir(), target).unwrap();
        let target_summary = DurableSession::inspect(paths.data_dir(), target).unwrap();
        let source_entries = source_after.conversation_entry_path().unwrap();
        let target_entries = target_after.conversation_entry_path().unwrap();

        assert_eq!(source_entries.len(), 3);
        assert_eq!(target_entries.len(), 2);
        assert_eq!(target_entries[0].id, first);
        assert_eq!(target_entries[1].id, second);
        assert_eq!(target_summary.active_entry_count, 2);
        assert_eq!(target_summary.recent_active_entry_ids, vec![first, second]);
        assert_eq!(receipt.shared_entry_count, 2);
        assert_eq!(target_after.branch.unwrap().source_entry_id, second);
        assert_eq!(
            ProfileStore::open(&paths)
                .snapshot(&receipt.target.to_string())
                .unwrap()
                .unwrap(),
            source_profile
        );
        let target_path = SessionStore::path_for(&paths.data_dir().join("sessions"), target);
        let mut tampered = SessionStore::inspect(&target_path).unwrap().records;
        let SessionRecord::ConversationBranched { lineage } = &mut tampered[1].record else {
            panic!("branch lineage must be the second record");
        };
        lineage.shared_entry_count += 1;
        assert!(reduce(&tampered).is_err());
        assert_eq!(
            ProfileStore::open(&paths)
                .predecessor(&receipt.target.to_string())
                .unwrap()
                .as_deref(),
            Some(source_id.to_string().as_str())
        );
    }

    #[test]
    fn an_invalid_native_branch_point_creates_no_target() {
        let (_directory, paths, workspace) = fixture();
        let source = DurableSession::create(paths.data_dir(), workspace.clone()).unwrap();
        let source_id = ConversationId::for_native(source.session_id());
        freeze_default(&paths, source_id);
        drop(source);
        let before = DurableSession::list_for_workspace(paths.data_dir(), &workspace)
            .unwrap()
            .len();

        assert!(
            ConversationBranchService::open(&paths, &workspace)
                .unwrap()
                .branch(source_id, &ConversationEntryId::new().to_string())
                .is_err()
        );
        assert_eq!(
            DurableSession::list_for_workspace(paths.data_dir(), &workspace)
                .unwrap()
                .len(),
            before
        );
    }

    struct FakeFork;

    impl ManagedBranchOwner for FakeFork {
        fn fork_thread(
            &mut self,
            _connection: &str,
            source_thread_id: &str,
        ) -> Result<Option<String>> {
            Ok(Some(format!("fork-of-{source_thread_id}")))
        }
    }

    fn managed_source(paths: &XanaPaths, workspace: &Path) -> (ConversationId, String) {
        let source = ConversationId::new();
        let thread = "thread-source".to_owned();
        let mut store = ManagedThreadStore::open(paths.data_dir(), "codex", workspace).unwrap();
        store
            .set_thread(Some(source), Some(thread.clone()), Some("xana-identity-v1"))
            .unwrap();
        drop(store);
        freeze_default(paths, source);
        (source, thread)
    }

    #[test]
    fn unsupported_managed_fork_records_an_explicit_fresh_boundary() {
        let (_directory, paths, workspace) = fixture();
        let (source, thread) = managed_source(&paths, &workspace);
        let receipt = ConversationBranchService::open(&paths, &workspace)
            .unwrap()
            .branch(source, "current")
            .unwrap();

        assert_eq!(receipt.kind, BranchKind::ManagedFreshContinuation);
        assert_eq!(receipt.shared_entry_count, 0);
        assert!(matches!(
            receipt.target_ref,
            ConversationRef::NewManaged { .. }
        ));
        assert!(matches!(
            branch_record(&paths, receipt.target).unwrap().unwrap().continuation,
            ConversationBranchContinuation::ManagedFreshContinuation {
                source_thread_id,
                ..
            } if source_thread_id == thread
        ));
        let snapshot = WorkspaceHost::open(paths.data_dir(), &workspace)
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(snapshot.conversations.iter().any(|projection| {
            matches!(
                projection.conversation,
                ConversationRef::NewManaged { conversation_id, .. }
                    if conversation_id == receipt.target
            )
        }));
    }

    #[test]
    fn fake_managed_owner_native_fork_retains_both_owner_identities() {
        let (_directory, paths, workspace) = fixture();
        let (source, thread) = managed_source(&paths, &workspace);
        let mut owner = FakeFork;
        let receipt = ConversationBranchService::open(&paths, &workspace)
            .unwrap()
            .branch_with_owner(source, "current", &mut owner)
            .unwrap();

        assert_eq!(receipt.kind, BranchKind::ManagedNativeFork);
        assert!(matches!(
            &receipt.target_ref,
            ConversationRef::Managed {
                conversation_id,
                connection,
                thread_id,
            } if *conversation_id == receipt.target
                && connection == "codex"
                && thread_id == &format!("fork-of-{thread}")
        ));
        let snapshot = WorkspaceHost::open(paths.data_dir(), &workspace)
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(snapshot.conversations.iter().any(|projection| {
            projection.conversation.conversation_id() == Some(receipt.target)
        }));
    }
}
