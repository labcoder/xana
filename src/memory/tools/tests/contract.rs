use super::*;

#[tokio::test]
async fn advertised_actions_save_correct_and_forget_through_the_permission_broker() {
    let fixture = Fixture::new();
    let turn = input("Remember: I prefer red.");
    let result = fixture
        .invoke(
            &call(
                "memory_remember",
                json!({
                    "statement":"I prefer red", "quote":turn.text.as_ref(), "risk":"ordinary"
                }),
            ),
            &turn,
            false,
        )
        .await;
    assert_eq!(
        result.status,
        ToolResultStatus::Success,
        "{}",
        result.output
    );
    let row = fixture.owner.page(None, None).unwrap().records.remove(0);
    let turn = input("Correct that: I prefer blue.");
    let correction = call(
        "memory_correct",
        json!({
            "id":row.id,"revision":row.revision,"statement":"I prefer blue",
            "quote":turn.text.as_ref(),"risk":"ordinary"
        }),
    );
    // The direct API still cannot silently overwrite an existing fact.
    assert_eq!(
        fixture.invoke(&correction, &turn, false).await.failure,
        Some(crate::message::ToolFailure::PermissionDenied)
    );
    assert_eq!(
        fixture.invoke(&correction, &turn, true).await.status,
        ToolResultStatus::Success
    );
    let row = fixture.owner.record(row.id).unwrap();
    assert_eq!(row.statement, "I prefer blue");
    let turn = input("Forget my color preference.");
    let forgetting = call(
        "memory_forget",
        json!({"id":row.id,"revision":row.revision,"risk":"ordinary"}),
    );
    assert_eq!(
        fixture.invoke(&forgetting, &turn, true).await.status,
        ToolResultStatus::Success
    );
    assert_eq!(
        fixture.owner.record(row.id).unwrap().state,
        crate::memory::MemoryState::Forgotten
    );
    assert_eq!(
        std::fs::read_dir(fixture.workspace.path()).unwrap().count(),
        0
    );
}

#[test]
fn required_fields_reject_missing_and_null_values_before_any_effect() {
    let fixture = Fixture::new();
    let turn = input("Remember: I prefer red.");
    for name in ["memory_remember", "memory_correct", "memory_forget"] {
        let definition = fixture.registry.definition(name).unwrap();
        let required = definition.parameters["required"].as_array().unwrap();
        for field in required {
            for null in [false, true] {
                let mut args = match name {
                    "memory_remember" => {
                        json!({"statement":"I prefer red", "quote":turn.text.as_ref(),"risk":"ordinary"})
                    }
                    "memory_correct" => {
                        json!({"id":Uuid::new_v4(),"revision":1,"statement":"I prefer red", "quote":turn.text.as_ref(),"risk":"ordinary"})
                    }
                    _ => json!({"id":Uuid::new_v4(),"revision":1,"risk":"ordinary"}),
                };
                if null {
                    args[field.as_str().unwrap()] = Value::Null;
                } else {
                    args.as_object_mut()
                        .unwrap()
                        .remove(field.as_str().unwrap());
                }
                let error = fixture
                    .registry
                    .plan_in_turn(&call(name, args), fixture.workspace.path(), Some(&turn))
                    .err()
                    .unwrap();
                assert_eq!(
                    error.failure,
                    Some(crate::message::ToolFailure::InvalidMemoryArguments)
                );
            }
        }
    }
    assert!(fixture.owner.page(None, None).unwrap().records.is_empty());
}
