use super::*;
use crate::storage::{ProtectedStore, RecoveryIdentity, TestCustody};
use std::sync::{Arc, Barrier};

fn home() -> (tempfile::TempDir, ProtectedStore, RecoveryIdentity) {
    let dir = tempfile::tempdir().unwrap();
    let key = RecoveryIdentity::generate();
    let store = ProtectedStore::initialize(&dir.path().join("data"), &key, &TestCustody::default())
        .unwrap();
    (dir, store, key)
}

fn request(root: &str, id: &str) -> Admission {
    Admission {
        facts: DispatchFacts::default(),
        id: id.into(),
        operation: "operation".into(),
        root: root.into(),
        job: "job".into(),
        route: "native/ollama/model".into(),
        class: WorkClass::Foreground,
        reserved_tokens: 100,
    }
}

#[test]
fn remaining_allowance_uses_every_enforced_partition_without_hydrating_history() {
    for partition in [
        "day_requests",
        "root_requests",
        "headroom",
        "day_tokens",
        "root_tokens",
        "background_tokens",
        "job_tokens",
    ] {
        let (_dir, store, _) = home();
        let mut policy = BudgetPolicy::default();
        let mut admission = request("root", "request");
        match partition {
            "day_requests" => {
                policy.daily_requests = 1;
                policy.foreground_request_reserve = 0;
            }
            "root_requests" => policy.root_requests = 1,
            "headroom" => {
                policy.daily_requests = 2;
                policy.foreground_request_reserve = 1;
                admission.class = WorkClass::Background;
            }
            "day_tokens" => policy.daily_tokens = Some(100),
            "root_tokens" => policy.root_tokens = Some(100),
            "background_tokens" => {
                policy.background_daily_tokens = 100;
                admission.class = WorkClass::Background;
            }
            "job_tokens" => {
                policy.background_job_tokens = 100;
                admission.class = WorkClass::Background;
            }
            _ => unreachable!(),
        }
        store.set_usage_policy(&policy).unwrap();
        store.reserve_usage(&admission, 100).unwrap();
        let remaining = store
            .usage_remaining("root", "job", admission.class, 50)
            .unwrap();
        if partition.ends_with("tokens") {
            assert_eq!(remaining.tokens, Some(0), "{partition}");
        } else {
            assert_eq!(remaining.requests, 0, "{partition}");
        }
    }
    let (_dir, store, _) = home();
    let remaining = store
        .usage_remaining("root", "job", WorkClass::Foreground, 100)
        .unwrap();
    assert_eq!(remaining.tokens, None);
    assert_eq!(remaining.requests, BudgetPolicy::default().root_requests);
    store
        .set_usage_policy(&BudgetPolicy {
            root_tokens: Some(200),
            ..Default::default()
        })
        .unwrap();
    store
        .reserve_usage(&request("root", "observed"), 100)
        .unwrap();
    store
        .settle_usage(
            "observed",
            &Receipt {
                cumulative: None,
                total_tokens: Some(40),
                reported_cost_microunits: Some(9),
                outcome: Outcome::Completed,
            },
        )
        .unwrap();
    assert_eq!(
        store
            .usage_remaining("root", "job", WorkClass::Foreground, 100)
            .unwrap()
            .tokens,
        Some(160)
    );
}

#[test]
fn concurrent_owners_cannot_dispatch_past_a_shared_cap() {
    let (dir, store, key) = home();
    store
        .set_usage_policy(&BudgetPolicy {
            daily_requests: 1,
            foreground_request_reserve: 0,
            ..Default::default()
        })
        .unwrap();
    let other = ProtectedStore::recover(&dir.path().join("data"), &key).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [store.clone(), other]
        .into_iter()
        .enumerate()
        .map(|(i, store)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .reserve_usage(&request("root", &format!("r{i}")), 100)
                    .is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|admitted| *admitted)
            .count(),
        1
    );
    assert_eq!(store.usage_page(None, None, None).unwrap().len(), 1);
}

#[test]
fn concurrent_policy_field_edits_preserve_each_owners_intent() {
    let (dir, store, key) = home();
    let other = ProtectedStore::recover(&dir.path().join("data"), &key).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let gate = barrier.clone();
    let worker = std::thread::spawn(move || {
        gate.wait();
        other
            .update_usage_policy(|p| p.root_tokens = Some(9000))
            .unwrap();
    });
    barrier.wait();
    store
        .update_usage_policy(|p| p.daily_requests = 50)
        .unwrap();
    worker.join().unwrap();
    let result = store.usage_policy().unwrap();
    assert_eq!(result.daily_requests, 50);
    assert_eq!(result.root_tokens, Some(9000));
}

#[test]
fn unknown_usage_survives_recovery_and_duplicate_settlement_is_idempotent() {
    let (dir, store, key) = home();
    store
        .set_usage_policy(&BudgetPolicy {
            root_tokens: Some(100),
            ..Default::default()
        })
        .unwrap();
    store.reserve_usage(&request("root", "r1"), 100).unwrap();
    drop(store);
    let store = ProtectedStore::recover(&dir.path().join("data"), &key).unwrap();
    assert!(store.reserve_usage(&request("root", "r1"), 100).is_err());
    assert!(store.reserve_usage(&request("root", "r2"), 100).is_err());
    let receipt = Receipt {
        cumulative: None,
        total_tokens: None,
        reported_cost_microunits: None,
        outcome: Outcome::Interrupted,
    };
    store.settle_usage("r1", &receipt).unwrap();
    store.settle_usage("r1", &receipt).unwrap();
    assert!(
        store
            .settle_usage(
                "r1",
                &Receipt {
                    total_tokens: Some(0),
                    ..receipt.clone()
                }
            )
            .is_err()
    );
    assert!(store.reserve_usage(&request("root", "r2"), 101).is_err());
    let record = store.usage_page(None, None, None).unwrap().remove(0);
    assert_eq!(record.charged_tokens, 100);
    assert_eq!(record.receipt, Some(receipt));
}

#[test]
fn children_routes_and_days_do_not_reset_job_allowances() {
    let (_dir, store, _key) = home();
    let mut first = request("root", "r1");
    first.class = WorkClass::Background;
    first.reserved_tokens = 8192;
    store.reserve_usage(&first, 100).unwrap();
    store
        .settle_usage(
            "r1",
            &Receipt {
                cumulative: None,
                total_tokens: Some(8192),
                reported_cost_microunits: None,
                outcome: Outcome::Completed,
            },
        )
        .unwrap();
    let mut child = request("root", "r2");
    child.class = WorkClass::Background;
    child.route = "child/codex/other-model".into();
    assert!(store.reserve_usage(&child, 101).is_err());
    child.class = WorkClass::Foreground;
    store.reserve_usage(&child, 101).unwrap();
    let records = store.usage_page(Some("root"), Some("job"), None).unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|row| {
        row.receipt
            .as_ref()
            .and_then(|receipt| receipt.reported_cost_microunits)
            .is_none()
    }));
}

#[test]
fn background_cannot_consume_foreground_headroom_and_clock_rollback_does_not_reset_day() {
    let (_dir, store, _key) = home();
    store
        .set_usage_policy(&BudgetPolicy {
            daily_requests: 2,
            foreground_request_reserve: 1,
            ..Default::default()
        })
        .unwrap();
    let mut r = request("root", "r1");
    r.class = WorkClass::Background;
    store.reserve_usage(&r, 100).unwrap();
    r.id = "r2".into();
    assert!(store.reserve_usage(&r, 99).is_err());
    r.class = WorkClass::Foreground;
    store.reserve_usage(&r, 99).unwrap();
    r.id = "r3".into();
    assert!(store.reserve_usage(&r, 99).is_err());
}

#[test]
fn restoring_usage_requires_explicit_review_without_enabling_background_authority() {
    let (_dir, store, _key) = home();
    store
        .set_document("usage/restore-review-required", b"review", 4096)
        .unwrap();
    store
        .set_document("restore/review-required", b"review", 4096)
        .unwrap();
    let mut admission = request("root", "r");
    assert!(store.reserve_usage(&admission, 100).is_err());
    store
        .remove_document("usage/restore-review-required")
        .unwrap();
    store.reserve_usage(&admission, 100).unwrap();
    admission.id = "background".into();
    admission.class = WorkClass::Background;
    assert!(store.reserve_usage(&admission, 100).is_err());
}

#[test]
fn cumulative_vendor_reports_are_not_charged_as_per_request_totals() {
    let (_dir, store, _key) = home();
    store.reserve_usage(&request("root", "r"), 100).unwrap();
    store
        .settle_usage(
            "r",
            &Receipt {
                total_tokens: None,
                reported_cost_microunits: None,
                cumulative: Some(CumulativeUsage {
                    counter: "thread".into(),
                    total_tokens: 1_000_000,
                }),
                outcome: Outcome::Completed,
            },
        )
        .unwrap();
    let record = store.usage_page(None, None, None).unwrap().remove(0);
    assert_eq!(record.charged_tokens, 100);
    assert_eq!(
        record.receipt.unwrap().cumulative.unwrap().total_tokens,
        1_000_000
    );
}

#[test]
fn actual_receipts_adjust_all_shared_counters_once() {
    let (_dir, store, _key) = home();
    store
        .set_usage_policy(&BudgetPolicy {
            daily_tokens: Some(150),
            root_tokens: Some(150),
            ..Default::default()
        })
        .unwrap();
    store.reserve_usage(&request("root", "r1"), 100).unwrap();
    let receipt = Receipt {
        total_tokens: Some(40),
        reported_cost_microunits: Some(7),
        cumulative: None,
        outcome: Outcome::Completed,
    };
    store.settle_usage("r1", &receipt).unwrap();
    store.settle_usage("r1", &receipt).unwrap();
    store.reserve_usage(&request("root", "r2"), 100).unwrap();
    assert!(store.reserve_usage(&request("root", "r3"), 100).is_err());
    assert_eq!(
        store
            .usage_page(None, None, None)
            .unwrap()
            .iter()
            .map(|row| row.charged_tokens)
            .sum::<u64>(),
        140
    );
}
