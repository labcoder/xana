//! Consumer-only contract for the governed image-turn facade. Credentials and
//! storage custody are synthetic and isolated in a child test process.

#[path = "support/adapter_vision_fixture.rs"]
mod fixture;

use fixture::*;
use std::{process::Command, time::Duration};
use xana::desktop::{DesktopLaunch, DesktopVisionDecision, DesktopVisionError, VisionStatus};

#[test]
fn governed_vision_facade_preserves_approval_order_and_durable_provenance() {
    if std::env::var_os("XANA_ADAPTER_VISION_CHILD").is_some() {
        run_contract();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let key = directory.path().join("recovery.key");
    let output = Command::new(env!("CARGO_BIN_EXE_xana"))
        .env("XANA_HOME", directory.path().join("unused-home"))
        .env_remove("XANA_STORAGE_RECOVERY_KEY")
        .args(["storage", "recovery-key", "--output"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "governed_vision_facade_preserves_approval_order_and_durable_provenance",
            "--nocapture",
        ])
        .env("XANA_ADAPTER_VISION_CHILD", directory.path())
        .env("XANA_HOME", directory.path().join("unused-child-home"))
        .env("XANA_STORAGE_RECOVERY_KEY", &key)
        .env(
            "XANA_VISION_FIXTURE_KEY",
            "synthetic-fixture-not-a-live-credential",
        )
        .env_remove("XANA_VISION_MISSING_FIXTURE_KEY")
        .env("NO_COLOR", "1")
        .env("NO_PROXY", "127.0.0.1,localhost,::1")
        .env("no_proxy", "127.0.0.1,localhost,::1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_contract() {
    let root = std::path::PathBuf::from(std::env::var_os("XANA_ADAPTER_VISION_CHILD").unwrap());
    let brain = Provider::new(Reply::Text);
    let specialist = Provider::https(Reply::Usage);
    let home = root.join("specialist-home");
    let workspace = root.join("specialist-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    initialize(&home, &brain.url, &specialist.url, false);
    let launch = specialist.launch(DesktopLaunch::new(
        &workspace,
        Some(home.clone().into_os_string()),
    ));
    let mut client = Capture::new(fixture::launch(launch.clone()).unwrap());
    let first = client.stage(png([255, 0, 0, 255]));
    let second = client.stage(png([0, 0, 255, 255]));
    assert!(
        client
            .client
            .stage_image_bytes(vec![0; 4 * 1024 * 1024 + 1], "image/png")
            .is_err()
    );
    assert!(
        client
            .client
            .stage_image_bytes(vec![0; 4], "image/svg+xml")
            .is_err()
    );
    let corrupt = client
        .client
        .stage_image_bytes(vec![0; 4], "image/png")
        .unwrap();
    client.rejected(corrupt.command_id, DesktopVisionError::InvalidImage);
    let missing = client
        .client
        .plan_vision_turn(
            "No hidden fallback",
            vec![first.clone()],
            Some("not-configured".into()),
        )
        .unwrap();
    client.rejected(missing.command_id, DesktopVisionError::Unsupported);

    let denied = client.plan(
        "Compare these selected images",
        vec![first.clone(), second.clone()],
        None,
    );
    assert!(denied.approval_required);
    assert_eq!(denied.receipt.sources[0].artifact_id, first.id);
    assert_eq!(denied.receipt.sources[1].artifact_id, second.id);
    assert_eq!(
        denied.receipt.destination.route.as_deref(),
        Some("describe")
    );
    assert_eq!(specialist.calls(), 0);
    let decision = client
        .client
        .decide_vision(denied, DesktopVisionDecision::Deny, false)
        .unwrap();
    assert_eq!(
        client.receipt(decision.command_id).status,
        VisionStatus::Denied
    );

    let changed_config = client.plan(
        "The exact configuration must stay frozen",
        vec![first.clone()],
        None,
    );
    let original_config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!("{original_config}\n# reviewed configuration changed\n"),
    )
    .unwrap();
    let request = client
        .client
        .decide_vision(changed_config, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    client.rejected(request.command_id, DesktopVisionError::StalePlan);
    std::fs::write(home.join("config.toml"), original_config).unwrap();
    assert_eq!(specialist.calls(), 0);
    let stale = client.plan("Compare images", vec![first.clone(), second.clone()], None);
    let mut changed = stale.clone();
    changed.receipt.sources.swap(0, 1);
    let decision = client
        .client
        .decide_vision(changed, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    client.rejected(decision.command_id, DesktopVisionError::StalePlan);
    let replay = client
        .client
        .decide_vision(stale, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    client.rejected(replay.command_id, DesktopVisionError::StalePlan);
    assert_eq!(specialist.calls(), 0);

    let cancel = client.plan("Do not send this", vec![first.clone()], None);
    let operation = cancel.operation_id();
    let request = client.client.cancel_vision(operation).unwrap();
    assert_eq!(
        client.receipt(request.command_id).status,
        VisionStatus::Cancelled
    );
    assert_eq!(specialist.calls(), 0);

    let approved = client.plan(
        "Compare these selected images",
        vec![first.clone(), second.clone()],
        Some("describe".into()),
    );
    let operation = approved.operation_id();
    let request = client
        .client
        .decide_vision(approved.clone(), DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    let receipt = client.receipt(request.command_id);
    assert_eq!(
        receipt.status,
        VisionStatus::AnalysisReady,
        "receipt: {receipt:?}; specialist requests: {}; brain requests: {}",
        specialist.calls(),
        brain.calls()
    );
    assert_eq!(receipt.usage.input_tokens, Some(12));
    assert_eq!(receipt.usage.output_tokens, Some(3));
    assert_eq!(receipt.usage.cost_microusd, None);
    assert!(receipt.untrusted_derivative && receipt.derivative.is_some());
    client.completed(operation);
    assert_eq!(specialist.calls(), 1);
    assert_eq!(brain.calls(), 1);
    let sent = specialist.requests();
    let blocks = sent[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    let urls = blocks
        .iter()
        .filter_map(|block| block["image_url"]["url"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(urls.len(), 2);
    assert_eq!(decode_image(urls[0]), png([255, 0, 0, 255]));
    assert_eq!(decode_image(urls[1]), png([0, 0, 255, 255]));
    let brain_request = brain.requests()[0].to_string();
    assert!(brain_request.contains("untrusted model output"));
    assert!(brain_request.contains(&first.id) && brain_request.contains(&second.id));
    assert!(!brain_request.contains("data:image"));
    let replay = client
        .client
        .decide_vision(approved, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    client.rejected(replay.command_id, DesktopVisionError::StalePlan);
    client.client.shutdown().unwrap();
    let mut client = Capture::new(fixture::launch(launch).unwrap());
    let inspection = client
        .client
        .inspect_vision_receipt(&operation.to_string())
        .unwrap();
    assert_eq!(client.receipt(inspection.command_id), receipt);
    assert_eq!(
        specialist.calls(),
        1,
        "restore must not replay specialist analysis"
    );
    client.client.shutdown().unwrap();

    // The ordinary capable-model route sends original images to the brain once,
    // without selecting or invoking the configured specialist.
    let native_home = root.join("native-home");
    let native_workspace = root.join("native-workspace");
    std::fs::create_dir_all(&native_workspace).unwrap();
    initialize(&native_home, &brain.url, &specialist.url, true);
    let mut native = Capture::new(
        fixture::launch(DesktopLaunch::new(
            &native_workspace,
            Some(native_home.into_os_string()),
        ))
        .unwrap(),
    );
    let image = native.stage(png([0, 255, 0, 255]));
    let foreign = native
        .client
        .plan_vision_turn("Do not borrow another owner", vec![first], None)
        .unwrap();
    native.rejected(foreign.command_id, DesktopVisionError::WrongOwner);
    let plan = native.plan("Describe the image", vec![image], None);
    assert!(!plan.approval_required);
    assert!(plan.receipt.destination.route.is_none());
    let operation = plan.operation_id();
    let request = native
        .client
        .decide_vision(plan, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    let receipt = native.receipt(request.command_id);
    assert_eq!(receipt.status, VisionStatus::NativeSubmitted);
    assert_eq!(receipt.usage.input_tokens, None);
    native.completed(operation);
    assert_eq!(specialist.calls(), 1);
    assert_eq!(brain.calls(), 2);
    assert!(
        brain.requests()[1]
            .to_string()
            .contains("data:image/png;base64,")
    );
    native.client.shutdown().unwrap();

    // Joined cancellation after dispatch does not continue into a brain turn.
    let waiting = Provider::https(Reply::Hold);
    let cancel_home = root.join("cancel-home");
    initialize(&cancel_home, &brain.url, &waiting.url, false);
    let mut cancelled = Capture::new(
        fixture::launch(waiting.launch(DesktopLaunch::new(
            &workspace,
            Some(cancel_home.into_os_string()),
        )))
        .unwrap(),
    );
    let image = cancelled.stage(png([1, 2, 3, 255]));
    let plan = cancelled.plan("Wait for this synthetic analysis", vec![image], None);
    let operation = plan.operation_id();
    let request = cancelled
        .client
        .decide_vision(plan, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    wait(Duration::from_secs(10), || waiting.calls() == 1);
    cancelled.client.cancel_vision(operation).unwrap();
    assert_eq!(
        cancelled.receipt(request.command_id).status,
        VisionStatus::Cancelled
    );
    assert_eq!(brain.calls(), 2);
    cancelled.client.shutdown().unwrap();

    let failing = Provider::https(Reply::Failure);
    let failed_home = root.join("failed-home");
    initialize(&failed_home, &brain.url, &failing.url, false);
    let mut failed = Capture::new(
        fixture::launch(failing.launch(DesktopLaunch::new(
            &workspace,
            Some(failed_home.into_os_string()),
        )))
        .unwrap(),
    );
    let image = failed.stage(png([4, 5, 6, 255]));
    let plan = failed.plan("Synthetic provider failure", vec![image], None);
    let request = failed
        .client
        .decide_vision(plan, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    let receipt = failed.receipt(request.command_id);
    assert_eq!(receipt.status, VisionStatus::Failed);
    assert!(
        !serde_json::to_string(&receipt)
            .unwrap()
            .contains("private-provider-canary")
    );
    assert_eq!(
        brain.calls(),
        2,
        "failed analysis must not continue to the brain"
    );
    failed.client.shutdown().unwrap();

    let missing_home = root.join("missing-credential-home");
    initialize(&missing_home, &brain.url, &specialist.url, false);
    let config = std::fs::read_to_string(missing_home.join("config.toml"))
        .unwrap()
        .replace("XANA_VISION_FIXTURE_KEY", "XANA_VISION_MISSING_FIXTURE_KEY");
    std::fs::write(missing_home.join("config.toml"), config).unwrap();
    let mut unavailable = Capture::new(
        fixture::launch(specialist.launch(DesktopLaunch::new(
            &workspace,
            Some(missing_home.into_os_string()),
        )))
        .unwrap(),
    );
    let image = unavailable.stage(png([7, 8, 9, 255]));
    let plan = unavailable.plan("Missing credential must not dispatch", vec![image], None);
    let request = unavailable
        .client
        .decide_vision(plan, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    assert_eq!(
        unavailable.receipt(request.command_id).status,
        VisionStatus::Unavailable
    );
    assert_eq!(specialist.calls(), 1);
    assert_eq!(brain.calls(), 2);
    unavailable.client.shutdown().unwrap();

    // Trusting a different configured origin must not relax this recipient's
    // handshake, even when both servers use the same disposable fixture CA.
    let other = Provider::https(Reply::Usage);
    let wrong_home = root.join("wrong-origin-home");
    initialize(&wrong_home, &brain.url, &specialist.url, false);
    let config_path = wrong_home.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str(&format!(
        "\n[service_connections.other]\nadapter = \"openai.vision\"\nbase_url = \"{}\"\n",
        other.url
    ));
    std::fs::write(config_path, config).unwrap();
    let mut wrong = Capture::new(
        fixture::launch(other.launch(DesktopLaunch::new(
            &workspace,
            Some(wrong_home.into_os_string()),
        )))
        .unwrap(),
    );
    let image = wrong.stage(png([10, 11, 12, 255]));
    let plan = wrong.plan("A different origin is not trusted", vec![image], None);
    let request = wrong
        .client
        .decide_vision(plan, DesktopVisionDecision::AllowOnce, false)
        .unwrap();
    assert_eq!(
        wrong.receipt(request.command_id).status,
        VisionStatus::Failed
    );
    assert_eq!(
        specialist.calls(),
        1,
        "TLS rejection must occur before the image request"
    );
    assert_eq!(
        other.calls(),
        0,
        "trust configuration is not a routing instruction"
    );
    assert_eq!(brain.calls(), 2);
    wrong.client.shutdown().unwrap();
}
