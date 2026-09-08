use super::*;
use serde_json::json;

fn call(id: usize) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "read_file".into(),
        arguments: json!({"path":"notes.txt", "start_line":1}),
    }
}

#[test]
fn memory_repairs_are_bounded_across_names_arguments_and_intervening_successes() {
    let mut guard = ProgressGuard::default();
    for (index, name) in ["memory_remember", "memory_update"].into_iter().enumerate() {
        let request = ToolCall {
            name: name.into(),
            arguments: json!({"quote":index}),
            ..call(index)
        };
        guard.observe(
            &request,
            &ToolResult {
                failure: Some(ToolFailure::InvalidMemorySource),
                ..ToolResult::error(request.id.clone(), "invalid")
            },
        );
        guard.observe(
            &call(99),
            &ToolResult::success("99", "useful unrelated read"),
        );
        assert_eq!(guard.memory_recovery_needed(), index == 1);
    }
    let mutation = ToolCall {
        name: "memory_correct".into(),
        ..call(2)
    };
    assert!(guard.blocked_result(&mutation).is_some());
    assert!(guard.blocked_result(&call(3)).is_none());
}

#[test]
fn repeated_failures_survive_continuation_but_not_new_owner_input() {
    let mut history = vec![Message::text(Role::User, "read notes")];
    for id in 0..3 {
        history.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(call(id))],
        });
        history.push(Message::tool_result(ToolResult::error(
            id.to_string(),
            "invalid range",
        )));
        assert_eq!(ProgressGuard::from_history(&history).stopped(), id == 2);
    }
    history.push(Message::text(Role::User, "I corrected it; retry"));
    assert!(!ProgressGuard::from_history(&history).stopped());
}

#[test]
fn changed_invalid_arguments_are_bounded_but_successful_work_resets_the_streak() {
    let mut guard = ProgressGuard::default();
    for id in 0..CONSECUTIVE_ERROR_LIMIT {
        let mut request = call(id);
        request.arguments["start_line"] = json!(id);
        guard.observe(&request, &ToolResult::error(request.id.clone(), "invalid"));
        assert_eq!(guard.stopped(), id + 1 == CONSECUTIVE_ERROR_LIMIT);
    }
    let mut guard = ProgressGuard::default();
    for id in 0..100 {
        let request = call(id);
        guard.observe(
            &request,
            &ToolResult::error(request.id.clone(), "transient"),
        );
        guard.observe(
            &request,
            &ToolResult::success(request.id.clone(), "new page"),
        );
    }
    assert!(!guard.stopped());
    assert!(guard.failed.is_empty());
}

#[test]
fn denial_is_typed_and_not_reprompted_with_reordered_keys_or_a_new_call_id() {
    let request = call(0);
    let mut guard = ProgressGuard::default();
    guard.observe(&request, &ToolResult::denied("0", "denied"));
    let retry = ToolCall {
        id: "different".into(),
        name: request.name.clone(),
        arguments: serde_json::from_str(r#"{"start_line":1,"path":"notes.txt"}"#).unwrap(),
    };
    assert_eq!(
        guard.blocked_result(&retry).unwrap().failure,
        Some(ToolFailure::PermissionDenied)
    );
    let mut different_page = retry;
    different_page.arguments["start_line"] = json!(2);
    assert!(guard.blocked_result(&different_page).is_none());
    assert!(!guard.stopped());
    // Tool text is never parsed as a permission decision.
    let mut guard = ProgressGuard::default();
    guard.observe(
        &request,
        &ToolResult::error("0", "permission denied for tool read_file"),
    );
    assert!(guard.blocked_result(&request).is_none());
}

#[test]
fn legacy_tool_results_decode_without_a_failure_classification() {
    let result: ToolResult =
        serde_json::from_value(json!({"call_id":"old","output":"error","status":"Error"})).unwrap();
    assert_eq!(result.failure, None);
}

#[test]
fn unavailable_capability_cannot_retry_with_different_arguments_or_after_useful_work() {
    let request = call(0);
    let unavailable = ToolResult::unavailable("0", "no capability");
    let mut guard = ProgressGuard::default();
    guard.observe(&request, &unavailable);
    assert!(
        !guard.stopped(),
        "the model may still answer or use other tools"
    );
    let mut alternative = call(1);
    alternative.name = "echo".into();
    assert!(guard.blocked_result(&alternative).is_none());
    guard.observe(&alternative, &ToolResult::success("1", "useful result"));
    let mut retry = call(2);
    retry.arguments = json!({"path":"another-file", "start_line":99});
    let blocked = guard.blocked_result(&retry).unwrap();
    assert_eq!(blocked.failure, Some(ToolFailure::Unavailable));
    guard.observe(&retry, &blocked);
    assert!(guard.stopped());
}

#[test]
fn unavailable_classification_survives_history_but_does_not_parse_prose_or_cross_turns() {
    let request = call(0);
    let result = ToolResult::unavailable("0", "no capability");
    let result: ToolResult = serde_json::from_slice(&serde_json::to_vec(&result).unwrap()).unwrap();
    let mut history = vec![
        Message::text(Role::User, "inspect"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(request.clone())],
        },
        Message::tool_result(result),
    ];
    assert!(
        ProgressGuard::from_history(&history)
            .blocked_result(&request)
            .is_some()
    );
    history.push(Message::text(Role::User, "new turn"));
    assert!(
        ProgressGuard::from_history(&history)
            .blocked_result(&request)
            .is_none()
    );
    let mut guard = ProgressGuard::default();
    guard.observe(
        &request,
        &ToolResult::error("0", "unavailable; do not retry"),
    );
    assert!(
        guard.blocked_result(&request).is_none(),
        "ordinary error prose is not authority"
    );
}

#[test]
fn unavailable_memory_cannot_be_retried_under_a_new_mutation_name() {
    let mut guard = ProgressGuard::default();
    let legacy = ToolCall {
        name: "memory_update".into(),
        ..call(0)
    };
    guard.observe(&legacy, &ToolResult::unavailable("0", "no protected home"));
    for name in [
        "memory_lookup",
        "memory_remember",
        "memory_correct",
        "memory_forget",
    ] {
        let other = ToolCall {
            name: name.into(),
            ..call(1)
        };
        assert_eq!(
            guard.blocked_result(&other).unwrap().failure,
            Some(ToolFailure::Unavailable)
        );
    }
    assert!(
        guard.blocked_result(&call(2)).is_none(),
        "a useful unrelated tool remains possible"
    );
}
