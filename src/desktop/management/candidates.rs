//! Typed owner review over the shared candidate lifecycle; no installation capability.
use super::{DesktopControlPlane, DesktopError, MemoryScope, control_error};
use crate::memory::candidates::{CandidateEdit, CandidateTargetKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopCandidateKind {
    Memory,
    Skill,
}

pub enum DesktopCandidateCommand {
    List {
        scope: MemoryScope,
        after: Option<u64>,
    },
    Inspect {
        id: String,
    },
    Approve {
        id: String,
        revision: u64,
        confirm_sensitive: bool,
    },
    Reject {
        id: String,
        revision: u64,
        reason: String,
    },
    Archive {
        id: String,
        revision: u64,
    },
    Undo {
        id: String,
        revision: u64,
    },
    StageSkill {
        scope: MemoryScope,
        name: String,
        markdown: String,
    },
}

pub struct DesktopCandidateRow {
    pub id: String,
    pub revision: u64,
    pub label: String,
}

pub enum DesktopCandidateResult {
    Page {
        rows: Vec<DesktopCandidateRow>,
        next_after: Option<u64>,
    },
    Inspected {
        id: String,
        revision: u64,
        detail: String,
        can_approve: bool,
        kind: DesktopCandidateKind,
    },
    Changed {
        detail: String,
    },
}

impl DesktopControlPlane {
    pub fn learning_candidate(
        &self,
        command: DesktopCandidateCommand,
    ) -> Result<DesktopCandidateResult, DesktopError> {
        let owner = self.personal_memory_owner()?;
        apply(&owner, command).map_err(control_error)
    }
}

fn apply(
    owner: &crate::memory::MemoryOwner,
    command: DesktopCandidateCommand,
) -> anyhow::Result<DesktopCandidateResult> {
    let (id, revision, edit) = match command {
        DesktopCandidateCommand::List { scope, after } => {
            let page = owner.candidate_page(Some(&scope), after)?;
            return Ok(DesktopCandidateResult::Page {
                rows: page
                    .records
                    .into_iter()
                    .map(|row| DesktopCandidateRow {
                        id: row.id.to_string(),
                        revision: row.revision,
                        label: format!(
                            "{:?} · {:?} · {:?} · {}",
                            row.target_kind, row.state, row.risk, row.id
                        ),
                    })
                    .collect(),
                next_after: page.next_after,
            });
        }
        DesktopCandidateCommand::Inspect { id } => {
            let value = owner.candidate(id.parse()?)?;
            return Ok(DesktopCandidateResult::Inspected {
                id,
                revision: value.record.revision,
                detail: serde_json::to_string_pretty(&value)?,
                can_approve: value.can_approve,
                kind: match value.record.target_kind() {
                    CandidateTargetKind::Memory => DesktopCandidateKind::Memory,
                    CandidateTargetKind::Skill => DesktopCandidateKind::Skill,
                },
            });
        }
        DesktopCandidateCommand::StageSkill {
            scope,
            name,
            markdown,
        } => {
            let value = owner.stage_skill(scope, name, markdown)?;
            return Ok(DesktopCandidateResult::Changed {
                detail: serde_json::to_string_pretty(&value)?,
            });
        }
        DesktopCandidateCommand::Approve {
            id,
            revision,
            confirm_sensitive,
        } => (id, revision, CandidateEdit::Approve { confirm_sensitive }),
        DesktopCandidateCommand::Reject {
            id,
            revision,
            reason,
        } => (id, revision, CandidateEdit::Reject { reason }),
        DesktopCandidateCommand::Archive { id, revision } => (id, revision, CandidateEdit::Archive),
        DesktopCandidateCommand::Undo { id, revision } => (id, revision, CandidateEdit::Undo),
    };
    let value = owner.review_candidate(id.parse()?, revision, edit)?;
    Ok(DesktopCandidateResult::Changed {
        detail: serde_json::to_string_pretty(&value)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryContext, MemoryOwner},
        storage::{ProtectedStore, RecoveryIdentity, TestCustody},
    };

    #[test]
    fn desktop_candidate_commands_use_same_revision_gate_and_no_installation() {
        let directory = tempfile::tempdir().unwrap();
        let owner = MemoryOwner::new(
            ProtectedStore::initialize(
                directory.path(),
                &RecoveryIdentity::generate(),
                &TestCustody::default(),
            )
            .unwrap(),
            MemoryContext::default(),
        );
        apply(
            &owner,
            DesktopCandidateCommand::StageSkill {
                scope: MemoryScope::User,
                name: "test-draft".into(),
                markdown: "# Inert text".into(),
            },
        )
        .unwrap();
        let DesktopCandidateResult::Page { rows, next_after } = apply(
            &owner,
            DesktopCandidateCommand::List {
                scope: MemoryScope::User,
                after: None,
            },
        )
        .unwrap() else {
            panic!("page required")
        };
        assert!(next_after.is_none());
        assert_eq!(rows.len(), 1);
        let id = rows[0].id.clone();
        let DesktopCandidateResult::Inspected {
            revision,
            detail,
            can_approve,
            kind,
            ..
        } = apply(&owner, DesktopCandidateCommand::Inspect { id: id.clone() }).unwrap()
        else {
            panic!("inspection required")
        };
        assert!(can_approve);
        assert_eq!(kind, DesktopCandidateKind::Skill);
        assert!(detail.contains("Inert text"));
        let DesktopCandidateResult::Changed { detail } = apply(
            &owner,
            DesktopCandidateCommand::Approve {
                id: id.clone(),
                revision,
                confirm_sensitive: false,
            },
        )
        .unwrap() else {
            panic!("review required")
        };
        assert!(detail.contains("reviewed_only"));
        assert!(
            apply(
                &owner,
                DesktopCandidateCommand::Approve {
                    id,
                    revision,
                    confirm_sensitive: false
                }
            )
            .is_err()
        );
        assert!(!directory.path().join(".agents").exists());
    }
}
