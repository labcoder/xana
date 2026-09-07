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
