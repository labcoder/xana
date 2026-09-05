//! Bounded terminal-only productivity state shared by plain and TUI clients.
//!
//! This module owns no runtime authority. Composer history is frontend-local,
//! workspace-scoped, secret-filtered, and replaceable. Path completion reuses
//! the tool layer's bounded, gitignore-aware discovery without granting file
//! access, and transcript search reads canonical retained history through the
//! workspace host.

use crate::{
    bounded_file,
    message::{ContentBlock, Message, Role},
    paths::XanaPaths,
    tool,
    workspace_host::{ConversationRef, WorkspaceHost},
    workspace_identity::WorkspaceIdentity,
};
use anyhow::{Context, Result, bail};
use rustyline::{
    Context as ReadlineContext, Helper,
    completion::{Completer, Pair},
    highlight::Highlighter,
    hint::Hinter,
    validate::Validator,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    ops::Range,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_FILE_BYTES: usize = MAX_HISTORY_BYTES + 64 * 1024;
const MAX_SEARCH_QUERY_BYTES: usize = 1024;
const MAX_SEARCH_MATCHES: usize = 100;
const MAX_SEARCH_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_SEARCH_EXCERPT_BYTES: usize = 512;
pub(crate) const MAX_FILE_COMPLETIONS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryLoad {
    pub(crate) entries: Vec<String>,
    pub(crate) warning: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ComposerHistoryStore {
    protected: Option<crate::storage::ProtectedStore>,
    path: PathBuf,
    workspace_id: String,
    entries: VecDeque<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HistoryDocument {
    version: u16,
    workspace_id: String,
    entries: Vec<String>,
}

impl ComposerHistoryStore {
    pub(crate) fn open(paths: &XanaPaths, workspace: &Path) -> Result<(Self, HistoryLoad)> {
        let protected = crate::storage::ProtectedStore::configured(paths.data_dir())?;
        let identity = WorkspaceIdentity::resolve(workspace)
            .with_context(|| format!("could not identify workspace {}", workspace.display()))?;
        let workspace_id = identity.collision_key().to_owned();
        let path = paths
            .data_dir()
            .join("frontend")
            .join("composer-history")
            .join(format!("{workspace_id}.json"));
        let input = if let Some(store) = &protected {
            store
                .document(
                    &format!("frontend/composer-history/{workspace_id}.json"),
                    MAX_HISTORY_FILE_BYTES,
                )?
                .map(|bytes| {
                    String::from_utf8(bytes).map_err(|error| bounded_file::BoundedReadError::Io {
                        path: path.clone(),
                        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                    })
                })
                .unwrap_or_else(|| {
                    Err(bounded_file::BoundedReadError::Io {
                        path: path.clone(),
                        source: std::io::Error::from(std::io::ErrorKind::NotFound),
                    })
                })
        } else {
            bounded_file::read_to_string(&path, MAX_HISTORY_FILE_BYTES)
        };
        let (entries, warning) = match input {
            Ok(input) => match decode_history(&input, &workspace_id) {
                Ok(entries) => (entries, None),
                Err(error) => (
                    VecDeque::new(),
                    Some(format!(
                        "composer history was ignored because {} is invalid: {error}",
                        path.display()
                    )),
                ),
            },
            Err(bounded_file::BoundedReadError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                (VecDeque::new(), None)
            }
            Err(error) => (
                VecDeque::new(),
                Some(format!(
                    "composer history was unavailable at {}: {error}",
                    path.display()
                )),
            ),
        };
        let load = HistoryLoad {
            entries: entries.iter().cloned().collect(),
            warning,
        };
        Ok((
            Self {
                protected,
                path,
                workspace_id,
                entries,
            },
            load,
        ))
    }

    /// Records one submitted draft and returns the exact safe entry retained.
    /// Secret-shaped input is replaced as a whole rather than partially masked.
    pub(crate) fn record(&mut self, value: &str) -> Result<Option<String>> {
        let Some(entry) = append_history_entry(&mut self.entries, value) else {
            return Ok(None);
        };
        self.persist()?;
        Ok(Some(entry))
    }

    fn persist(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("composer history path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "could not create composer history directory {}",
                parent.display()
            )
        })?;
        let encoded = serde_json::to_vec_pretty(&HistoryDocument {
            version: 1,
            workspace_id: self.workspace_id.clone(),
            entries: self.entries.iter().cloned().collect(),
        })?;
        if encoded.len() > MAX_HISTORY_FILE_BYTES {
            bail!("composer history exceeded its encoded storage bound");
        }
        if let Some(store) = &self.protected {
            return store.set_document(
                &format!("frontend/composer-history/{}.json", self.workspace_id),
                &encoded,
                MAX_HISTORY_FILE_BYTES,
            );
        }
        let mut file = atomic_write_file::AtomicWriteFile::open(&self.path)
            .with_context(|| format!("could not stage {}", self.path.display()))?;
        use std::io::Write as _;
        file.write_all(&encoded)?;
        file.commit()
            .with_context(|| format!("could not commit {}", self.path.display()))?;
        Ok(())
    }
}

fn decode_history(input: &str, workspace_id: &str) -> Result<VecDeque<String>> {
    let document: HistoryDocument = serde_json::from_str(input)?;
    if document.version != 1 || document.workspace_id != workspace_id {
        bail!("history identity or version does not match")
    }
    if document.entries.len() > MAX_HISTORY_ENTRIES {
        bail!("history entry count exceeds its bound")
    }
    let mut entries = VecDeque::new();
    for value in document.entries {
        let Some(safe) = safe_history_entry(&value) else {
            continue;
        };
        entries.push_back(safe);
    }
    trim_history(&mut entries);
    Ok(entries)
}

pub(crate) fn safe_history_entry(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if looks_secret_like(value) {
        return Some("[redacted secret-like composer entry]".to_owned());
    }
    Some(bound_utf8(value, crate::tui::MAX_COMPOSER_INPUT_BYTES))
}

pub(crate) fn append_history_entry(entries: &mut VecDeque<String>, value: &str) -> Option<String> {
    let entry = safe_history_entry(value)?;
    if entries.back() == Some(&entry) {
        return None;
    }
    entries.push_back(entry.clone());
    trim_history(entries);
    Some(entry)
}

fn looks_secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if [
        "authorization: bearer ",
        "api_key=",
        "api-key=",
        "apikey=",
        "access_token=",
        "refresh_token=",
        "password=",
        "secret=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return true;
    }
    value.split_whitespace().any(|word| {
        let candidate = word.trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']'
            )
        });
        candidate.len() >= 24
            && ["sk-", "sk_", "xoxb-", "ghp_", "github_pat_"]
                .iter()
                .any(|prefix| candidate.starts_with(prefix))
    })
}

fn trim_history(entries: &mut VecDeque<String>) {
    while entries.len() > MAX_HISTORY_ENTRIES
        || entries.iter().map(String::len).sum::<usize>() > MAX_HISTORY_BYTES
    {
        entries.pop_front();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AtFileQuery {
    pub(crate) replacement: Range<usize>,
    pub(crate) query: String,
}

pub(crate) fn at_file_query(line: &str, cursor: usize) -> Option<AtFileQuery> {
    if cursor > line.len() || !line.is_char_boundary(cursor) {
        return None;
    }
    let before = &line[..cursor];
    let start = before
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            character
                .is_whitespace()
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let token = &line[start..cursor];
    let query = token.strip_prefix('@')?;
    if query.contains(['\r', '\n']) || query.len() > 1024 {
        return None;
    }
    Some(AtFileQuery {
        replacement: start..cursor,
        query: query.to_owned(),
    })
}

pub(crate) fn complete_workspace_paths(
    workspace: &Path,
    query: &str,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<String>> {
    tool::complete_workspace_paths(
        workspace,
        query,
        limit.clamp(1, MAX_FILE_COMPLETIONS),
        cancellation,
    )
    .map_err(anyhow::Error::msg)
}

#[derive(Clone)]
pub(crate) struct WorkspaceCompleter {
    workspace: PathBuf,
}

impl WorkspaceCompleter {
    pub(crate) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

impl Completer for WorkspaceCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _context: &ReadlineContext<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let Some(query) = at_file_query(line, pos) else {
            return Ok((pos, Vec::new()));
        };
        let cancellation = CancellationToken::new();
        let choices = complete_workspace_paths(
            &self.workspace,
            &query.query,
            MAX_FILE_COMPLETIONS,
            &cancellation,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|path| Pair {
            display: path.clone(),
            replacement: format!("@{path}"),
        })
        .collect();
        Ok((query.replacement.start, choices))
    }
}

impl Hinter for WorkspaceCompleter {
    type Hint = String;
}

impl Highlighter for WorkspaceCompleter {}
impl Validator for WorkspaceCompleter {}
impl Helper for WorkspaceCompleter {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TranscriptSearchReport {
    pub(crate) version: u16,
    pub(crate) conversation: String,
    pub(crate) query: String,
    pub(crate) matches: Vec<TranscriptMatch>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TranscriptMatch {
    pub(crate) index: usize,
    pub(crate) role: &'static str,
    pub(crate) excerpt: String,
}

pub(crate) fn search_transcript(
    host: &WorkspaceHost,
    conversation: &ConversationRef,
    query: &str,
    limit: usize,
) -> Result<TranscriptSearchReport> {
    let query = query.trim();
    if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
        bail!("transcript search query must be 1..={MAX_SEARCH_QUERY_BYTES} bytes")
    }
    if !(1..=MAX_SEARCH_MATCHES).contains(&limit) {
        bail!("transcript search limit must be in 1..={MAX_SEARCH_MATCHES}")
    }
    let needle = query.to_lowercase();
    let mut before = None;
    let mut matches = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated = false;
    loop {
        let Some(page) = host.conversation_history_page(conversation, before, 128)? else {
            bail!("managed transcript search is unavailable because its runtime owns history")
        };
        for (offset, message) in page.messages.iter().enumerate().rev() {
            let text = searchable_message_text(message);
            if !text.to_lowercase().contains(&needle) {
                continue;
            }
            let excerpt = matching_excerpt(&text, &needle);
            let estimated = excerpt.len().saturating_add(96);
            if matches.len() == limit
                || output_bytes.saturating_add(estimated) > MAX_SEARCH_OUTPUT_BYTES
            {
                truncated = true;
                break;
            }
            output_bytes = output_bytes.saturating_add(estimated);
            matches.push(TranscriptMatch {
                index: page.start.saturating_add(offset),
                role: role_label(message.role),
                excerpt,
            });
        }
        if truncated || !page.has_older {
            break;
        }
        before = Some(page.start);
    }
    matches.sort_by_key(|entry| entry.index);
    Ok(TranscriptSearchReport {
        version: 1,
        conversation: conversation.to_string(),
        query: query.to_owned(),
        matches,
        truncated,
    })
}

fn searchable_message_text(message: &Message) -> String {
    let mut output = String::new();
    for block in &message.content {
        let text = match block {
            ContentBlock::Text(text) => text.as_str(),
            ContentBlock::ToolResult(result) => result.output.as_str(),
            ContentBlock::Image(_) | ContentBlock::ToolCall(_) => continue,
        };
        if !output.is_empty() {
            output.push('\n');
        }
        let remaining = MAX_SEARCH_OUTPUT_BYTES.saturating_sub(output.len());
        output.push_str(&bound_utf8(text, remaining));
        if output.len() == MAX_SEARCH_OUTPUT_BYTES {
            break;
        }
    }
    output
}

fn matching_excerpt(text: &str, needle_lower: &str) -> String {
    let lower = text.to_lowercase();
    let start = lower.find(needle_lower).unwrap_or(0);
    let context_start = floor_char_boundary(text, start.saturating_sub(160));
    let context_end = floor_char_boundary(
        text,
        start
            .saturating_add(needle_lower.len())
            .saturating_add(280)
            .min(text.len()),
    );
    let mut excerpt = text[context_start..context_end]
        .replace(['\r', '\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    excerpt = bound_utf8(&excerpt, MAX_SEARCH_EXCERPT_BYTES);
    if context_start > 0 {
        excerpt.insert(0, '…');
    }
    if context_end < text.len() {
        excerpt.push('…');
    }
    excerpt
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn bound_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    value[..floor_char_boundary(value, maximum)].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::SessionId, session::DurableSession};
    use tempfile::tempdir;

    #[test]
    fn history_is_workspace_scoped_bounded_and_secret_filtered() {
        let directory = tempdir().unwrap();
        let home =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let (mut store, load) = ComposerHistoryStore::open(&home, &workspace).unwrap();
        assert!(load.entries.is_empty());
        assert_eq!(store.record("hello").unwrap().as_deref(), Some("hello"));
        assert_eq!(
            store
                .record("api_key=do-not-store-this")
                .unwrap()
                .as_deref(),
            Some("[redacted secret-like composer entry]")
        );
        for index in 0..MAX_HISTORY_ENTRIES + 10 {
            store.record(&format!("entry-{index}")).unwrap();
        }
        let (_, reloaded) = ComposerHistoryStore::open(&home, &workspace).unwrap();
        assert_eq!(reloaded.entries.len(), MAX_HISTORY_ENTRIES);
        assert!(
            !reloaded
                .entries
                .iter()
                .any(|entry| entry.contains("do-not-store"))
        );
    }

    #[test]
    fn at_file_query_tracks_only_the_token_at_the_cursor() {
        let line = "please inspect @src/main";
        let query = at_file_query(line, line.len()).unwrap();
        assert_eq!(&line[query.replacement.clone()], "@src/main");
        assert_eq!(query.query, "src/main");
        assert!(at_file_query("plain text", 10).is_none());
    }

    #[test]
    fn completion_is_fuzzy_sorted_gitignore_aware_and_cancelable() {
        let workspace = tempdir().unwrap();
        fs::create_dir(workspace.path().join("src")).unwrap();
        fs::write(workspace.path().join(".gitignore"), "src/ignored.rs\n").unwrap();
        fs::write(workspace.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(workspace.path().join("src/ignored.rs"), "ignored").unwrap();
        let choices =
            complete_workspace_paths(workspace.path(), "smr", 10, &CancellationToken::new())
                .unwrap();
        assert_eq!(choices, ["src/main.rs"]);

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            complete_workspace_paths(workspace.path(), "", 10, &cancelled)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn transcript_search_is_bounded_and_uses_canonical_history() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let session_id = SessionId::new();
        let mut session =
            DurableSession::create_with_id(directory.path(), workspace.clone(), session_id)
                .unwrap();
        session
            .append_message(Message::text(Role::User, "the bounded needle is here"))
            .unwrap();
        session
            .append_message(Message::text(Role::Assistant, "needle confirmed"))
            .unwrap();
        drop(session);
        let host = WorkspaceHost::open(directory.path(), &workspace).unwrap();

        let report =
            search_transcript(&host, &ConversationRef::Native { session_id }, "needle", 20)
                .unwrap();
        assert_eq!(report.matches.len(), 2);
        assert_eq!(report.matches[0].role, "user");
        assert!(!report.truncated);

        let limited =
            search_transcript(&host, &ConversationRef::Native { session_id }, "needle", 1).unwrap();
        assert_eq!(limited.matches.len(), 1);
        assert!(limited.truncated);
    }
}
