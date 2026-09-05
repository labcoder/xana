//! Atomic bounded-memory native branch creation from an exact active prefix.

use super::*;
use crate::{
    identity::{CompactionId, ConversationEntryId, OperationId, ThreadId},
    session::{
        CompactionCheckpoint, NativeBranchLineage, RestoredSession,
        compaction::CompactionSourceProofBuilder, hydration,
    },
};

impl ProtectedStore {
    pub(crate) fn branch_history(
        &self,
        source: SessionId,
        point: ConversationEntryId,
        target: SessionId,
        thread: ThreadId,
    ) -> Result<RestoredSession> {
        self.with_database(|db| {
            let tx=db.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(super::super::forgetting::source_allowed(&tx,source.to_string().parse()?)?,"forgotten source history cannot be reused by branching");
            let (workspace,position):(String,usize)=tx.query_row("SELECT s.workspace,p.position FROM native_sessions s JOIN native_path p ON p.session=s.id WHERE s.id=?1 AND p.entry=?2",params![source.to_string(),point.to_string()],|row|Ok((row.get(0)?,read_usize(row,1)?))).optional()?.context("branch point is not on the active history path")?;
            let count=position.checked_add(1).context("branch size overflow")?;
            let mut checkpoint=branch_checkpoint(&tx,source,count)?;
            if count<=hydration::MAX_EXECUTION_ENTRIES { checkpoint=None; }
            let offset=checkpoint.as_ref().map_or(0,|item|item.source_entry_count);
            ensure!(count-offset<=hydration::MAX_EXECUTION_ENTRIES,"branch has no bounded continuation checkpoint before this point");
            let created=RecordEnvelope::new(target,SessionRecord::SessionCreated {thread_id:thread,workspace_root:workspace.clone().into()});
            let mut state=crate::session::reduce(std::slice::from_ref(&created))?;
            let lineage=NativeBranchLineage {source_session_id:source,source_entry_id:point,shared_entry_count:count};
            state.branch=Some(lineage.clone());
            tx.execute("INSERT INTO native_sessions VALUES(?1,?2,?3,NULL,0,0,0)",params![target.to_string(),thread.to_string(),workspace])?;
            let mut revision=0usize;
            let mut bytes=0usize;
            let mut write=|record:SessionRecord|->Result<()> {
                bytes=append(&tx,target,&RecordEnvelope::new(target,record),revision,bytes)?;
                revision+=1;
                Ok(())
            };
            write(created.record)?;
            write(SessionRecord::ConversationBranched {lineage})?;
            let mut proof=checkpoint.as_ref().map(|item|CompactionSourceProofBuilder::new(target,item.source_entry_count));
            let mut query=tx.prepare("SELECT p.position,p.entry,r.body FROM native_path p JOIN native_records r ON r.session=p.session AND r.sequence=p.sequence WHERE p.session=?1 AND p.position<=?2 ORDER BY p.position")?;
            let mut rows=query.query(params![source.to_string(),i64::try_from(position)?])?;
            let mut expected=0usize;
            let mut previous=None;
            while let Some(row)=rows.next()? {
                ensure!(read_usize(row,0)?==expected,"branch source positions are discontinuous");
                let body=row.get_ref(2)?.as_blob()?;
                ensure!(body.len()<=MAX_RECORD_BYTES,"branch source exceeds record bound");
                let envelope:RecordEnvelope=serde_json::from_slice(body)?;
                ensure!(envelope.session_id==source && envelope.version==SESSION_RECORD_VERSION,"branch source identity differs");
                let SessionRecord::ConversationEntryAppended {entry}=envelope.record else {anyhow::bail!("branch source is not an entry");};
                ensure!(entry.parent==previous && entry.id.to_string()==row.get::<_,String>(1)?,"branch source ancestry differs");
                if expected<=offset && let Some(proof)=&mut proof {proof.push(entry.id,&entry.message)?;}
                for artifact in entry.message.artifacts() {
                    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_subjects WHERE session=?1 AND kind='artifact' AND subject=?2)",params![target.to_string(),artifact.reference.id.to_string()],|row|row.get(0))?;
                    if !exists {write(SessionRecord::ArtifactRegistered {artifact:artifact.clone()})?;}
                    if expected>=offset {state.artifacts.insert(artifact.reference.id,artifact.clone());}
                }
                write(SessionRecord::ConversationEntryAppended {entry:entry.clone()})?;
                previous=Some(entry.id);
                if expected>=offset { state.entries.insert(entry.id,entry); }
                expected+=1;
            }
            ensure!(expected==count && previous==Some(point),"branch source range is incomplete");
            drop(rows); drop(query);
            write(SessionRecord::ThreadHeadMoved {thread_id:thread,head:Some(point)})?;
            state.head=Some(point);
            if let Some(mut checkpoint)=checkpoint {
                checkpoint.id=CompactionId::new();
                checkpoint.operation_id=OperationId::new();
                checkpoint.previous_checkpoint=None;
                ensure!(proof.context("branch prefix proof is absent")?.finish()?.matches(target,&checkpoint),"branch checkpoint differs from exact copied source");
                write(SessionRecord::ConversationCompacted {checkpoint:checkpoint.clone()})?;
                state.archived_prefix=Some(checkpoint.clone());
                state.compactions.push(checkpoint);
            }
            let encoded=hydration::encode(&state)?;
            let anchor:String=tx.query_row("SELECT digest FROM native_record_digests WHERE session=?1 AND sequence=?2",params![target.to_string(),i64::try_from(revision-1)?],|row|row.get(0))?;
            let digest=subjects::record_digest(&anchor,&encoded);
            tx.execute("INSERT INTO native_execution_checkpoints VALUES(?1,?2,?3,?4)",params![target.to_string(),i64::try_from(revision)?,digest,encoded])?;
            tx.commit()?;
            Ok(state)
        })
    }
}

fn branch_checkpoint(
    tx: &Transaction<'_>,
    source: SessionId,
    count: usize,
) -> Result<Option<CompactionCheckpoint>> {
    // Select by immutable prefix endpoints, not merely by recency: a checkpoint
    // on another branch or after the selected point cannot seed this branch.
    let mut query=tx.prepare("SELECT r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence JOIN native_path a ON a.session=s.session AND a.position=0 AND a.entry=json_extract(CAST(r.body AS TEXT),'$.data.checkpoint.source_start') JOIN native_path b ON b.session=s.session AND b.position=json_extract(CAST(r.body AS TEXT),'$.data.checkpoint.source_entry_count')-1 AND b.entry=json_extract(CAST(r.body AS TEXT),'$.data.checkpoint.source_end') JOIN native_path c ON c.session=s.session AND c.position=b.position+1 AND c.entry=json_extract(CAST(r.body AS TEXT),'$.data.checkpoint.retained_tail_start') WHERE s.session=?1 AND s.kind='compaction' AND c.position<?2 ORDER BY s.sequence DESC LIMIT 1")?;
    let mut rows = query.query(params![source.to_string(), i64::try_from(count)?])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let body = row.get_ref(0)?.as_blob()?;
    ensure!(
        body.len() <= MAX_RECORD_BYTES,
        "branch checkpoint exceeds record bound"
    );
    let record: RecordEnvelope = serde_json::from_slice(body)?;
    let SessionRecord::ConversationCompacted { checkpoint } = record.record else {
        anyhow::bail!("branch checkpoint index differs");
    };
    ensure!(
        record.session_id == source && checkpoint.source_entry_count < count,
        "branch checkpoint identity or range differs"
    );
    Ok(Some(checkpoint))
}
