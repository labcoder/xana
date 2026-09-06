use super::*;
use crate::{
    identity::PrincipalId,
    paths::XanaPaths,
    storage::{ProtectedStore, TestCustody},
};

fn fixture() -> (tempfile::TempDir, BrowserOwner) {
    let root = tempfile::tempdir().unwrap();
    let paths = XanaPaths::resolve(Some(root.path().as_os_str().to_owned())).unwrap();
    let store = ProtectedStore::initialize(
        paths.data_dir(),
        &crate::storage::RecoveryIdentity::generate(),
        &TestCustody::default(),
    )
    .unwrap();
    let owner = BrowserOwner::with_executable(paths, store, PrincipalId::new(), None, true);
    (root, owner)
}

#[tokio::test]
async fn unavailable_owner_is_lazy_and_lifecycle_receipts_are_durable_without_replay() {
    let (_root, owner) = fixture();
    assert!(!owner.snapshot().available);
    assert!(!owner.paths().cache_dir().exists());
    assert!(matches!(
        owner.plan(BrowserRequest::Launch {
            origins: vec!["https://example.com".into()]
        }),
        Err(BrowserError::Unavailable)
    ));
    assert!(matches!(
        owner.plan(BrowserRequest::Observe {}),
        Err(BrowserError::NoSession)
    ));
    let close = owner.plan(BrowserRequest::Close {}).unwrap();
    let receipt = owner
        .execute(close.clone(), crate::identity::OperationId::new())
        .await
        .unwrap();
    assert!(receipt.acknowledged);
    assert_eq!(receipt.snapshot.state, "closed");
    assert!(matches!(
        owner
            .execute(close, crate::identity::OperationId::new())
            .await,
        Err(BrowserError::Stale)
    ));
    let retained = owner.receipts(64).await.unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id, receipt.id);
    assert!(!owner.paths().cache_dir().exists());
}

#[tokio::test]
async fn failed_cleanup_cannot_turn_into_acknowledged_close_or_new_launch() {
    let (_root, owner) = fixture();
    let task = uuid::Uuid::new_v4();
    owner.simulate_cleanup_failure(task);
    assert_eq!(owner.shutdown().await, Err(BrowserError::Process));
    let receipt = owner
        .execute(
            owner.plan(BrowserRequest::Close {}).unwrap(),
            crate::identity::OperationId::new(),
        )
        .await
        .unwrap();
    assert!(!receipt.acknowledged);
    assert_eq!(receipt.snapshot.state, "cleanup_failed");
    assert_eq!(receipt.snapshot.task, Some(task));
    assert!(matches!(
        owner.plan(BrowserRequest::Launch {
            origins: vec!["https://example.com".into()]
        }),
        Err(BrowserError::Process)
    ));
    assert!(!owner.receipts(64).await.unwrap()[0].acknowledged);
}

#[tokio::test]
async fn receipt_reopen_is_principal_scoped_and_no_page_text_leaks_into_status() {
    let (_root, owner) = fixture();
    owner
        .execute(
            owner.plan(BrowserRequest::Close {}).unwrap(),
            crate::identity::OperationId::new(),
        )
        .await
        .unwrap();
    let same = owner.same_principal_fixture();
    assert_eq!(same.receipts(64).await.unwrap().len(), 1);
    let other = owner.other_principal_fixture();
    assert!(other.receipts(64).await.unwrap().is_empty());
    assert_eq!(other.snapshot().task, None);
    assert!(
        !serde_json::to_string(&owner.snapshot())
            .unwrap()
            .contains("observation")
    );
}

#[tokio::test]
async fn locked_receipt_storage_is_visible_and_never_acknowledges_an_action() {
    let (_root, owner) = fixture();
    let plan = owner.plan(BrowserRequest::Close {}).unwrap();
    owner.lock_fixture();
    assert!(matches!(
        owner
            .execute(plan, crate::identity::OperationId::new())
            .await,
        Err(BrowserError::LockedStorage)
    ));
    assert!(owner.snapshot().receipt_error);
    assert!(owner.receipts(8).await.is_err());
    assert_eq!(owner.snapshot().task, None);
}

#[test]
fn public_browser_contract_cannot_carry_raw_scripts_profiles_or_methods() {
    for value in [
        serde_json::json!({"op":"evaluate","script":"1+1"}),
        serde_json::json!({"op":"observe","script":"1+1"}),
        serde_json::json!({"op":"launch","origins":["https://example.com"],"executable":"arbitrary.exe"}),
        serde_json::json!({"op":"launch","origins":["https://example.com"],"profile":"everyday"}),
        serde_json::json!({"op":"act","reference":"ba02e2e0-a40c-44d3-b94d-35c23756c0cd","effect":{"kind":"click","script":"x"},"purpose":"do work"}),
    ] {
        assert!(serde_json::from_value::<BrowserRequest>(value).is_err());
    }
}

#[test]
fn recipient_policy_requires_exact_origins_not_wildcards_paths_or_private_http() {
    use crate::mcp::McpHttpSecurity;
    for value in [
        "http://example.com",
        "https://example.com/path",
        "https://name:secret@example.com",
        "https://example.com?q=secret",
        "file:///tmp/a",
        "https://*.example.com",
    ] {
        assert!(
            proxy::EgressPolicy::parse(&[value.into()], McpHttpSecurity::default()).is_err(),
            "accepted {value}"
        );
    }
    assert!(
        proxy::EgressPolicy::parse(
            &["https://example.com:8443".into()],
            McpHttpSecurity::default()
        )
        .is_ok()
    );
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "explicit disposable HTTPS fixture and installed qualified Edge required"]
async fn native_production_adapter_reads_effects_and_lifecycle() {
    use serde_json::{Value, json};
    use std::time::Instant;
    let fixture_file =
        std::env::var_os("XANA_BROWSER_FIXTURE").expect("synthetic fixture JSON path");
    let facts: Value = serde_json::from_slice(
        &crate::bounded_file::read(std::path::Path::new(&fixture_file), 4096).unwrap(),
    )
    .unwrap();
    let origin = facts["origin"].as_str().unwrap();
    let address: std::net::SocketAddr = facts["address"].as_str().unwrap().parse().unwrap();
    assert!(address.ip().is_loopback());
    assert_eq!(
        reqwest::Url::parse(origin).unwrap().host_str(),
        Some("localhost")
    );
    let spki = facts["spki"].as_str().unwrap();
    let counts_file = std::path::Path::new(&fixture_file)
        .parent()
        .unwrap()
        .join("counts.json");
    let before: Value = if counts_file.exists() {
        serde_json::from_slice(&crate::bounded_file::read(&counts_file, 4096).unwrap()).unwrap()
    } else {
        json!({"clicks":0,"forms":0,"downloads":0})
    };
    async fn apply(owner: &BrowserOwner, request: BrowserRequest) -> BrowserReceipt {
        let description = format!("{request:?}");
        let plan = owner.plan(request).unwrap_or_else(|error| {
            panic!(
                "planned {description}: {error:?}; snapshot={:?}",
                owner.snapshot()
            )
        });
        let receipt = owner
            .execute(plan, crate::identity::OperationId::new())
            .await
            .expect("native fixture receipt");
        assert!(
            receipt.acknowledged,
            "{description}: {}",
            serde_json::to_string(&receipt).unwrap()
        );
        receipt
    }
    let mut timings = Vec::new();
    for cycle in 0..5 {
        let (_root, owner) = fixture();
        let owner = owner.native_fixture(origin, address, spki.into());
        let started = Instant::now();
        apply(
            &owner,
            BrowserRequest::Launch {
                origins: vec![origin.into()],
            },
        )
        .await;
        let ready_ms = started.elapsed().as_secs_f64() * 1000.;
        let cold = Instant::now();
        apply(
            &owner,
            BrowserRequest::Navigate {
                url: format!("{origin}/"),
            },
        )
        .await;
        let observed = apply(&owner, BrowserRequest::Observe {}).await;
        let cold_ms = cold.elapsed().as_secs_f64() * 1000.;
        assert!(
            observed.observation.as_ref().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Production adapter fixture")
        );
        let warm = Instant::now();
        let current = apply(&owner, BrowserRequest::Observe {}).await;
        let warm_ms = warm.elapsed().as_secs_f64() * 1000.;
        let active_resources = owner.metrics().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let idle_resources = owner.metrics().await.unwrap();
        assert_eq!(
            idle_resources["sampled_processes"],
            idle_resources["processes"]
        );
        let mut stale_cleanup_ms = None;
        if cycle == 4 {
            let target = current.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["label"] == "Count effect")
                .unwrap()["id"]
                .as_str()
                .unwrap();
            let approved = owner
                .plan(BrowserRequest::Act {
                    reference: target.into(),
                    effect: BrowserEffect::Click {},
                    purpose: "Synthetic stale attribute must not dispatch".into(),
                })
                .unwrap();
            owner.mutate_fixture_target().await.unwrap();
            let stale_cleanup = Instant::now();
            let receipt = owner
                .execute(approved, crate::identity::OperationId::new())
                .await
                .unwrap();
            assert!(!receipt.acknowledged);
            assert_eq!(owner.snapshot().state, "closed");
            stale_cleanup_ms = Some(stale_cleanup.elapsed().as_secs_f64() * 1000.);
        }
        if cycle == 0 {
            let refs = observed.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap();
            assert!(
                !refs
                    .iter()
                    .any(|v| v["label"] == "Forbidden upload" || v["label"] == "Secret password")
            );
            // Fresh observation replaced the earlier opaque reference set.
            let stale = refs.iter().find(|v| v["label"] == "Count effect").unwrap()["id"]
                .as_str()
                .unwrap();
            assert!(matches!(
                owner.plan(BrowserRequest::Act {
                    reference: stale.into(),
                    effect: BrowserEffect::Click {},
                    purpose: "synthetic stale reference rejection".into(),
                }),
                Err(BrowserError::Stale)
            ));
            apply(&owner, BrowserRequest::Close {}).await;
            assert_eq!(owner.snapshot().state, "closed");
            apply(
                &owner,
                BrowserRequest::Launch {
                    origins: vec![origin.into()],
                },
            )
            .await;
            apply(
                &owner,
                BrowserRequest::Navigate {
                    url: format!("{origin}/"),
                },
            )
            .await;
            let current = apply(&owner, BrowserRequest::Observe {}).await;
            let refs = current.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap();
            let click = refs.iter().find(|v| v["label"] == "Count effect").unwrap()["id"]
                .as_str()
                .unwrap();
            apply(
                &owner,
                BrowserRequest::Act {
                    reference: click.into(),
                    effect: BrowserEffect::Click {},
                    purpose: "Increment the synthetic fixture counter exactly once".into(),
                },
            )
            .await;
            let screenshot = apply(&owner, BrowserRequest::Screenshot {}).await;
            assert_eq!(screenshot.evidence.unwrap().media_type, "image/png");
            let download_observation = apply(&owner, BrowserRequest::Observe {}).await;
            let download = download_observation.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["label"] == "Download fixture")
                .unwrap()["id"]
                .as_str()
                .unwrap();
            apply(
                &owner,
                BrowserRequest::Act {
                    reference: download.into(),
                    effect: BrowserEffect::Click {},
                    purpose: "Verify synthetic download is denied".into(),
                },
            )
            .await;
            apply(&owner, BrowserRequest::Takeover {}).await;
            assert_eq!(owner.snapshot().state, "manual_takeover");
            assert!(matches!(
                owner.plan(BrowserRequest::Observe {}),
                Err(BrowserError::TakenOver)
            ));
            let resumed = apply(&owner, BrowserRequest::Resume {}).await;
            let field = resumed.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["label"] == "Synthetic value")
                .unwrap()["id"]
                .as_str()
                .unwrap();
            apply(
                &owner,
                BrowserRequest::Act {
                    reference: field.into(),
                    effect: BrowserEffect::Fill {
                        text: "synthetic".into(),
                    },
                    purpose: "Fill synthetic fixture field".into(),
                },
            )
            .await;
            let current = apply(&owner, BrowserRequest::Observe {}).await;
            let submit = current.observation.as_ref().unwrap()["references"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["label"] == "Submit fixture")
                .unwrap()["id"]
                .as_str()
                .unwrap();
            apply(
                &owner,
                BrowserRequest::Act {
                    reference: submit.into(),
                    effect: BrowserEffect::Click {},
                    purpose: "Submit only the synthetic test form".into(),
                },
            )
            .await;
        }
        let closing = Instant::now();
        if cycle == 1 || cycle == 2 {
            if cycle == 1 {
                owner.cancel_fixture();
            } else {
                tokio::time::pause();
                tokio::time::advance(std::time::Duration::from_secs(MAX_TASK_SECONDS + 1)).await;
                tokio::time::resume();
            }
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                while owner.snapshot().state != "closed" {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("idle cancellation must join owned cleanup without another tool call");
        } else if cycle == 3 {
            owner
                .shutdown()
                .await
                .expect("runtime shutdown joins exact browser cleanup");
        } else {
            apply(&owner, BrowserRequest::Close {}).await;
        }
        let close_ms = stale_cleanup_ms.unwrap_or_else(|| closing.elapsed().as_secs_f64() * 1000.);
        let close_mode = ["explicit", "cancel", "deadline", "shutdown", "stale_action"][cycle];
        let profile_root = owner.paths().cache_dir().join("browser");
        assert_eq!(
            std::fs::read_dir(profile_root).unwrap().count(),
            0,
            "owned profile remains"
        );
        assert!(!owner.receipts(64).await.unwrap().is_empty());
        timings.push(json!({"cycle":cycle,"ready_ms":ready_ms,"cold_observe_ms":cold_ms,"warm_observe_ms":warm_ms,"close_ms":close_ms,"close_mode":close_mode,"active_resources":active_resources,"idle_resources":idle_resources,"profile":if cfg!(debug_assertions){"debug"}else{"release"}}));
    }
    println!(
        "NATIVE_BROWSER_QUALIFICATION {}",
        serde_json::to_string(&timings).unwrap()
    );
    for mutation in ["action", "value", "secret"] {
        let (_root, owner) = fixture();
        let owner = owner.native_fixture(origin, address, spki.into());
        apply(
            &owner,
            BrowserRequest::Launch {
                origins: vec![origin.into()],
            },
        )
        .await;
        apply(
            &owner,
            BrowserRequest::Navigate {
                url: format!("{origin}/"),
            },
        )
        .await;
        if mutation == "secret" {
            owner.mutate_fixture_form(mutation).await.unwrap();
        }
        let observed = apply(&owner, BrowserRequest::Observe {}).await;
        let submit = observed.observation.as_ref().unwrap()["references"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["label"] == "Submit fixture")
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let request = BrowserRequest::Act {
            reference: submit.into(),
            effect: BrowserEffect::Click {},
            purpose: "Synthetic exact form binding".into(),
        };
        let mut registry = crate::tool::ToolRegistry::new();
        super::register_tools(
            &mut registry,
            owner.clone(),
            std::collections::BTreeSet::from([crate::config::OutboundDataClass::PromptText]),
        )
        .unwrap();
        let call = crate::message::ToolCall {
            id: "native-fixture-preview".into(),
            name: "browser".into(),
            arguments: serde_json::to_value(&request).unwrap(),
        };
        if mutation == "secret" {
            assert!(
                !serde_json::to_string(&observed)
                    .unwrap()
                    .contains("XANA_BROWSER_SECRET_CANARY")
            );
            assert!(matches!(
                owner.plan(request),
                Err(BrowserError::InvalidInput)
            ));
            assert!(registry.plan(&call, owner.paths().cache_dir()).is_err());
            apply(&owner, BrowserRequest::Close {}).await;
            assert!(
                !serde_json::to_string(&owner.receipts(64).await.unwrap())
                    .unwrap()
                    .contains("XANA_BROWSER_SECRET_CANARY")
            );
            continue;
        }
        let plan = owner.plan(request).unwrap();
        let review = plan.review.as_ref().unwrap();
        let prepared = registry.plan(&call, owner.paths().cache_dir()).unwrap();
        assert_eq!(
            &prepared.final_arguments()["observed_target"],
            review,
            "approval must expose the actual bounded target, not only an opaque ID"
        );
        assert_eq!(review["url"], format!("{origin}/"));
        assert_eq!(review["label"], "Submit fixture");
        assert_eq!(
            review["element"]["form"]["action"],
            format!("{origin}/submit")
        );
        assert_eq!(review["element"]["form"]["method"], "post");
        assert_eq!(review["element"]["form"]["fields"][0]["name"], "value");
        owner.mutate_fixture_form(mutation).await.unwrap();
        let receipt = owner
            .execute(plan, crate::identity::OperationId::new())
            .await
            .unwrap();
        assert!(!receipt.acknowledged, "changed {mutation} must not submit");
        let retained = serde_json::to_string(&owner.receipts(64).await.unwrap()).unwrap();
        assert!(!retained.contains("XANA_BROWSER_SECRET_CANARY"));
        assert_eq!(owner.snapshot().state, "closed");
    }
    let counts: Value =
        serde_json::from_slice(&crate::bounded_file::read(&counts_file, 4096).unwrap()).unwrap();
    assert_eq!(
        counts["unapprovedRequests"], 0,
        "unapproved HTTPS recipient was contacted"
    );
    assert_eq!(
        counts["unapprovedWebSockets"], 0,
        "unapproved WSS recipient was contacted"
    );
    assert_eq!(counts["udpPackets"], 0, "non-proxied UDP was emitted");
    assert_eq!(
        counts["intrinsicPoisonEffects"], 0,
        "page-world prototype poisoned a typed action"
    );
    for (key, expected) in [("clicks", 1), ("forms", 1), ("downloads", 1)] {
        assert_eq!(
            counts[key].as_u64().unwrap() - before[key].as_u64().unwrap(),
            expected,
            "unexpected {key} effects; never blindly retry"
        );
    }
    println!("NATIVE_BROWSER_RECIPIENT_COUNTS {counts}");
}
