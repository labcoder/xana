//! Owner-only Desktop controls over the same governed API as terminal commands.
use super::{DesktopControlPlane, DesktopError, control_error};
pub use crate::memory::{
    MemoryClaim, MemoryControlEdit, MemoryControls, MemoryEdit, MemoryPage, MemoryRecord,
    MemoryScope, MemoryState, SourceDeletionPreview, SourceDeletionReceipt,
};
use crate::{
    memory::{MemoryContext, MemoryOwner},
    storage::ProtectedStore,
};
use std::path::Path;
use uuid::Uuid;

pub struct DesktopMemorySnapshot {
    pub learning_status: String,
    pub page: MemoryPage,
    pub controls: MemoryControls,
    pub scope_options: Vec<(String, String)>,
    pub restore_review_required: bool,
}

pub enum DesktopMemoryMutation {
    Remember {
        scope: MemoryScope,
        statement: String,
        expires_at: Option<u64>,
    },
    Revise {
        id: Uuid,
        revision: u64,
        edit: MemoryEdit,
    },
    Controls {
        scope: MemoryScope,
        edit: MemoryControlEdit,
    },
}

impl DesktopControlPlane {
    fn personal_memory_owner(&self) -> Result<MemoryOwner, DesktopError> {
        let store = ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| {
                control_error(
                    "Personal memory requires protected storage; no plaintext memory was created",
                )
            })?;
        Ok(MemoryOwner::new(store, MemoryContext::default()))
    }
    pub fn personal_memory_snapshot(
        &self,
        scope: MemoryScope,
        after: Option<u64>,
    ) -> Result<DesktopMemorySnapshot, DesktopError> {
        let owner = self.personal_memory_owner()?;
        let restore_review_required = owner
            .store
            .memory_requires_review()
            .map_err(control_error)?;
        let page = owner.page(Some(&scope), after).map_err(control_error)?;
        let controls = owner
            .controls(scope.clone(), MemoryControlEdit::default())
            .map_err(control_error)?;
        let mut scope_options = vec![("user".into(), "All conversations (user-wide)".into())];
        let management = self.management_snapshot()?;
        scope_options.extend(
            management
                .profiles
                .into_iter()
                .map(|p| (format!("profile:{}", p.id), format!("Profile: {}", p.name))),
        );
        scope_options.extend(
            management
                .projects
                .into_iter()
                .map(|p| (format!("project:{}", p.id), format!("Project: {}", p.name))),
        );
        if !scope_options
            .iter()
            .any(|(key, _)| key == &scope.to_string())
        {
            scope_options.push((scope.to_string(), scope.to_string()));
        }
        Ok(DesktopMemorySnapshot {
            learning_status: serde_json::to_string(
                &owner.store.learning_status().map_err(control_error)?,
            )
            .map_err(control_error)?,
            page,
            controls,
            scope_options,
            restore_review_required,
        })
    }
    pub fn mutate_personal_memory(
        &self,
        request: DesktopMemoryMutation,
    ) -> Result<String, DesktopError> {
        apply(&self.personal_memory_owner()?, request).map_err(control_error)
    }
    pub fn export_personal_memory(
        &self,
        scope: MemoryScope,
        path: &Path,
    ) -> Result<u64, DesktopError> {
        self.personal_memory_owner()?
            .export(Some(&scope), path)
            .map_err(control_error)
    }
    pub fn personal_memory_source_preview(
        &self,
        conversation: Uuid,
    ) -> Result<SourceDeletionPreview, DesktopError> {
        self.personal_memory_owner()?
            .deletion_preview(conversation)
            .map_err(control_error)
    }
    pub fn delete_personal_memory_source(
        &self,
        conversation: Uuid,
        review: &str,
    ) -> Result<SourceDeletionReceipt, DesktopError> {
        self.personal_memory_owner()?
            .delete_source(conversation, review)
            .map_err(control_error)
    }
}

fn apply(owner: &MemoryOwner, request: DesktopMemoryMutation) -> anyhow::Result<String> {
    Ok(match request {
        DesktopMemoryMutation::Remember {
            scope,
            statement,
            expires_at,
        } => {
            let record = owner.remember(scope, statement, expires_at)?;
            format!(
                "Remembered {} revision {} in {}",
                record.id, record.revision, record.scope
            )
        }
        DesktopMemoryMutation::Revise { id, revision, edit } => {
            let record = owner.revise(id, revision, edit)?;
            let notice = if record.state == MemoryState::Forgotten {
                "Forgotten; originating conversations are excluded from automatic reuse. Raw history and provider copies are separate."
            } else {
                "Changes apply on the next eligible read/turn without restarting work. Restoring a fact does not reauthorize excluded history."
            };
            format!(
                "Updated {} to revision {} in {}. {notice}",
                record.id, record.revision, record.scope,
            )
        }
        DesktopMemoryMutation::Controls { scope, edit } => {
            let controls = owner.controls(scope, edit)?;
            format!(
                "Memory controls saved at revision {}. No-memory does not erase history or provider retention.",
                controls.revision
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_memory_uses_governed_revisions_and_scope_confirmation() {
        let home = tempfile::tempdir().unwrap();
        let owner = MemoryOwner::new(
            ProtectedStore::initialize(
                home.path(),
                &crate::storage::RecoveryIdentity::generate(),
                &crate::storage::TestCustody::default(),
            )
            .unwrap(),
            MemoryContext::default(),
        );
        apply(
            &owner,
            DesktopMemoryMutation::Remember {
                scope: MemoryScope::User,
                statement: "Synthetic desktop preference".into(),
                expires_at: None,
            },
        )
        .unwrap();
        let row = owner.page(None, None).unwrap().records.remove(0);
        let target = MemoryScope::Project(Uuid::new_v4());
        assert!(
            apply(
                &owner,
                DesktopMemoryMutation::Revise {
                    id: row.id,
                    revision: row.revision,
                    edit: MemoryEdit::Scope {
                        target: target.clone(),
                        confirm: false
                    }
                }
            )
            .is_err()
        );
        apply(
            &owner,
            DesktopMemoryMutation::Revise {
                id: row.id,
                revision: row.revision,
                edit: MemoryEdit::Scope {
                    target,
                    confirm: true,
                },
            },
        )
        .unwrap();
        assert!(
            apply(
                &owner,
                DesktopMemoryMutation::Revise {
                    id: row.id,
                    revision: row.revision,
                    edit: MemoryEdit::Disable
                }
            )
            .is_err()
        );
    }
}
