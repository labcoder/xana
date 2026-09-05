//! Copied native branches inherit source quarantine, including preexisting branches.
use super::*;
use crate::session::{MAX_RECORD_BYTES, RecordEnvelope, SESSION_RECORD_VERSION, SessionRecord};

pub(super) fn source_allowed(db: &Connection, id: Uuid) -> Result<bool> {
    let mut current = id;
    let mut seen = Vec::with_capacity(32);
    for depth in 0..32 {
        if seen.contains(&current) {
            return Ok(false);
        }
        seen.push(current);
        if db
            .query_row(
                "SELECT 1 FROM excluded_sources WHERE conversation=?1",
                [current.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Ok(false);
        }
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_sessions WHERE id=?1)",
            [current.to_string()],
            |row| row.get(0),
        )?;
        // An initial non-native ID can be managed or new owner input. Once a
        // native branch declares a parent, absence is not evidence of eligibility.
        if !exists {
            return Ok(depth == 0);
        }
        let mut query =
            db.prepare("SELECT body FROM native_records WHERE session=?1 AND sequence=1")?;
        let mut rows = query.query([current.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(true);
        };
        let body = row.get_ref(0)?.as_blob()?;
        ensure!(
            body.len() <= MAX_RECORD_BYTES,
            "native source lineage exceeds record bound"
        );
        let envelope: RecordEnvelope = serde_json::from_slice(body)?;
        ensure!(
            envelope.session_id.to_string() == current.to_string()
                && envelope.version == SESSION_RECORD_VERSION,
            "native source lineage identity differs"
        );
        match envelope.record {
            SessionRecord::ConversationBranched { lineage } => {
                ensure!(
                    lineage.shared_entry_count > 0,
                    "native source lineage is empty"
                );
                current = lineage.source_session_id.to_string().parse()?;
            }
            _ => return Ok(true),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{SessionId, ThreadId},
        message::{Message, Role},
        session::DurableSession,
        storage::{RecoveryIdentity, TestCustody},
    };

    fn source() -> (
        tempfile::TempDir,
        ProtectedStore,
        SessionId,
        crate::identity::ConversationEntryId,
    ) {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        let id = SessionId::new();
        let mut session =
            DurableSession::create_protected(store.clone(), home.path().into(), id).unwrap();
        let entry = session
            .append_message(Message::text(Role::User, "Private original statement"))
            .unwrap();
        drop(session);
        (home, store, id, entry)
    }
    #[test]
    fn forgotten_source_quarantines_existing_descendants_but_keeps_history_inspectable() {
        let (_home, store, parent, point) = source();
        let child = SessionId::new();
        let grandchild = SessionId::new();
        store
            .branch_history(parent, point, child, ThreadId::new())
            .unwrap();
        store
            .branch_history(child, point, grandchild, ThreadId::new())
            .unwrap();
        assert!(
            store
                .source_eligible(grandchild.to_string().parse().unwrap())
                .unwrap()
        );
        let owner = crate::memory::MemoryOwner::new(
            store.clone(),
            crate::memory::MemoryContext {
                conversation: Some(parent.to_string().parse().unwrap()),
                ..Default::default()
            },
        );
        let fact = owner
            .remember(
                crate::memory::MemoryScope::User,
                "Private original statement".into(),
                None,
            )
            .unwrap();
        owner
            .revise(fact.id, fact.revision, crate::memory::MemoryEdit::Forget)
            .unwrap();
        assert!(
            !store
                .source_eligible(child.to_string().parse().unwrap())
                .unwrap()
        );
        assert!(
            !store
                .source_eligible(grandchild.to_string().parse().unwrap())
                .unwrap()
        );
        assert_eq!(
            store
                .history_page(grandchild, None, Some(0), 8)
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(
            store
                .branch_history(grandchild, point, SessionId::new(), ThreadId::new())
                .is_err()
        );
    }
    #[test]
    fn missing_native_ancestor_is_not_a_managed_source_and_cycles_fail_closed() {
        let (_home, store, parent, point) = source();
        let child = SessionId::new();
        store
            .branch_history(parent, point, child, ThreadId::new())
            .unwrap();
        let managed = Uuid::new_v4();
        assert!(store.source_eligible(managed).unwrap());
        store
            .with_database(|db| {
                db.connection.execute(
                    "DELETE FROM native_sessions WHERE id=?1",
                    [parent.to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            !store
                .source_eligible(child.to_string().parse().unwrap())
                .unwrap()
        );
        store
            .with_database(|db| {
                let record = RecordEnvelope::new(
                    child,
                    SessionRecord::ConversationBranched {
                        lineage: crate::session::NativeBranchLineage {
                            source_session_id: child,
                            source_entry_id: point,
                            shared_entry_count: 1,
                        },
                    },
                );
                db.connection.execute(
                    "UPDATE native_records SET body=?1 WHERE session=?2 AND sequence=1",
                    params![serde_json::to_vec(&record)?, child.to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            !store
                .source_eligible(child.to_string().parse().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn lineage_depth_is_bounded_without_loading_any_message_history() {
        let (_home, store, mut parent, point) = source();
        for _ in 0..32 {
            let child = SessionId::new();
            store
                .branch_history(parent, point, child, ThreadId::new())
                .unwrap();
            parent = child;
        }
        assert!(
            !store
                .source_eligible(parent.to_string().parse().unwrap())
                .unwrap()
        );
        assert_eq!(
            store
                .history_page(parent, None, Some(0), 8)
                .unwrap()
                .messages
                .len(),
            1
        );
    }
}
