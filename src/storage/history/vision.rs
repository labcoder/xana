//! Exact bounded vision receipts; no all-history hydration or replay authority.

use super::*;

#[cfg(test)]
mod tests;

impl ProtectedStore {
    pub(crate) fn history_vision_receipt(
        &self,
        session: SessionId,
        operation: crate::identity::OperationId,
    ) -> Result<Option<crate::vision::receipt::VisionReceipt>> {
        self.with_database(|db| {
            let tx = db.connection.transaction()?;
            let mut query = tx.prepare("SELECT r.sequence,r.body FROM native_subjects s JOIN native_records r ON r.session=s.session AND r.sequence=s.sequence WHERE s.session=?1 AND s.kind='vision' AND s.subject=?2 ORDER BY s.sequence LIMIT 3")?;
            let mut rows = query.query(params![session.to_string(), operation.to_string()])?;
            let mut latest = None;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                count += 1;
                ensure!(count <= 2, "vision receipt history exceeds its bound");
                let bytes = row.get_ref(1)?.as_blob()?;
                ensure!(bytes.len() <= 32 * 1024, "vision receipt exceeds its record bound");
                let envelope: RecordEnvelope = serde_json::from_slice(bytes)?;
                ensure!(envelope.session_id == session && envelope.version == SESSION_RECORD_VERSION, "vision receipt belongs to another Conversation");
                constraints::validate_registration_before(&tx, session, &envelope.record, Some(read_usize(row, 0)?))?;
                let SessionRecord::VisionReceiptRecorded { receipt } = envelope.record else { anyhow::bail!("vision receipt index differs"); };
                ensure!(receipt.operation() == Some(operation), "vision operation identity differs");
                latest = Some(receipt);
            }
            Ok(latest)
        })
    }
}
