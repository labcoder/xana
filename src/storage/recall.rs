//! Rebuildable encrypted lexical index; originals are revalidated on every read.
use super::{ProtectedStore, database::read_usize};
use crate::recall::{CHUNK_BYTES, Citation, IndexedSource, MAX_SOURCE_BYTES, Source};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(super) const SCHEMA:&str="
CREATE TABLE recall_sources(key TEXT PRIMARY KEY,scope TEXT NOT NULL,conversation TEXT REFERENCES native_sessions(id) ON DELETE CASCADE,root TEXT,hash TEXT NOT NULL,body BLOB NOT NULL);
CREATE INDEX recall_scope ON recall_sources(scope,key);
CREATE VIRTUAL TABLE recall_search USING fts5(source UNINDEXED,start UNINDEXED,end UNINDEXED,text,tokenize='unicode61');
CREATE TRIGGER recall_delete_source AFTER DELETE ON recall_sources BEGIN DELETE FROM recall_search WHERE source=old.key; END;
CREATE TABLE recall_progress(conversation TEXT PRIMARY KEY REFERENCES native_sessions(id) ON DELETE CASCADE,scope TEXT NOT NULL,next_sequence INTEGER NOT NULL);
";

impl ProtectedStore {
    pub(crate) fn recall_reset_scope(&self, scope: &str) -> Result<()> {
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM recall_search WHERE source IN(SELECT key FROM recall_sources WHERE scope=?1)", [scope])?;
            tx.execute("DELETE FROM recall_sources WHERE scope=?1", [scope])?;
            tx.execute("DELETE FROM recall_progress WHERE scope=?1", [scope])?;
            tx.execute("DELETE FROM documents WHERE substr(name,1,15)='recall/refresh/' AND json_extract(CAST(body AS TEXT),'$.scope')=?1", [scope])?;
            super::forgetting::advance_generation(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }
    pub(crate) fn recall_root_sources(&self, root: uuid::Uuid) -> Result<Vec<IndexedSource>> {
        self.with_database(|db| {
            let mut statement = db.connection.prepare(
                "SELECT body FROM recall_sources WHERE root=?1 ORDER BY key LIMIT 10001",
            )?;
            let mut rows = statement.query([root.to_string()])?;
            let mut sources = Vec::new();
            while let Some(row) = rows.next()? {
                let body = row.get_ref(0)?.as_blob()?;
                ensure!(
                    body.len() <= 4096 && sources.len() < 10_000,
                    "notes export inventory exceeds bound"
                );
                sources.push(serde_json::from_slice(body)?);
            }
            Ok(sources)
        })
    }
    pub(crate) fn recall_policy(&self, name: &str, body: Option<&[u8]>) -> Result<()> {
        ensure!(
            name.starts_with("recall/")
                && name.len() <= 256
                && body.is_none_or(|body| body.len() <= 16 * 1024),
            "invalid recall policy document"
        );
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match body {
                Some(body)=>{tx.execute("INSERT INTO documents VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body",params![name,body])?;},
                None=>{tx.execute("DELETE FROM documents WHERE name=?1",[name])?;},
            }
            super::forgetting::advance_generation(&tx)?;
            tx.commit()?;Ok(())
        })
    }
    /// Checkpoint a bounded notes scan without advancing privacy generation.
    /// CAS rejects competing/stale cursor owners; only a complete scan may prune.
    pub(crate) fn recall_refresh_checkpoint(
        &self,
        root: uuid::Uuid,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        generation: u64,
        seen: Option<&[String]>,
    ) -> Result<()> {
        const MAX_CURSOR: usize = 2 * 1024 * 1024;
        ensure!(
            expected.is_none_or(|bytes| bytes.len() <= MAX_CURSOR)
                && next.is_none_or(|bytes| bytes.len() <= MAX_CURSOR)
                && seen.is_none_or(|keys| keys.len() <= 10_000)
                && (next.is_some() || seen.is_some()),
            "notes checkpoint exceeds bounds"
        );
        let name = format!("recall/refresh/{root}");
        self.with_database(|db| {
            let tx = db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: u64 = tx.query_row(
                "SELECT revision FROM privacy_generation WHERE singleton=1", [],
                |r| super::database::read_u64(r, 0))?;
            ensure!(current == generation && !super::forgetting::memory_review_required(&tx)?,
                "notes privacy policy changed; restart the selected-root refresh after review");
            let actual = super::documents::read(&tx, &name, MAX_CURSOR)?;
            ensure!(actual.as_deref() == expected, "notes cursor changed concurrently; retry refresh");
            ensure!(super::documents::read(&tx, &format!("recall/roots/{root}"), 16 * 1024)?.is_some(),
                "knowledge root was revoked");
            match next {
                Some(body) => {
                    tx.execute("INSERT INTO documents VALUES(?1,1,?2) ON CONFLICT(name) DO UPDATE SET revision=revision+1,body=excluded.body", params![name,body])?;
                }
                None => {
                    let retained = seen.context("completed notes scan lacks its inventory")?
                        .iter().map(String::as_str).collect::<std::collections::HashSet<_>>();
                    let keys = {
                        let mut query = tx.prepare("SELECT key FROM recall_sources WHERE root=?1 LIMIT 10001")?;
                        query.query_map([root.to_string()], |r| r.get::<_, String>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?
                    };
                    ensure!(keys.len() <= 10_000, "notes index cleanup exceeds bound");
                    for key in keys {
                        if !retained.contains(key.as_str()) {
                            tx.execute("DELETE FROM recall_sources WHERE key=?1", [key])?;
                        }
                    }
                    tx.execute("DELETE FROM documents WHERE name=?1", [name])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }
    pub(crate) fn recall_index(
        &self,
        source: &IndexedSource,
        text: &str,
        generation: u64,
    ) -> Result<()> {
        ensure!(
            text.len() <= MAX_SOURCE_BYTES && source.key.len() <= 2048 && source.scope.len() <= 512,
            "recall source exceeds index bounds"
        );
        let body = serde_json::to_vec(source)?;
        ensure!(
            body.len() <= 4096,
            "recall citation metadata exceeds its bound"
        );
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current:u64=tx.query_row("SELECT revision FROM privacy_generation WHERE singleton=1",[],|r|super::database::read_u64(r,0))?;
            ensure!(current==generation,"recall privacy policy changed before index commit");
            let (conversation,root)=match &source.source {Source::Conversation{conversation,..} | Source::Artifact{conversation,..}=>{
                ensure!(super::forgetting::source_allowed(&tx,conversation.to_string().parse()?)?,"recall source is excluded");
                (Some(conversation.to_string()),None)
            },Source::File{root,..}=>(None,Some(root.to_string()))};
            let blocked=super::forgetting::memory_review_required(&tx)?;
            ensure!(!blocked,"recall is suspended until restore privacy review is complete");
            let old:Option<(String,String)>=tx.query_row("SELECT hash,scope FROM recall_sources WHERE key=?1",[&source.key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if old.as_ref().is_some_and(|(hash,scope)|hash==&source.hash&&scope==&source.scope) {return Ok(());}
            tx.execute("DELETE FROM recall_search WHERE source=?1",[&source.key])?;
            tx.execute("INSERT INTO recall_sources VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(key) DO UPDATE SET scope=excluded.scope,conversation=excluded.conversation,root=excluded.root,hash=excluded.hash,body=excluded.body",params![source.key,source.scope,conversation,root,source.hash,body])?;
            let mut start=0;
            while start<text.len() {
                let mut end=(start+CHUNK_BYTES).min(text.len());
                while !text.is_char_boundary(end) {end-=1;}
                tx.execute("INSERT INTO recall_search(source,start,end,text) VALUES(?1,?2,?3,?4)",params![source.key,i64::try_from(start)?,i64::try_from(end)?,&text[start..end]])?;
                start=end;
            }
            tx.commit()?;
            Ok(())
        })
    }
    pub(crate) fn recall_candidates(
        &self,
        query: &str,
        scope: &str,
        included: &[crate::identity::SessionId],
    ) -> Result<Vec<Citation>> {
        ensure!(
            included.len() <= 32,
            "explicit recall inclusion limit exceeded"
        );
        let terms = query.split_whitespace().take(9).collect::<Vec<_>>();
        ensure!(
            !terms.is_empty() && terms.len() <= 8,
            "recall query supports1..8 literal terms"
        );
        let expression = terms
            .into_iter()
            .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        self.with_database(|db| {
            let selected=included.iter().map(ToString::to_string).collect::<Vec<_>>();
            let inclusion=if selected.is_empty(){String::new()}else{format!(" OR s.conversation IN({})",(3..3+selected.len()).map(|index|format!("?{index}")).collect::<Vec<_>>().join(","))};
            let sql=format!("SELECT s.body,f.start,f.end FROM recall_search f JOIN recall_sources s ON s.key=f.source WHERE recall_search MATCH ?1 AND (s.scope=?2{inclusion}) AND (s.conversation IS NULL OR NOT EXISTS(SELECT 1 FROM excluded_sources e WHERE e.conversation=s.conversation)) ORDER BY rank LIMIT 64");
            let mut query=db.connection.prepare(&sql)?;
            let mut parameters:Vec<&dyn rusqlite::ToSql>=vec![&expression,&scope];parameters.extend(selected.iter().map(|value|value as &dyn rusqlite::ToSql));
            let mut rows=query.query(parameters.as_slice())?;
            let mut output=Vec::new();
            while let Some(row)=rows.next()? {
                let body=row.get_ref(0)?.as_blob()?;
                ensure!(body.len()<=4096,"recall source metadata exceeds bound");
                let source:IndexedSource=serde_json::from_slice(body)?;
                output.push(Citation{source:source.source,source_hash:source.hash,start:read_usize(row,1)?,end:read_usize(row,2)?});
            }
            Ok(output)
        })
    }
    pub(crate) fn recall_forget_root(&self, root: uuid::Uuid) -> Result<()> {
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM recall_search WHERE source IN(SELECT key FROM recall_sources WHERE root=?1)",[root.to_string()])?;
            tx.execute("DELETE FROM recall_sources WHERE root=?1",[root.to_string()])?;
            tx.execute("DELETE FROM documents WHERE name=?1",[format!("recall/refresh/{root}")])?;
            tx.commit()?; Ok(())
        })
    }
    pub(crate) fn recall_history_batch(
        &self,
        id: crate::identity::SessionId,
        scope: &str,
    ) -> Result<(usize, Vec<crate::session::RecordEnvelope>, bool)> {
        self.with_database(|db| {
            let start:usize=db.connection.query_row("SELECT next_sequence FROM recall_progress WHERE conversation=?1 AND scope=?2",params![id.to_string(),scope],|r|read_usize(r,0)).optional()?.unwrap_or(0);
            let mut query=db.connection.prepare("SELECT sequence,body FROM native_records WHERE session=?1 AND sequence>=?2 ORDER BY sequence LIMIT 128")?;
            let mut rows=query.query(params![id.to_string(),i64::try_from(start)?])?;
            let mut output=Vec::new();let mut bytes=0;let mut next=start;
            while let Some(row)=rows.next()? {
                ensure!(read_usize(row,0)?==next,"recall journal cursor is discontinuous");
                let body=row.get_ref(1)?.as_blob()?;
                ensure!(body.len()<=crate::session::MAX_RECORD_BYTES,"recall history record exceeds bound");
                if bytes+body.len()>2*1024*1024 {break;}
                bytes+=body.len();
                output.push(serde_json::from_slice(body)?); next+=1;
            }
            let revision:usize=db.connection.query_row("SELECT revision FROM native_sessions WHERE id=?1",[id.to_string()],|r|read_usize(r,0))?;
            Ok((next,output,next<revision))
        })
    }
    pub(crate) fn recall_advance(
        &self,
        id: crate::identity::SessionId,
        scope: &str,
        next: usize,
    ) -> Result<()> {
        self.with_database(|db|{db.connection.execute("INSERT INTO recall_progress VALUES(?1,?2,?3) ON CONFLICT(conversation) DO UPDATE SET scope=excluded.scope,next_sequence=CASE WHEN scope=excluded.scope THEN MAX(next_sequence,excluded.next_sequence) ELSE excluded.next_sequence END",params![id.to_string(),scope,i64::try_from(next)?])?;Ok(())})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recall_deleted_history_cascades_derived_text_and_cursor_without_deleting_notes() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let id = crate::identity::SessionId::new();
        let mut session =
            crate::session::DurableSession::create_protected(store.clone(), home.path().into(), id)
                .unwrap();
        let entry = session
            .append_message(crate::message::Message::text(
                crate::message::Role::User,
                "Source cobalt",
            ))
            .unwrap();
        drop(session);
        let scope = format!("conversation:{id}");
        let generation = store.privacy_generation().unwrap();
        store
            .recall_index(
                &IndexedSource {
                    key: "history".into(),
                    scope: scope.clone(),
                    source: Source::Conversation {
                        conversation: id,
                        entry,
                    },
                    hash: "fixture".into(),
                },
                "Source cobalt",
                generation,
            )
            .unwrap();
        store
            .recall_index(
                &IndexedSource {
                    key: "notes".into(),
                    scope: scope.clone(),
                    source: Source::File {
                        root: uuid::Uuid::new_v4(),
                        relative_path: "note.md".into(),
                    },
                    hash: "fixture".into(),
                },
                "Selected aurora",
                generation,
            )
            .unwrap();
        store.recall_advance(id, &scope, 1).unwrap();
        let preview = store
            .source_deletion_preview(id.to_string().parse().unwrap())
            .unwrap();
        store
            .delete_source_history(id.to_string().parse().unwrap(), &preview.review, 1)
            .unwrap();
        store.with_database(|db|{
            let (sources, chunks, cursors):(i64,i64,i64)=db.connection.query_row("SELECT (SELECT COUNT(*) FROM recall_sources),(SELECT COUNT(*) FROM recall_search),(SELECT COUNT(*) FROM recall_progress)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            assert_eq!((sources,chunks,cursors),(1,1,0));
            let text:String=db.connection.query_row("SELECT text FROM recall_search",[],|r|r.get(0))?;
            assert_eq!(text,"Selected aurora");Ok(())
        }).unwrap();
    }
}
