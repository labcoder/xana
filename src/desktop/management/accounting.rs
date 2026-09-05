//! Bounded local accounting inspection; account/vendor observations stay separate.

use super::{DesktopControlPlane, DesktopError, control_error};

pub struct DesktopUsagePage {
    pub text: String,
    pub next_after: Option<u64>,
    pub budget: Vec<DesktopBudgetSetting>,
    pub restore_review_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopBudgetField {
    DailyRequests,
    RootRequests,
    ForegroundReserve,
    DailyTokens,
    RootTokens,
    BackgroundDailyTokens,
    BackgroundJobTokens,
}

pub struct DesktopBudgetSetting {
    pub field: DesktopBudgetField,
    pub label: &'static str,
    pub value: u64,
}

#[derive(Debug, Clone)]
pub struct DesktopBudgetEdit {
    pub field: DesktopBudgetField,
    pub value: u64,
}

impl DesktopControlPlane {
    pub fn update_local_budget(
        &self,
        edits: &[DesktopBudgetEdit],
        accept_restored_usage: bool,
    ) -> Result<(), DesktopError> {
        let store = crate::storage::ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| control_error("Durable budgets require protected storage"))?;
        apply_budget(&store, edits, accept_restored_usage)
    }
    pub fn local_usage_page(
        &self,
        root: Option<&str>,
        job: Option<&str>,
        after: Option<u64>,
    ) -> Result<DesktopUsagePage, DesktopError> {
        let store = crate::storage::ProtectedStore::configured(self.paths.data_dir())
            .map_err(control_error)?
            .ok_or_else(|| {
                control_error(
                    "Durable usage requires protected storage. No plaintext ledger was created.",
                )
            })?;
        usage_page(&store, root, job, after)
    }
}

fn apply_budget(
    store: &crate::storage::ProtectedStore,
    edits: &[DesktopBudgetEdit],
    accept_restored_usage: bool,
) -> Result<(), DesktopError> {
    if edits.len() > 7
        || edits
            .iter()
            .enumerate()
            .any(|(ix, edit)| edits[..ix].iter().any(|prior| prior.field == edit.field))
    {
        return Err(control_error(
            "Budget edits must name each field at most once",
        ));
    }
    store
        .update_usage_policy(|policy| {
            for edit in edits {
                use DesktopBudgetField::*;
                match edit.field {
                    DailyRequests => policy.daily_requests = edit.value,
                    RootRequests => policy.root_requests = edit.value,
                    ForegroundReserve => policy.foreground_request_reserve = edit.value,
                    DailyTokens => policy.daily_tokens = (edit.value != 0).then_some(edit.value),
                    RootTokens => policy.root_tokens = (edit.value != 0).then_some(edit.value),
                    BackgroundDailyTokens => policy.background_daily_tokens = edit.value,
                    BackgroundJobTokens => policy.background_job_tokens = edit.value,
                }
            }
        })
        .map_err(control_error)?;
    if accept_restored_usage {
        store
            .remove_document("usage/restore-review-required")
            .map_err(control_error)?;
    }
    Ok(())
}
fn usage_page(
    store: &crate::storage::ProtectedStore,
    root: Option<&str>,
    job: Option<&str>,
    after: Option<u64>,
) -> Result<DesktopUsagePage, DesktopError> {
    for value in [root, job].into_iter().flatten() {
        if value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(control_error("Invalid usage filter"));
        }
    }
    let rows = store.usage_page(root, job, after).map_err(control_error)?;
    let policy = store.usage_policy().map_err(control_error)?;
    let mut text = String::from(
        "Local admission estimates and reported receipts — not vendor quota or a billing ceiling. Null means unknown, not free. Managed cumulative counters are observations, not per-turn charges.\n\n",
    );
    text.push_str(&serde_json::to_string_pretty(&policy).map_err(control_error)?);
    text.push_str("\n\nExisting reservations survive policy changes. Zero removes only optional day/root token limits.\n\n");
    text.push_str(&serde_json::to_string_pretty(&rows).map_err(control_error)?);
    if text.len() > 2 * 1024 * 1024 {
        return Err(control_error(
            "Usage page exceeds the Desktop projection bound",
        ));
    }
    Ok(DesktopUsagePage {
        text,
        next_after: (rows.len() == 128).then(|| rows.last().expect("full page").sequence),
        budget: {
            let p = policy;
            use DesktopBudgetField::*;
            [
                (DailyRequests, "Requests per UTC day", p.daily_requests),
                (
                    RootRequests,
                    "Requests per root Conversation",
                    p.root_requests,
                ),
                (
                    ForegroundReserve,
                    "Daily requests reserved for foreground",
                    p.foreground_request_reserve,
                ),
                (
                    DailyTokens,
                    "Tokens per UTC day (0: unset)",
                    p.daily_tokens.unwrap_or(0),
                ),
                (
                    RootTokens,
                    "Tokens per root (0: unset)",
                    p.root_tokens.unwrap_or(0),
                ),
                (
                    BackgroundDailyTokens,
                    "Background tokens per UTC day",
                    p.background_daily_tokens,
                ),
                (
                    BackgroundJobTokens,
                    "Background tokens per job",
                    p.background_job_tokens,
                ),
            ]
            .into_iter()
            .map(|(field, label, value)| DesktopBudgetSetting {
                field,
                label,
                value,
            })
            .collect()
        },
        restore_review_required: store
            .document("usage/restore-review-required", 4096)
            .map_err(control_error)?
            .is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ProtectedStore, RecoveryIdentity, TestCustody};

    #[test]
    fn desktop_budget_edits_validate_and_acknowledge_only_usage_authority() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &RecoveryIdentity::generate(),
            &TestCustody::default(),
        )
        .unwrap();
        store
            .set_document("usage/restore-review-required", b"review", 4096)
            .unwrap();
        store
            .set_document("restore/review-required", b"memory remains disabled", 4096)
            .unwrap();
        assert!(
            usage_page(&store, None, None, None)
                .unwrap()
                .restore_review_required
        );
        let invalid = [DesktopBudgetEdit {
            field: DesktopBudgetField::DailyRequests,
            value: 0,
        }];
        assert!(apply_budget(&store, &invalid, true).is_err());
        assert!(
            usage_page(&store, None, None, None)
                .unwrap()
                .restore_review_required
        );
        let edit = DesktopBudgetEdit {
            field: DesktopBudgetField::DailyRequests,
            value: 500,
        };
        assert!(apply_budget(&store, &[edit.clone(), edit.clone()], false).is_err());
        apply_budget(&store, &[edit], true).unwrap();
        let page = usage_page(&store, None, None, None).unwrap();
        assert!(!page.restore_review_required);
        assert_eq!(
            page.budget
                .iter()
                .find(|field| field.field == DesktopBudgetField::DailyRequests)
                .unwrap()
                .value,
            500
        );
        assert!(page.next_after.is_none());
        assert!(
            store
                .document("restore/review-required", 4096)
                .unwrap()
                .is_some()
        );
    }
}
