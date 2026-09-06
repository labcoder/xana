//! Admission accounting is historical, not part of the evictable execution view.
use super::*;
use crate::orchestration::ReservationRequest;

impl ProtectedStore {
    pub(crate) fn history_orchestration_reservations(
        &self,
        id: SessionId,
    ) -> Result<Vec<ReservationRequest>> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let expected = tx.query_row(
                "SELECT COUNT(*) FROM (SELECT DISTINCT subject FROM native_subjects WHERE session=?1 AND kind='child' LIMIT 4097)",
                [id.to_string()],
                |row| read_usize(row, 0),
            )?;
            ensure!(expected <= 4096, "admission history exceeds its accounting bound");
            let mut query = tx.prepare(
                "SELECT r.body,d.digest,p.digest FROM native_records r JOIN \
                 (SELECT DISTINCT sequence FROM native_subjects WHERE session=?1 \
                 AND kind='child_operation' ORDER BY sequence LIMIT 4097) admissions \
                 ON r.sequence=admissions.sequence \
                 LEFT JOIN native_record_digests d ON d.session=r.session AND d.sequence=r.sequence \
                 LEFT JOIN native_record_digests p ON p.session=r.session AND p.sequence=r.sequence-1 \
                 WHERE r.session=?1 ORDER BY r.sequence",
            )?;
            let mut rows = query.query([id.to_string()])?;
            let mut reservations = Vec::new();
            let mut bytes = 0usize;
            let mut seen = std::collections::HashSet::new();
            while let Some(row) = rows.next()? {
                let body = row.get_ref(0)?.as_blob()?;
                bytes = bytes
                    .checked_add(body.len())
                    .context("admission history size overflow")?;
                ensure!(
                    body.len() <= MAX_RECORD_BYTES && bytes <= 16 * 1024 * 1024,
                    "admission history exceeds its bounded accounting inspection"
                );
                let digest: String = row.get(1)?;
                let previous: String = row.get(2)?;
                ensure!(subjects::record_digest(&previous, body) == digest,
                    "admission history differs from its immutable digest");
                let record: RecordEnvelope = serde_json::from_slice(body)?;
                ensure!(
                    record.session_id == id && record.version == SESSION_RECORD_VERSION,
                    "admission history identity differs"
                );
                let handles = match record.record {
                    SessionRecord::ChildAdmitted { handle } => vec![handle],
                    SessionRecord::ChildrenBatchAdmitted { handles } => handles,
                    _ => anyhow::bail!("admission index differs from its record"),
                };
                for handle in handles {
                    ensure!(
                        reservations.len() < 4096
                            && seen.insert(handle.admission.attribution.agent_id)
                            && handle.admission.attribution.parent_agent_id
                                == crate::identity::AgentId::for_session(id),
                        "admission history exceeds its bound or repeats a child identity"
                    );
                    reservations.push(ReservationRequest::from(&handle.admission));
                }
            }
            ensure!(reservations.len() == expected, "admission history index is incomplete");
            Ok(reservations)
        })
    }
}

/// Fault injection stays behind the storage owner; production has no raw SQL hook.
#[cfg(test)]
impl ProtectedStore {
    pub(crate) fn corrupt_admission_fixture(
        &self,
        id: SessionId,
        fault: super::AdmissionFault,
    ) -> Result<()> {
        self.with_database(|db| {
            match fault {
                super::AdmissionFault::WrongIndex => {
                    db.connection.execute("UPDATE native_subjects SET sequence=1 WHERE session=?1 AND kind='child_operation'", [id.to_string()])?;
                }
                super::AdmissionFault::MissingIndex => {
                    db.connection.execute("DELETE FROM native_subjects WHERE session=?1 AND kind='child_operation'", [id.to_string()])?;
                }
                super::AdmissionFault::OversizeRecord => {
                    db.connection.execute(
                        "UPDATE native_records SET body=zeroblob(?2) WHERE session=?1 AND sequence IN \
                         (SELECT sequence FROM native_subjects WHERE session=?1 AND kind='child_operation')",
                        params![id.to_string(), (MAX_RECORD_BYTES + 1) as i64],
                    )?;
                }
                super::AdmissionFault::ChangedCharge => {
                    db.connection.execute(
                        "UPDATE native_records SET body=CAST(json_set(CAST(body AS TEXT), '$.data.handle.admission.max_tool_rounds', 0) AS BLOB) \
                         WHERE session=?1 AND sequence IN (SELECT sequence FROM native_subjects WHERE session=?1 AND kind='child_operation')",
                        [id.to_string()],
                    )?;
                }
            }
            Ok(())
        })
    }
}
