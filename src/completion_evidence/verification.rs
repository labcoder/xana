//! One local immutable-artifact verification pass, never effect replay.

use super::*;
use crate::artifact::ArtifactStore;
use tokio_util::sync::CancellationToken;

impl CompletionEvidence {
    /// The caller must durably append this reservation before any verification
    /// I/O. A reopened reservation is unresolved, not permission to run it again.
    pub(crate) fn reserve_verifier(&mut self, authorized: bool, cancelled: bool) -> Result<bool> {
        self.validate()?;
        ensure!(
            self.revision == 2 && self.verification == VerificationState::NotNeeded,
            "completion verifier has already been considered"
        );
        if self.artifacts.is_empty() {
            self.evaluate();
            return Ok(false);
        }
        let bytes = self.artifacts.iter().try_fold(0_u64, |total, fact| {
            total.checked_add(fact.artifact.byte_len)
        });
        if cancelled {
            self.verification = VerificationState::Cancelled;
        } else if !authorized
            || self.budget.exhausted
            || bytes.is_none_or(|bytes| bytes > MAX_VERIFY_BYTES)
        {
            self.verification = VerificationState::Unavailable;
        } else {
            self.budget.verification_bytes_reserved = bytes.unwrap_or(0);
            self.verification = VerificationState::Reserved;
        }
        self.evaluate();
        Ok(self.verification == VerificationState::Reserved)
    }
}

/// Runs on a caller-owned blocking lane. Bytes are streamed by ArtifactStore;
/// only references and verified digests survive. Never restart a reserved pass.
pub(crate) fn verify_artifacts(
    mut evidence: CompletionEvidence,
    store: &ArtifactStore,
    cancelled: &CancellationToken,
) -> Result<CompletionEvidence> {
    evidence.validate()?;
    ensure!(
        evidence.revision == 2 && evidence.verification == VerificationState::Reserved,
        "completion verifier requires its durable one-shot reservation"
    );
    let mut failed = false;
    for fact in &mut evidence.artifacts {
        if cancelled.is_cancelled() {
            break;
        }
        // This verifies opened regular-file identity and the complete digest,
        // while retaining zero body bytes in the completion projection.
        fact.verified = store
            .read_verified_range(&fact.artifact, 0, 0, MAX_VERIFY_BYTES as usize)
            .is_ok();
        failed |= !fact.verified;
    }
    evidence.revision = 3;
    evidence.verification = if cancelled.is_cancelled() {
        VerificationState::Cancelled
    } else if failed {
        VerificationState::Failed
    } else {
        VerificationState::Passed
    };
    evidence.evaluate();
    evidence.validate()?;
    Ok(evidence)
}
