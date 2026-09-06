//! Indexed personal records with transactional revision/eligibility boundaries.

use super::ProtectedStore;
use crate::memory::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{io::Write, path::Path};
use uuid::Uuid;

impl ProtectedStore {
    pub(crate) fn record_memory_handoff(
        &self,
        conversation: Uuid,
        generation: u64,
        use_enabled: bool,
        ids: &[Uuid],
    ) -> Result<()> {
        ensure!(ids.len() <= 1024, "memory receipt exceeds its bound");
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current=tx.query_row("SELECT revision FROM privacy_generation WHERE singleton=1",[],|r|super::database::read_u64(r,0))?;
            ensure!(current==generation,"Memory changed before dispatch; retry the turn");
            ensure!(super::forgetting::source_allowed(&tx,conversation)?,"This Conversation contains a forgotten source; start a new Conversation");
            let key=format!("memory/handoff/{conversation}");
            let stored:Option<(usize,Option<Vec<u8>>)>=tx.query_row("SELECT length(body),CASE WHEN length(body)<=131072 THEN body END FROM documents WHERE name=?1",[&key],|r|Ok((super::database::read_usize(r,0)?,r.get(1)?))).optional()?;
            let mut seen:Vec<Uuid>=match stored {
                Some((length,bytes))=>{ensure!(length<=128*1024,"memory receipt exceeds read bound");serde_json::from_slice(&bytes.context("invalid memory receipt")?)?},
                None=>Vec::new(),
            };
            ensure!(seen.len()<=1024,"memory receipt exceeds identity bound");
            for id in &seen {ensure!(get(&tx,*id)?.state!=MemoryState::Forgotten,"A previously sent memory was forgotten; start a new Conversation");}
            for id in ids {if !seen.contains(id) {seen.push(*id);}}
            ensure!(seen.len()<=1024,"This Conversation reached its bounded memory handoff history; start a new Conversation");
            ensure!(seen.is_empty() || use_enabled,"Memory use is disabled but this Conversation already received memory; start a new Conversation");
            if !seen.is_empty() {tx.execute("INSERT INTO documents(name,revision,body) VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body",params![key,serde_json::to_vec(&seen)?])?;}
            tx.commit()?;Ok(())
        })
    }
}

pub(super) const SCHEMA: &str = "
CREATE TABLE memory_entries(sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, revision INTEGER NOT NULL CHECK(revision > 0), scope TEXT NOT NULL, body BLOB NOT NULL);
CREATE INDEX memory_scope_page ON memory_entries(scope,sequence);
CREATE TABLE memory_revisions(id TEXT NOT NULL, revision INTEGER NOT NULL, body BLOB NOT NULL, PRIMARY KEY(id,revision));
CREATE TABLE memory_controls(scope TEXT PRIMARY KEY, revision INTEGER NOT NULL, use_enabled INTEGER NOT NULL CHECK(use_enabled IN (0,1)), learning_enabled INTEGER NOT NULL CHECK(learning_enabled IN (0,1)), no_memory INTEGER NOT NULL CHECK(no_memory IN (0,1)));
";

fn read_record(row: &rusqlite::Row<'_>, col: usize) -> rusqlite::Result<MemoryRecord> {
    use rusqlite::types::{Type, ValueRef};
    let decode = || -> Result<MemoryRecord> {
        let ValueRef::Blob(bytes) = row.get_ref(col)? else {
            anyhow::bail!("memory record is not a blob")
        };
        ensure!(
            bytes.len() <= RECORD_BYTES,
            "memory record exceeds its read bound"
        );
        let record: MemoryRecord = serde_json::from_slice(bytes)?;
        record.validate()?;
        Ok(record)
    };
    decode()
        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(col, Type::Blob, error.into()))
}

pub(super) fn encode(record: &MemoryRecord) -> Result<Vec<u8>> {
    record.validate()?;
    let bytes = serde_json::to_vec(record)?;
    ensure!(
        bytes.len() <= RECORD_BYTES,
        "encoded memory record exceeds its bound"
    );
    Ok(bytes)
}

/// Every index-backed read validates its routing metadata before exposing data.
fn indexed_record(row: &rusqlite::Row<'_>, col: usize) -> rusqlite::Result<MemoryRecord> {
    let record = read_record(row, col)?;
    let id: String = row.get(col + 1)?;
    let revision = super::database::read_u64(row, col + 2)?;
    let scope: String = row.get(col + 3)?;
    if id != record.id.to_string()
        || revision != record.revision
        || scope != record.scope.to_string()
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Blob,
            anyhow::anyhow!("memory routing metadata differs from its record").into(),
        ));
    }
    Ok(record)
}

pub(super) fn get(db: &Connection, id: Uuid) -> Result<MemoryRecord> {
    let record = db
        .query_row(
            "SELECT body,id,revision,scope FROM memory_entries WHERE id=?1",
            [id.to_string()],
            |row| indexed_record(row, 0),
        )
        .optional()?
        .context("memory record not found")?;
    ensure!(record.id == id, "memory record identity differs");
    Ok(record)
}

pub(super) fn controls(db: &Connection, scope: MemoryScope) -> Result<MemoryControls> {
    let stored = db.query_row("SELECT revision,use_enabled,learning_enabled,no_memory FROM memory_controls WHERE scope=?1", [scope.to_string()], |r| Ok((super::database::read_u64(r,0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    Ok(match stored {
        Some((revision, use_enabled, learning_enabled, no_memory)) => MemoryControls {
            scope,
            revision,
            use_enabled,
            learning_enabled,
            no_memory,
        },
        None => MemoryControls::defaults(scope),
    })
}

impl ProtectedStore {
    pub(crate) fn memory_insert(&self, record: &MemoryRecord) -> Result<()> {
        let bytes = encode(record)?;
        self.with_database(|db| {
            db.connection.execute(
                "INSERT INTO memory_entries(id,revision,scope,body) VALUES(?1,?2,?3,?4)",
                params![
                    record.id.to_string(),
                    i64::try_from(record.revision)?,
                    record.scope.to_string(),
                    bytes
                ],
            )?;
            Ok(())
        })
    }

    pub(crate) fn memory_record(&self, id: Uuid) -> Result<MemoryRecord> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let record = get(&tx,id)?;
            ensure!(super::candidates::memory_visible(&tx,&record)?,"candidate content is unavailable after source exclusion; inspect its redacted candidate metadata instead");
            Ok(record)
        })
    }

    pub(crate) fn memory_revise(
        &self,
        id: Uuid,
        revision: u64,
        edit: MemoryEdit,
        mut origin: MemoryProvenance,
    ) -> Result<MemoryRecord> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old = get(&tx, id)?;
            ensure!(old.revision == revision, "memory changed since inspection; refresh before applying this edit");
            ensure!(old.state != MemoryState::Forgotten || matches!(edit, MemoryEdit::Restore { confirm: true }), "forgotten memory requires an explicit confirmed restore; old text cannot reactivate it");
            let mut record = old.clone();
            record.revision = record.revision.checked_add(1).context("memory revision exhausted")?;
            origin.at_unix_seconds = origin.at_unix_seconds.max(old.changed.at_unix_seconds);
            record.changed = origin;
            match edit {
                MemoryEdit::Correct { statement, valid_until_unix_seconds } => { record.statement=statement; record.valid_until_unix_seconds=valid_until_unix_seconds; record.state=MemoryState::Active; record.claim=MemoryClaim::Stated; },
                MemoryEdit::Scope { target, confirm } => { ensure!(target == old.scope || confirm, "changing memory scope requires explicit confirmation"); record.scope=target; },
                MemoryEdit::Disable => record.state=MemoryState::Stale,
                MemoryEdit::Forget => {
                    super::forgetting::suppress(&tx, &old, &record.changed)?;
                    record.state = MemoryState::Forgotten;
                },
                MemoryEdit::Restore { confirm } => {
                    ensure!(confirm && old.state == MemoryState::Forgotten, "restoring a forgotten fact requires explicit confirmation");
                    record.state = MemoryState::Active;
                    super::forgetting::restore_fact(&tx, id)?;
                },
            }
            let bytes = encode(&record)?;
            let mut historical = old;
            historical.state = MemoryState::Superseded;
            tx.execute("INSERT INTO memory_revisions(id,revision,body) VALUES(?1,?2,?3)", params![id.to_string(),i64::try_from(revision)?,encode(&historical)?])?;
            ensure!(tx.execute("UPDATE memory_entries SET revision=?2,scope=?3,body=?4 WHERE id=?1 AND revision=?5", params![id.to_string(),i64::try_from(record.revision)?,record.scope.to_string(),bytes,i64::try_from(revision)?])? == 1, "memory revision conflict");
            super::forgetting::advance_generation(&tx)?;
            tx.commit()?;
            Ok(record)
        })
    }

    pub(crate) fn memory_page(
        &self,
        scope: Option<&MemoryScope>,
        after: Option<u64>,
    ) -> Result<MemoryPage> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            // Keep the exact-scope predicate indexable; a nullable OR forces
            // scoped pages to walk unrelated sequence rows before their limit.
            let sql = if scope.is_some() {
                "SELECT sequence,body,id,revision,scope FROM memory_entries WHERE sequence>?1 AND scope=?2 ORDER BY sequence LIMIT ?3"
            } else {
                "SELECT sequence,body,id,revision,scope FROM memory_entries WHERE sequence>?1 ORDER BY sequence LIMIT ?2"
            };
            let mut q = tx.prepare(sql)?;
            let after = i64::try_from(after.unwrap_or(0))?;
            let limit = i64::try_from(PAGE_SIZE + 1)?;
            let mut rows = if let Some(scope) = scope {
                q.query(params![after, scope.to_string(), limit])?
            } else {
                q.query(params![after, limit])?
            };
            let mut records = Vec::new();
            let mut last = None;
            let mut more = false;
            let mut inspected = 0;
            while let Some(row) = rows.next()? {
                if inspected == PAGE_SIZE { more = true; break; }
                last = Some(super::database::read_u64(row,0)?);
                inspected += 1;
                let record = indexed_record(row,1)?;
                if super::candidates::memory_visible(&tx,&record)? { records.push(record); }
            }
            Ok(MemoryPage { records, next_after: more.then_some(last).flatten() })
        })
    }

    pub(crate) fn memory_controls(
        &self,
        scope: MemoryScope,
        edit: MemoryControlEdit,
    ) -> Result<MemoryControls> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut current = controls(&tx, scope)?;
            ensure!(edit.expected_revision.is_none_or(|rev| rev==current.revision), "memory controls changed; refresh before editing");
            if edit.use_enabled.is_some() || edit.learning_enabled.is_some() || edit.no_memory.is_some() {
                current.revision = current.revision.checked_add(1).context("memory control revision exhausted")?;
                if let Some(value)=edit.use_enabled {current.use_enabled=value;}
                if let Some(value)=edit.learning_enabled {current.learning_enabled=value;}
                if let Some(value)=edit.no_memory {current.no_memory=value;}
                tx.execute("INSERT INTO memory_controls VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope) DO UPDATE SET revision=excluded.revision,use_enabled=excluded.use_enabled,learning_enabled=excluded.learning_enabled,no_memory=excluded.no_memory", params![current.scope.to_string(),i64::try_from(current.revision)?,current.use_enabled,current.learning_enabled,current.no_memory])?;
                super::forgetting::advance_generation(&tx)?;
            }
            tx.commit()?;
            Ok(current)
        })
    }

    pub(crate) fn memory_eligible(
        &self,
        context: &MemoryContext,
        now: u64,
    ) -> Result<EligibleMemory> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let gated = super::forgetting::memory_review_required(&tx)?;
            let scopes = context.scopes();
            let mut use_enabled = !gated;
            let mut learning_enabled = !gated;
            for scope in &scopes {
                let flags = controls(&tx, scope.clone())?;
                use_enabled &= flags.use_enabled && !flags.no_memory;
                learning_enabled &= flags.learning_enabled && !flags.no_memory;
            }
            let mut result = EligibleMemory {
                records: Vec::new(),
                use_enabled,
                learning_enabled,
                restore_review_required: gated,
                has_more: false,
            };
            if !use_enabled {
                return Ok(result);
            }
            // One indexed empty-catalog check keeps explicit-only homes on
            // their existing selection path. A read transaction makes this
            // stable for the whole bounded page, even during learning writes.
            let candidate_visibility = super::candidates::memory_visibility_required(&tx)?;
            // Lazily merge at most four covering-index cursors. Stop index work
            // when the candidate page is full, not after reading 1025 IDs from
            // every scope. Keep the same global sequence order and inspect cap.
            let mut queries = scopes
                .iter()
                .map(|_| tx.prepare(ELIGIBLE_INDEX_QUERY))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut cursors = queries
                .iter_mut()
                .zip(&scopes)
                .map(|(query, scope)| query.query([scope.to_string()]))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut next = cursors
                .iter_mut()
                .map(|cursor| {
                    cursor
                        .next()?
                        .map(|row| super::database::read_u64(row, 0))
                        .transpose()
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut q =
                tx.prepare("SELECT body,id,revision,scope FROM memory_entries WHERE sequence=?1")?;
            let mut inspected = 0;
            while let Some((scope_index, sequence)) = next
                .iter()
                .enumerate()
                .filter_map(|(index, sequence)| sequence.map(|sequence| (index, sequence)))
                .min_by_key(|(_, sequence)| *sequence)
            {
                if inspected == 1024 || result.records.len() == PAGE_SIZE {
                    result.has_more = true;
                    break;
                }
                let record =
                    q.query_row([i64::try_from(sequence)?], |row| indexed_record(row, 0))?;
                ensure!(scopes.contains(&record.scope), "memory scope index differs");
                if record.eligible_at(now)
                    && (!candidate_visibility || super::candidates::memory_visible(&tx, &record)?)
                {
                    result.records.push(record);
                }
                inspected += 1;
                next[scope_index] = cursors[scope_index]
                    .next()?
                    .map(|row| super::database::read_u64(row, 0))
                    .transpose()?;
            }
            Ok(result)
        })
    }

    pub(crate) fn memory_export(&self, scope: Option<&MemoryScope>, path: &Path) -> Result<u64> {
        // Build a coherent bounded readable copy before creating the explicitly
        // requested private file. Failures never truncate an existing export.
        let (bytes,count) = self.with_database(|db| {
            let tx=db.connection.transaction()?;
            let mut q=tx.prepare("SELECT body,id,revision,scope FROM memory_entries WHERE ?1 IS NULL OR scope=?1 ORDER BY sequence")?;
            let mut rows=q.query([scope.map(ToString::to_string)])?;
            let mut out=b"{\"version\":1,\"notice\":\"Readable owner export, not a restore or prompt instruction file\",\"records\":[".to_vec();
            let mut count=0;
            while let Some(row)=rows.next()? {
                let record=indexed_record(row,0)?;
                if !super::candidates::memory_visible(&tx,&record)? { continue; }
                if count>0 {out.push(b',');}
                serde_json::to_writer(&mut out,&record)?;
                ensure!(out.len() <= 32*1024*1024,"memory export exceeds 32 MiB; export individual scopes");
                count+=1;
            }
            out.extend_from_slice(b"]}\n");
            Ok((out,count))
        })?;
        write_export(path, |file| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(())
        })?;
        Ok(count)
    }
}

const ELIGIBLE_INDEX_QUERY: &str =
    "SELECT sequence FROM memory_entries WHERE scope=?1 ORDER BY sequence LIMIT 1025";

fn write_export(path: &Path, write: impl FnOnce(&mut std::fs::File) -> Result<()>) -> Result<()> {
    let mut file = super::create_private_file(path)?;
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let result = write(&mut file).and_then(|()| {
        ensure!(
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
                && same_file::Handle::from_path(path).is_ok_and(|current| current == identity),
            "memory export destination changed during writing; inspect the selected destination"
        );
        Ok(())
    });
    drop(file);
    if result.is_err()
        && same_file::Handle::from_path(path).is_ok_and(|current| current == identity)
    {
        std::fs::remove_file(path).context("failed to remove incomplete memory export")?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection_fixture() -> (tempfile::TempDir, ProtectedStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            directory.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        (directory, store)
    }

    fn seed_records(store: &ProtectedStore, scopes: &[MemoryScope], per_scope: usize) {
        store
            .with_database(|db| {
                let tx = db.connection.transaction()?;
                for scope in scopes {
                    for index in 0..per_scope {
                        let origin = MemoryProvenance {
                            owner_request: Uuid::new_v4(),
                            conversation: None,
                            at_unix_seconds: 1,
                        };
                        let record = MemoryRecord {
                            version: 1,
                            id: Uuid::new_v4(),
                            revision: 1,
                            scope: scope.clone(),
                            statement: format!("Fixture preference {index}"),
                            claim: MemoryClaim::Stated,
                            state: MemoryState::Active,
                            created: origin.clone(),
                            changed: origin,
                            valid_until_unix_seconds: None,
                        };
                        tx.execute(
                            "INSERT INTO memory_entries(id,revision,scope,body) VALUES(?1,1,?2,?3)",
                            params![record.id.to_string(), scope.to_string(), encode(&record)?],
                        )?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    }

    fn measured_work<T>(store: &ProtectedStore, operation: impl FnOnce() -> T) -> (T, usize) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let steps = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&steps);
        store
            .with_database(|db| {
                db.connection.progress_handler(
                    1,
                    Some(move || {
                        observed.fetch_add(1, Ordering::Relaxed);
                        false
                    }),
                )?;
                Ok(())
            })
            .unwrap();
        let result = operation();
        store
            .with_database(|db| {
                db.connection.progress_handler(0, None::<fn() -> bool>)?;
                Ok(())
            })
            .unwrap();
        (result, steps.load(Ordering::Relaxed))
    }

    #[test]
    fn memory_scope_page_does_not_scan_unrelated_records() {
        let (_directory, store) = selection_fixture();
        seed_records(&store, &[MemoryScope::Project(Uuid::new_v4())], 4096);
        seed_records(&store, &[MemoryScope::User], 1);
        let (page, steps) =
            measured_work(&store, || store.memory_page(Some(&MemoryScope::User), None));
        let page = page.unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].scope, MemoryScope::User);
        assert_eq!(page.next_after, None);
        println!("memory_scope_page population=4097 returned=1 vm_steps={steps}");
        assert!(
            steps <= 256,
            "scope page scanned unrelated records: {steps} VM steps"
        );
    }

    #[test]
    fn memory_selection_stops_index_work_after_its_candidate_page() {
        let (_directory, store) = selection_fixture();
        let context = MemoryContext {
            conversation: Some(Uuid::new_v4()),
            profile: Some(Uuid::new_v4()),
            project: Some(Uuid::new_v4()),
        };
        seed_records(&store, &context.scopes(), 1025);
        let (selection, steps) = measured_work(&store, || store.memory_eligible(&context, 2));
        let selection = selection.unwrap();
        assert_eq!(selection.records.len(), PAGE_SIZE);
        assert!(selection.has_more);
        assert!(
            selection
                .records
                .iter()
                .all(|record| record.scope == MemoryScope::User)
        );
        println!(
            "memory_selection population=4100 returned={} vm_steps={steps}",
            selection.records.len()
        );
        assert!(
            steps <= 4096,
            "selection eagerly read unused scope candidates: {steps} VM steps"
        );
    }

    #[test]
    fn memory_selection_merges_scopes_in_sequence_order_and_skips_ineligible_records() {
        let (_directory, store) = selection_fixture();
        let context = MemoryContext {
            conversation: Some(Uuid::new_v4()),
            profile: Some(Uuid::new_v4()),
            project: Some(Uuid::new_v4()),
        };
        for _ in 0..20 {
            seed_records(&store, &context.scopes(), 1);
        }
        let first = store.memory_page(None, None).unwrap();
        let second = store.memory_page(None, first.next_after).unwrap();
        let mut expected = first.records;
        expected.extend(second.records);
        store
            .with_database(|db| {
                let tx = db.connection.transaction()?;
                for (index, record) in expected.iter_mut().take(4).enumerate() {
                    if index == 0 {
                        record.valid_until_unix_seconds = Some(2);
                    } else {
                        record.state = [
                            MemoryState::Candidate,
                            MemoryState::Stale,
                            MemoryState::Forgotten,
                        ][index - 1];
                    }
                    tx.execute(
                        "UPDATE memory_entries SET body=?2 WHERE id=?1",
                        params![record.id.to_string(), encode(record)?],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        let selection = store.memory_eligible(&context, 2).unwrap();
        assert_eq!(
            selection.records,
            expected
                .into_iter()
                .filter(|record| record.eligible_at(2))
                .take(PAGE_SIZE)
                .collect::<Vec<_>>()
        );
        assert!(selection.has_more);
    }

    #[test]
    fn memory_selection_keeps_the_global_ineligible_inspection_limit() {
        let (_directory, store) = selection_fixture();
        seed_records(&store, &[MemoryScope::User], 1025);
        store
            .with_database(|db| {
                let tx = db.connection.transaction()?;
                for sequence in 1..=1024 {
                    let mut record = tx.query_row(
                        "SELECT body FROM memory_entries WHERE sequence=?1",
                        [sequence],
                        |row| read_record(row, 0),
                    )?;
                    record.state = MemoryState::Stale;
                    tx.execute(
                        "UPDATE memory_entries SET body=?2 WHERE sequence=?1",
                        params![sequence, encode(&record)?],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        let selection = store.memory_eligible(&MemoryContext::default(), 2).unwrap();
        assert!(
            selection.records.is_empty(),
            "record 1025 remains beyond the read limit"
        );
        assert!(selection.has_more);
    }

    #[test]
    fn memory_scope_pages_keep_cursors_and_reject_mismatched_routing() {
        let (_directory, store) = selection_fixture();
        seed_records(&store, &[MemoryScope::Project(Uuid::new_v4())], 65);
        seed_records(&store, &[MemoryScope::User], 65);
        let first = store.memory_page(Some(&MemoryScope::User), None).unwrap();
        let second = store
            .memory_page(Some(&MemoryScope::User), first.next_after)
            .unwrap();
        assert_eq!(first.records.len(), PAGE_SIZE);
        assert_eq!(second.records.len(), 1);
        assert!(second.next_after.is_none());
        assert!(
            first.records.iter().all(
                |record| record.id != second.records[0].id && record.scope == MemoryScope::User
            )
        );
        store
            .with_database(|db| {
                db.connection.execute(
                    "UPDATE memory_entries SET scope='user' WHERE sequence=1",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store.memory_page(Some(&MemoryScope::User), None).is_err());
        assert!(store.memory_eligible(&MemoryContext::default(), 2).is_err());
    }

    #[test]
    fn memory_eligibility_uses_bounded_covering_index_without_blob_sort() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {ELIGIBLE_INDEX_QUERY}"))
            .unwrap()
            .query_map(["user"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ");
        assert!(plan.contains("COVERING INDEX memory_scope_page"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn memory_failed_export_cleans_only_its_new_file_and_preserves_existing_exports() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.json");
        assert!(
            write_export(&path, |f| {
                f.write_all(b"partial private fact")?;
                anyhow::bail!("injected write failure")
            })
            .is_err()
        );
        assert!(!path.exists());
        std::fs::write(&path, b"existing").unwrap();
        assert!(write_export(&path, |_| panic!("must not open an existing export")).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"existing");
    }

    #[test]
    fn memory_export_rejects_replaced_destination_without_deleting_the_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.json");
        let moved = directory.path().join("moved.json");
        let result = write_export(&path, |file| {
            file.write_all(b"private export")?;
            std::fs::rename(&path, &moved)?;
            std::fs::write(&path, b"replacement")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }
}
