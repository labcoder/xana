//! Incremental personal learning from owner-authored input only. Providers
//! propose data; deterministic eligibility, provenance and commit authority stay local.
mod processing;
#[cfg(test)]
mod tests;
use super::*;
use crate::provider::ConversationalProvider;
use anyhow::Context;
use std::sync::Arc;

pub(crate) const QUEUE_LIMIT: usize = 1000;
pub(crate) const BATCH_LIMIT: usize = 8;
pub(crate) const SOURCE_BYTES: usize = 8192;
pub(crate) const DISCLOSURE: &str = "Automatic personal learning is enabled unless scope controls opt out, and requires an unlocked protected home and an explicitly authorized native helper. Only eligible user statements enter bounded processing; ambiguous suggestions stay inactive and sensitive suggestions are not copied into memory without explicit owner consent. Use `memory controls --scope user --learn off` to opt out independently of memory use. No helper route means visible pending learning, not hidden provider calls.";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LearningSource {
    pub(crate) id: Uuid,
    pub(crate) context: MemoryContext,
    pub(crate) generation: u64,
    pub(crate) text: String,
    pub(crate) hash: String,
    pub(crate) accepted_at: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LearningRoute {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) digest: String,
}
pub(crate) struct LearningWorker {
    pub(crate) store: ProtectedStore,
    pub(crate) route: LearningRoute,
    pub(crate) provider: Arc<dyn ConversationalProvider>,
    /// Composition owns live configuration; the domain never loads process state.
    pub(crate) validate_route: Arc<LearningRouteValidator>,
}

pub(crate) type LearningRouteValidator = dyn Fn(&LearningRoute) -> Result<()> + Send + Sync;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Suggestion {
    pub(crate) source: Uuid,
    pub(crate) quote: String,
    pub(crate) claim: MemoryClaim,
    pub(crate) sensitive: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct LearningStatus {
    pub(crate) pending: u64,
    pub(crate) candidates: u64,
    pub(crate) excluded_after_change: u64,
    pub(crate) last_retirement: Option<LearningRetirement>,
    pub(crate) route: Option<LearningRoute>,
    pub(crate) last_receipt: Option<serde_json::Value>,
    pub(crate) disclosure: &'static str,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LearningRetirement {
    pub(crate) at_unix_seconds: u64,
    pub(crate) sources: u64,
    pub(crate) reason: String,
}

impl MemoryOwner {
    pub(crate) fn enqueue_user_statement(&self, id: Uuid, text: &str) -> Result<bool> {
        ensure!(!id.is_nil(), "learning source identity must not be nil");
        if text.len() > SOURCE_BYTES
            || text.trim().is_empty()
            || super::parse_natural(text).is_some()
        {
            return Ok(false);
        }
        let Some(conversation) = self.context.conversation else {
            return Ok(false);
        };
        let eligibility = self.eligible()?;
        if !eligibility.learning_enabled || !self.store.source_eligible(conversation)? {
            return Ok(false);
        }
        let source = LearningSource {
            id,
            context: self.context.clone(),
            generation: self.store.privacy_generation()?,
            text: text.into(),
            hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            accepted_at: super::now()?,
        };
        self.store.enqueue_learning(&source)
    }
}

/// A deliberately small transparent allowlist for ordinary durable preferences.
/// Everything else stays staged; helper confidence is never consent.
pub(crate) fn auto_eligible(source: &LearningSource, suggestion: &Suggestion) -> bool {
    if suggestion.sensitive
        || suggestion.claim != MemoryClaim::Stated
        || suggestion.quote.len() > 512
        || suggestion.quote.contains(['\n', '\r', '"', '`'])
    {
        return false;
    }
    let text = suggestion.quote.trim().to_lowercase();
    let ordinary = [
        "i prefer concise responses",
        "i prefer detailed responses",
        "i prefer examples",
        "i prefer metric units",
        "i prefer dark mode",
        "i prefer light mode",
        "i use rust",
        "i use python",
        "i use typescript",
    ];
    ordinary.contains(&text.trim_end_matches(['.', '!']))
        && source.text.trim().eq(suggestion.quote.trim())
}

pub(crate) fn record_for(source: &LearningSource, suggestion: &Suggestion) -> Result<MemoryRecord> {
    ensure!(
        source.id == suggestion.source && source.text.contains(&suggestion.quote),
        "learning suggestion has no exact owner source quote"
    );
    super::validate_statement(&suggestion.quote)?;
    let active = auto_eligible(source, suggestion);
    // Automatic statements remain narrow; owner controls explicitly promote scope.
    let scope = MemoryScope::Conversation(
        source
            .context
            .conversation
            .context("learning source lacks Conversation")?,
    );
    let origin = MemoryProvenance {
        owner_request: source.id,
        conversation: source.context.conversation,
        at_unix_seconds: source.accepted_at,
    };
    let record = MemoryRecord {
        version: 1,
        id: Uuid::new_v4(),
        revision: 1,
        scope,
        statement: suggestion.quote.clone(),
        claim: suggestion.claim,
        state: if active {
            MemoryState::Active
        } else {
            MemoryState::Candidate
        },
        created: origin.clone(),
        changed: origin,
        valid_until_unix_seconds: None,
    };
    record.validate()?;
    Ok(record)
}
