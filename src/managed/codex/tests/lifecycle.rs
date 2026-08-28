use super::*;
use std::ffi::OsString;
use tempfile::{TempDir, tempdir};

struct Fixture {
    directory: TempDir,
    program: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempdir().expect("fixture directory");
        let program = directory
            .path()
            .join(format!("codex-fixture{}", std::env::consts::EXE_SUFFIX));
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_app_server.rs");
        let compilation = std::process::Command::new(
            std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc")),
        )
        .args(["--edition=2024", "--crate-name", "xana_codex_fixture"])
        .arg(source)
        .arg("-o")
        .arg(&program)
        .output()
        .expect("fixture compiler starts");
        assert!(
            compilation.status.success(),
            "fixture compilation failed: {}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        Self { directory, program }
    }

    fn workspace(&self) -> PathBuf {
        self.directory.path().canonicalize().expect("workspace")
    }

    async fn spawn(&self, script: &str) -> CodexAppServer {
        let completed = self.directory.path().join("completed");
        if completed.exists() {
            std::fs::remove_file(completed).expect("clear previous fixture receipt");
        }
        let initialize = json!({"id":1,"result":{"codexHome":self.directory.path()}});
        let script = format!(
            "READ \"method\":\"initialize\"\nHAS \"experimentalApi\":false\nSEND {initialize}\nREAD \"method\":\"initialized\"\n{script}"
        );
        std::fs::write(self.directory.path().join("script"), script).expect("write script");
        CodexAppServer::spawn(&CodexLaunchConfig {
            program: self.program.to_string_lossy().into_owned(),
            home: Some(self.directory.path().to_owned()),
        })
        .await
        .expect("inert Codex fixture starts")
    }

    fn assert_complete(&self) {
        assert_eq!(
            std::fs::read_to_string(self.directory.path().join("completed"))
                .expect("fixture consumed exactly its script and observed shutdown"),
            "ok"
        );
    }
}

#[tokio::test]
async fn start_and_resume_reject_incompatible_effective_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let valid = json!({
        "thread":{"id":"thread-1"},
        "approvalPolicy":"on-request",
        "approvalsReviewer":"user",
        "sandbox":{"type":"workspaceWrite"},
        "cwd":workspace
    });
    let mut widened_network = valid.clone();
    widened_network["sandbox"]["networkAccess"] = json!(true);
    let mut widened_roots = valid.clone();
    widened_roots["sandbox"]["writableRoots"] = json!([workspace.parent().unwrap()]);
    let cases = [
        ("approvalPolicy", json!("never"), "approvalPolicy"),
        ("approvalsReviewer", Value::Null, "approvalsReviewer"),
        ("sandbox", json!({"type":"dangerFullAccess"}), "sandbox"),
        ("cwd", json!(workspace.join("other")), "cwd"),
        ("cwd", json!("relative"), "cwd"),
    ];
    let mut incompatible = cases
        .into_iter()
        .map(|(field, value, error)| {
            let mut response = valid.clone();
            response[field] = value;
            (response, error)
        })
        .collect::<Vec<_>>();
    incompatible.push((widened_network, "networkAccess"));
    incompatible.push((widened_roots, "writableRoots"));

    for method in ["thread/start", "thread/resume"] {
        for (result, field) in &incompatible {
            let response = json!({"id":2,"result":result});
            let mut server = fixture
                .spawn(&format!(
                    "READ \"method\":\"{method}\"\nHAS \"approvalsReviewer\":\"user\"\nSEND {response}\n"
                ))
                .await;
            let mut handler = TestHandler::default();
            let result = if method == "thread/start" {
                server
                    .start_thread("model-fixture", &workspace, "Xana identity", &mut handler)
                    .await
            } else {
                server
                    .resume_thread(
                        "thread-1",
                        "model-fixture",
                        &workspace,
                        "Xana identity",
                        &mut handler,
                    )
                    .await
            };
            assert!(
                matches!(result, Err(CodexError::Protocol(ref message)) if message.contains(field)),
                "{method} must reject incompatible {field}: {result:?}"
            );
            server.shutdown().await.expect("fixture shuts down");
            fixture.assert_complete();
        }
    }
}

#[tokio::test]
async fn incompatible_thread_policy_prevents_a_turn_and_connection_reuse() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let response = json!({
        "id":2,
        "result":{
            "thread":{"id":"thread-1"},
            "approvalPolicy":"on-request",
            "approvalsReviewer":"auto_review",
            "sandbox":{"type":"workspaceWrite"},
            "cwd":workspace
        }
    });
    let mut server = fixture
        .spawn(&format!(
            "READ \"method\":\"thread/start\"\nSEND {response}\n"
        ))
        .await;
    let mut handler = TestHandler::default();

    let result = server
        .start_thread("model-fixture", &workspace, "Xana identity", &mut handler)
        .await;
    assert!(
        matches!(result, Err(CodexError::Protocol(ref message)) if message.contains("approvalsReviewer")),
        "an automatic reviewer must not masquerade as Xana-controlled approval: {result:?}"
    );
    let result = server
        .run_turn_cancellable(
            "thread-1",
            "model-fixture",
            &ManagedTurnOptions {
                reasoning_effort: None,
                reasoning_summary: None,
            },
            ManagedTurnInput {
                text: "must never be sent".to_owned(),
                local_images: Vec::new(),
            },
            &CancellationToken::new(),
            &mut handler,
        )
        .await;
    assert!(
        matches!(result, Err(CodexError::Protocol(ref message)) if message.contains("unavailable")),
        "a rejected thread must not send another request: {result:?}"
    );
    server.shutdown().await.expect("fixture shuts down");
    fixture.assert_complete();
}

#[tokio::test]
async fn checked_start_and_resume_preserve_identity_and_run_the_selected_turn() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    // Exercise the non-verbatim spelling that the Windows vendor may return.
    let cwd = workspace.to_string_lossy();
    let cwd = cwd.strip_prefix(r"\\?\").unwrap_or(&cwd);
    for (method, approval) in [
        ("thread/start", ManagedApprovalPolicy::OnRequest),
        ("thread/start", ManagedApprovalPolicy::Never),
        ("thread/resume", ManagedApprovalPolicy::OnRequest),
    ] {
        let thread = json!({
            "id":2,
            "result":{
                "thread":{"id":"thread-1"},
                "approvalPolicy":approval.wire(),
                "approvalsReviewer":"user",
                "sandbox":{
                    "type":"workspaceWrite",
                    "networkAccess":false,
                    "writableRoots":[cwd]
                },
                "cwd":cwd
            }
        });
        let mut server = fixture
            .spawn(&format!(
                "READ \"method\":\"{method}\"\n\
                 HAS \"approvalsReviewer\":\"user\"\n\
                 HAS \"approvalPolicy\":\"{}\"\n\
                 HAS \"sandbox\":\"workspace-write\"\n\
                 HAS \"developerInstructions\":\"Xana identity\"\n\
                 LACKS \"baseInstructions\"\n\
                 SEND {thread}\n\
                 READ \"method\":\"turn/start\"\n\
                 HAS \"threadId\":\"thread-1\"\n\
                 HAS \"model\":\"model-fixture\"\n\
                 HAS \"effort\":\"xhigh\"\n\
                 HAS \"summary\":\"detailed\"\n\
                 SEND {{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-1\"}}}}}}\n\
                 SEND {{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"delta\":\"answer\"}}}}\n\
                 SEND {{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thread-1\",\"turn\":{{\"id\":\"turn-1\",\"status\":\"completed\"}}}}}}\n",
                approval.wire()
            ))
            .await;
        let mut handler = TestHandler::default();
        let thread = if method == "thread/start" {
            server
                .start_thread_with_policy(
                    "model-fixture",
                    &workspace,
                    "Xana identity",
                    ManagedThreadPolicy {
                        approval,
                        ..ManagedThreadPolicy::default()
                    },
                    &mut handler,
                )
                .await
        } else {
            server
                .resume_thread(
                    "thread-1",
                    "model-fixture",
                    &workspace,
                    "Xana identity",
                    &mut handler,
                )
                .await
        }
        .expect("matching effective policy admits the thread");
        let turn = server
            .run_turn(
                &thread,
                "model-fixture",
                &ManagedTurnOptions {
                    reasoning_effort: Some("xhigh".to_owned()),
                    reasoning_summary: Some(ReasoningSummary::Detailed),
                },
                ManagedTurnInput {
                    text: "answer the fixture".to_owned(),
                    local_images: Vec::new(),
                },
                &mut handler,
            )
            .await
            .expect("validated thread completes the turn");
        assert_eq!(turn.final_text, "answer");
        assert_eq!(turn.thread_id, "thread-1");
        server.shutdown().await.expect("fixture shuts down");
        fixture.assert_complete();
    }
}

#[tokio::test]
async fn resume_refuses_a_different_vendor_thread() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let response = json!({
        "id":2,
        "result":{
            "thread":{"id":"another-conversation"},
            "approvalPolicy":"on-request",
            "approvalsReviewer":"user",
            "sandbox":{"type":"workspaceWrite"},
            "cwd":workspace
        }
    });
    let mut server = fixture
        .spawn(&format!(
            "READ \"method\":\"thread/resume\"\nHAS \"threadId\":\"thread-1\"\nSEND {response}\n"
        ))
        .await;
    let result = server
        .resume_thread(
            "thread-1",
            "model-fixture",
            &workspace,
            "Xana identity",
            &mut TestHandler::default(),
        )
        .await;
    assert!(
        matches!(result, Err(CodexError::Protocol(ref message)) if message.contains("resumed thread id")),
        "resume cannot substitute another conversation: {result:?}"
    );
    server.shutdown().await.expect("fixture shuts down");
    fixture.assert_complete();
}

#[tokio::test]
async fn unfinished_rpc_or_invalid_turn_stops_the_child_before_reuse() {
    let fixture = Fixture::new();
    for (method, response) in [
        ("account/read", "not-json"),
        ("turn/start", r#"{"id":2,"result":{"turn":{"id":""}}}"#),
        (
            "turn/start",
            r#"{"id":99,"method":"unsupported/request","params":{}}"#,
        ),
    ] {
        let mut server = fixture
            .spawn(&format!("READ \"method\":\"{method}\"\nSEND {response}\n"))
            .await;
        let mut handler = TestHandler::default();
        let options = ManagedTurnOptions {
            reasoning_effort: None,
            reasoning_summary: None,
        };
        let input = || ManagedTurnInput {
            text: "fixture".into(),
            local_images: Vec::new(),
        };
        let result = if method == "account/read" {
            server.account_status().await.map(|_| ())
        } else {
            server
                .run_turn_cancellable(
                    "thread-1",
                    "model-fixture",
                    &options,
                    input(),
                    &CancellationToken::new(),
                    &mut handler,
                )
                .await
                .map(|_| ())
        };
        assert!(result.is_err(), "invalid response must fail: {response}");
        let reuse = server
            .run_turn_cancellable(
                "thread-1",
                "model-fixture",
                &options,
                input(),
                &CancellationToken::new(),
                &mut handler,
            )
            .await;
        assert!(
            matches!(reuse, Err(CodexError::Protocol(ref message)) if message.contains("unavailable")),
            "uncertain protocol state must reject reuse before a write: {reuse:?}"
        );
        timeout(Duration::from_secs(2), server.child.wait())
            .await
            .expect("failed protocol retires the process promptly")
            .expect("child exit is reaped");
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn correlated_remote_rejection_does_not_poison_a_synchronized_connection() {
    let fixture = Fixture::new();
    let mut server = fixture
        .spawn(
            "READ \"method\":\"account/read\"\n\
         SEND {\"id\":2,\"error\":{\"code\":-32600,\"message\":\"fixture rejection\"}}\n\
         READ \"method\":\"account/read\"\n\
         SEND {\"id\":3,\"result\":{\"account\":null}}\n",
        )
        .await;
    assert!(matches!(
        server.account_status().await,
        Err(CodexError::Remote { .. })
    ));
    assert_eq!(
        server.account_status().await.unwrap(),
        AccountStatus::LoggedOut
    );
    server.shutdown().await.unwrap();
    fixture.assert_complete();
}

#[tokio::test]
async fn unsupported_or_malformed_approval_cannot_authorize_an_active_turn() {
    let fixture = Fixture::new();
    let valid = json!({
        "id":99,"method":"item/commandExecution/requestApproval",
        "params":{
            "threadId":"thread-1","turnId":"turn-1","itemId":"item-1",
            "command":"echo fixture","cwd":fixture.workspace()
        }
    });
    let cases = [
        ("availableDecisions", json!("accept")),
        ("availableDecisions", json!(["decline", "cancel"])),
        (
            "networkApprovalContext",
            json!({"host":"example.com","protocol":"https"}),
        ),
        ("grantRoot", json!(fixture.workspace().parent().unwrap())),
        ("command", Value::Null),
        ("command", json!("x".repeat(MAX_ITEM_DETAIL_BYTES + 1))),
        ("cwd", json!("x".repeat(4097))),
        ("itemId", Value::Null),
        ("threadId", json!("wrong-thread")),
        ("turnId", json!("wrong-turn")),
        ("requestId", json!({"invalid":"id"})),
    ];
    for (field, value) in cases {
        let mut request = valid.clone();
        request["params"][field] = value;
        if field == "requestId" {
            request["id"] = request["params"][field].take();
        }
        if field == "grantRoot" {
            request["method"] = json!("item/fileChange/requestApproval");
        }
        let mut server = fixture.spawn(&format!(
            "READ \"method\":\"turn/start\"\n\
             SEND {{\"id\":2,\"result\":{{\"turn\":{{\"id\":\"turn-1\"}}}}}}\n\
             SEND {request}\n\
             SEND {{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thread-1\",\"turn\":{{\"id\":\"turn-1\",\"status\":\"completed\"}}}}}}\n"
        )).await;
        let mut handler = TestHandler::default();
        let result = server
            .run_turn(
                "thread-1",
                "model-fixture",
                &ManagedTurnOptions {
                    reasoning_effort: None,
                    reasoning_summary: None,
                },
                ManagedTurnInput {
                    text: "fixture".into(),
                    local_images: Vec::new(),
                },
                &mut handler,
            )
            .await;
        assert!(
            result.is_err(),
            "unsafe {field} must not complete a turn: {result:?}"
        );
        if field != "availableDecisions" || request["params"][field].is_string() {
            assert_eq!(
                handler.approvals, 0,
                "unsafe {field} must not prompt an approval"
            );
        }
        assert!(matches!(
            server.account_status().await,
            Err(CodexError::Protocol(_))
        ));
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn approval_before_turn_acknowledgement_fails_without_prompting() {
    let fixture = Fixture::new();
    let mut server = fixture.spawn(
        "READ \"method\":\"turn/start\"\n\
         SEND {\"id\":99,\"method\":\"item/commandExecution/requestApproval\",\"params\":{\"threadId\":\"thread-1\",\"turnId\":\"unconfirmed-turn\",\"itemId\":\"command-1\",\"command\":\"echo fixture\"}}\n\
         SEND {\"id\":2,\"error\":{\"code\":-32600,\"message\":\"turn never started\"}}\n"
    ).await;
    let mut handler = TestHandler::default();
    let result = server
        .run_turn_cancellable(
            "thread-1",
            "model-fixture",
            &ManagedTurnOptions {
                reasoning_effort: None,
                reasoning_summary: None,
            },
            ManagedTurnInput {
                text: "fixture".into(),
                local_images: Vec::new(),
            },
            &CancellationToken::new(),
            &mut handler,
        )
        .await;
    assert!(
        matches!(result, Err(CodexError::Protocol(_))),
        "unconfirmed turn: {result:?}"
    );
    assert_eq!(handler.approvals, 0);
    server.shutdown().await.unwrap();
}

struct InspectingController {
    decision: ApprovalDecision,
    requests: Vec<ApprovalRequest>,
}

impl ManagedEventHandler for InspectingController {
    fn notification(&mut self, _: ManagedNotification) -> Result<(), CodexError> {
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        request: ApprovalRequest,
    ) -> BoxFuture<'a, Result<ApprovalDecision, CodexError>> {
        self.requests.push(request);
        let decision = self.decision;
        Box::pin(async move { Ok(decision) })
    }
}

#[tokio::test]
async fn exact_approval_payload_and_denial_cross_the_subprocess_boundary() {
    let fixture = Fixture::new();
    let command = format!(
        "echo {} && echo important-tail",
        "long argument ".repeat(90)
    );
    let cwd = format!(
        "{}{}",
        fixture.workspace().display(),
        "/nested-path".repeat(90)
    );
    let request = json!({
        "id":"approval-99","method":"item/commandExecution/requestApproval",
        "params":{
            "threadId":"thread-1","turnId":"turn-1","itemId":"item-1",
            "command":command,"cwd":cwd,"availableDecisions":["accept","decline"]
        }
    });
    let mut server = fixture.spawn(&format!(
        "READ \"method\":\"turn/start\"\n\
         SEND {{\"id\":2,\"result\":{{\"turn\":{{\"id\":\"turn-1\"}}}}}}\n\
         SEND {request}\n\
         READ \"id\":\"approval-99\"\n\
         HAS \"decision\":\"decline\"\n\
         SEND {{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thread-1\",\"turnId\":\"turn-1\",\"delta\":\"denied\"}}}}\n\
         SEND {{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thread-1\",\"turn\":{{\"id\":\"turn-1\",\"status\":\"completed\"}}}}}}\n"
    )).await;
    let mut handler = InspectingController {
        decision: ApprovalDecision::Decline,
        requests: Vec::new(),
    };
    let result = server
        .run_turn(
            "thread-1",
            "model-fixture",
            &ManagedTurnOptions {
                reasoning_effort: None,
                reasoning_summary: None,
            },
            ManagedTurnInput {
                text: "fixture".into(),
                local_images: Vec::new(),
            },
            &mut handler,
        )
        .await
        .unwrap();
    assert_eq!(result.final_text, "denied");
    assert_eq!(handler.requests.len(), 1);
    assert_eq!(
        handler.requests[0].command.as_deref(),
        Some(command.as_str())
    );
    assert_eq!(handler.requests[0].cwd.as_deref(), Some(cwd.as_str()));
    server.shutdown().await.unwrap();
    fixture.assert_complete();
}

#[tokio::test]
async fn duplicate_callback_is_not_presented_or_authorized_twice() {
    let fixture = Fixture::new();
    let request = json!({
        "id":99,"method":"item/fileChange/requestApproval",
        "params":{"threadId":"thread-1","turnId":"turn-1","itemId":"patch-1"}
    });
    let mut server = fixture.spawn(&format!(
        "READ \"method\":\"turn/start\"\n\
         SEND {{\"id\":2,\"result\":{{\"turn\":{{\"id\":\"turn-1\"}}}}}}\n\
         SEND {request}\n\
         READ \"id\":99\n\
         HAS \"decision\":\"accept\"\n\
         SEND {request}\n\
         SEND {{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thread-1\",\"turn\":{{\"id\":\"turn-1\",\"status\":\"completed\"}}}}}}\n"
    )).await;
    let mut handler = TestHandler::default();
    let result = server
        .run_turn(
            "thread-1",
            "model-fixture",
            &ManagedTurnOptions {
                reasoning_effort: None,
                reasoning_summary: None,
            },
            ManagedTurnInput {
                text: "fixture".into(),
                local_images: Vec::new(),
            },
            &mut handler,
        )
        .await;
    assert!(
        matches!(result, Err(CodexError::Protocol(ref message)) if message.contains("duplicate"))
    );
    assert_eq!(handler.approvals, 1);
    server.shutdown().await.unwrap();
}
