use super::*;

fn watch() -> GithubTrigger {
    GithubTrigger::create(
        "owner/repo/41",
        CredentialReference::Environment {
            variable: "XANA_FIXTURE_GH_TOKEN".into(),
        },
    )
    .unwrap()
}
fn response(status: &str, conclusion: Option<&str>, time: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"id":41,"repository":{"full_name":"owner/repo"},"run_attempt":1,"head_sha":"a".repeat(40),"status":status,"conclusion":conclusion,"updated_at":time})).unwrap()
}

#[test]
fn exact_identity_duplicate_and_reordered_statuses_never_invent_events() {
    let mut watch = watch();
    let queued = response("queued", None, "2026-09-05T10:00:00Z");
    accept(&mut watch, &queued, Some("\"one\"".into()), 100).unwrap();
    assert!(watch.observation.pending);
    watch.observation.pending = false;
    accept(&mut watch, &queued, None, 101).unwrap();
    assert!(!watch.observation.pending);
    accept(
        &mut watch,
        &response("in_progress", None, "2026-09-05T10:01:00Z"),
        None,
        102,
    )
    .unwrap();
    watch.observation.pending = false;
    accept(&mut watch, &queued, None, 103).unwrap();
    assert!(!watch.observation.pending);
    assert_eq!(watch.last.as_ref().unwrap().status, "in_progress");
    let mut altered: serde_json::Value = serde_json::from_slice(&queued).unwrap();
    altered["id"] = 42.into();
    assert!(
        accept(
            &mut watch,
            &serde_json::to_vec(&altered).unwrap(),
            None,
            104
        )
        .is_err()
    );
    altered["id"] = 41.into();
    altered["run_attempt"] = 2.into();
    assert!(
        accept(
            &mut watch,
            &serde_json::to_vec(&altered).unwrap(),
            None,
            104
        )
        .is_err()
    );
    accept(
        &mut watch,
        &response("completed", Some("success"), "2026-09-05T10:02:00Z"),
        None,
        105,
    )
    .unwrap();
    assert!(watch.observation.pending);
    assert!(super::super::Trigger::Github(watch).completed());
}

#[test]
fn path_injection_and_missing_credential_sources_are_rejected() {
    for value in [
        "owner/repo/../41",
        "owner/repo?secret/41",
        "owner/repo/0",
        "owner/repo",
    ] {
        assert!(
            GithubTrigger::create(
                value,
                CredentialReference::Stored {
                    id: "fixture".into()
                }
            )
            .is_err()
        );
    }
    assert_eq!(
        watch().url(),
        "https://api.github.com/repos/owner/repo/actions/runs/41"
    );
    assert!(
        GithubTrigger::create(
            "owner/repo/41",
            CredentialReference::Environment {
                variable: "".into()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn http_adapter_sends_one_named_request_and_respects_rate_delay() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(
                count > 0 && bytes.len() + count <= 4096,
                "bounded request headers"
            );
            bytes.extend_from_slice(&chunk[..count]);
        }
        let request = String::from_utf8_lossy(&bytes);
        assert!(request.starts_with("GET /repos/owner/repo/actions/runs/41 HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic-only")
        );
        stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let mut watch = watch();
    let delay = poll(
        &mut watch,
        &format!("http://{address}/repos/owner/repo/actions/runs/41"),
        SecretString::new("synthetic-only".into()).unwrap(),
        100,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(delay >= 600);
    assert!(!watch.observation.pending);
    assert_eq!(watch.observation.last_checked, Some(100));
    server.await.unwrap();
}

#[test]
fn bounded_retries_need_review_instead_of_busy_looping() {
    let mut watch = watch();
    let mut previous = 0;
    for _ in 0..6 {
        let delay = retry(&mut watch, "unavailable", 0).unwrap();
        assert!(delay >= previous);
        previous = delay;
    }
    assert!(retry(&mut watch, "unavailable", 0).is_err());
}

#[test]
fn unchanged_ci_status_cannot_claim_a_task_and_terminal_status_finishes_once() {
    use crate::autonomy::{JobState, RunOutcome, RunReceipt, Schedule, triggers::Trigger};
    let (_home, store, _custody, mut job) = crate::autonomy::tests::fixture();
    let at = job.next.at;
    let mut source = watch();
    let queued = response("queued", None, "2026-09-05T10:00:00Z");
    accept(&mut source, &queued, None, at).unwrap();
    job.trigger = Some(Trigger::Github(source));
    job.schedule = Schedule::Triggered { poll_seconds: 60 };
    store.autonomy_create(job.clone()).unwrap();
    let running = store.autonomy_claim(at).unwrap().unwrap();
    let receipt = |running: &crate::autonomy::Job, finished_at| RunReceipt {
        occurrence: running.occurrence.unwrap(),
        scheduled_at: running.next.at,
        finished_at,
        outcome: RunOutcome::Completed,
        detail: "Authorized fixed status action".into(),
        coalesced: false,
        dst_adjusted: false,
    };
    let mut next = store
        .autonomy_finish(job.id, receipt(&running, at))
        .unwrap();
    let Some(Trigger::Github(source)) = next.trigger.as_mut() else {
        unreachable!()
    };
    accept(source, &queued, None, at + 60).unwrap();
    store.autonomy_observed(next).unwrap();
    // The same durable gate used by the real host refuses admission; a provider
    // cannot be called for this unchanged observation.
    assert!(store.autonomy_claim(at + 60).unwrap().is_none());
    let mut next = store.autonomy_job(job.id).unwrap();
    let Some(Trigger::Github(source)) = next.trigger.as_mut() else {
        unreachable!()
    };
    accept(
        source,
        &response("completed", Some("success"), "2026-09-05T10:01:00Z"),
        None,
        at + 60,
    )
    .unwrap();
    store.autonomy_observed(next).unwrap();
    let running = store.autonomy_claim(at + 60).unwrap().unwrap();
    let completed = store
        .autonomy_finish(job.id, receipt(&running, at + 60))
        .unwrap();
    assert_eq!(completed.state, JobState::Completed);
    assert!(store.autonomy_claim(at + 120).unwrap().is_none());
    assert_eq!(store.autonomy_receipts(job.id, 0).unwrap().len(), 2);
}
