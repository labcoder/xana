//! A plan prepared under valid authority must not survive owner revocation.
use super::*;
use crate::{
    message::ToolCall,
    permission::PermissionScope,
    tool::{
        EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext, ToolRegistry,
    },
};

struct EffectCounter(Arc<AtomicUsize>);
impl Tool for EffectCounter {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "retained_fixture".into(),
            contract_version: 1,
            description: "Count an observable effect".into(),
            parameters: serde_json::json!({"type":"object"}),
            effect_class: EffectClass::Write,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        args: &serde_json::Value,
        _: &std::path::Path,
    ) -> Result<PlannedToolInvocation, String> {
        Ok(PlannedToolInvocation::new(
            args.clone(),
            PermissionScope::Unscoped,
            (),
        ))
    }

    fn execute<'a>(
        &'a self,
        _: &'a PlannedToolInvocation,
        _: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("effect completed".into())
        })
    }
}

fn guarded(f: &mut Fixture, cancellation: CancellationToken) -> ToolRegistry {
    f.queue();
    let execution = Uuid::new_v4();
    let guard = super::super::super::RetainedToolGuard::new(
        f.paths.clone(),
        f.store.clone(),
        &f.worker,
        execution,
        cancellation,
    );
    f.worker = f
        .store
        .retained_admit(f.worker.id, f.worker.revision, |worker| {
            worker.active = Some((execution, worker.mailbox.remove(0)));
            worker.state = WorkerState::Running;
            Ok(())
        })
        .unwrap()
        .0;
    let tool = EffectCounter(f.calls.clone());
    let expected = tool.definition();
    let mut registry = ToolRegistry::new();
    registry.register(tool).unwrap();
    let registry = guard.wrap(registry);
    assert_eq!(registry.definitions(), vec![&expected]);
    registry
}

fn context() -> ToolExecutionContext {
    ToolExecutionContext {
        operation_id: OperationId::new(),
        events: None,
        outbound_approval: None,
        cleanup: Default::default(),
    }
}

fn call() -> ToolCall {
    ToolCall {
        id: "effect".into(),
        name: "retained_fixture".into(),
        arguments: serde_json::json!({"exact":"arguments"}),
    }
}

#[tokio::test]
async fn native_guard_preserves_a_valid_plan_and_checks_again_after_stop() {
    let mut f = Fixture::new();
    let registry = guarded(&mut f, CancellationToken::new());
    let call = call();
    let plan = registry.plan(&call, f._home.path()).unwrap();
    assert_eq!(plan.final_arguments(), &call.arguments);
    assert_eq!(plan.scope(), &PermissionScope::Unscoped);
    assert_eq!(plan.execute(context()).await.unwrap(), "effect completed");
    f.command(serde_json::json!({"command":"stop","target":{"id":f.worker.id,"revision":f.worker.revision}})).unwrap();
    assert!(plan.execute(context()).await.is_err());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn native_guard_rejects_expired_reconfigured_replaced_and_cancelled_plans() {
    for cause in [
        "expired",
        "reconfigured",
        "replaced",
        "cancelled",
        "forgotten",
    ] {
        let mut f = Fixture::new();
        let cancel = CancellationToken::new();
        let registry = guarded(&mut f, cancel.clone());
        let call = call();
        let plan = registry.plan(&call, f._home.path()).unwrap();
        match cause {
            "reconfigured" => {
                let config = fs::read_to_string(f.paths.config_file())
                    .unwrap()
                    .replace("max_tool_rounds = 1", "max_tool_rounds = 2");
                fs::write(f.paths.config_file(), config).unwrap();
            }
            "cancelled" => cancel.cancel(),
            "forgotten" => {
                use crate::memory::{MemoryContext, MemoryEdit, MemoryOwner, MemoryScope};
                let owner = MemoryOwner::new(f.store.clone(), MemoryContext::default());
                let record = owner
                    .remember(MemoryScope::User, "I prefer concise answers".into(), None)
                    .unwrap();
                owner
                    .revise(record.id, record.revision, MemoryEdit::Forget)
                    .unwrap();
            }
            _ => {
                f.store
                    .retained_update(f.worker.id, f.worker.revision, |worker| {
                        if cause == "expired" {
                            worker.expires_at = crate::autonomy::now()? - 1;
                        } else {
                            worker.active.as_mut().unwrap().0 = Uuid::new_v4();
                        }
                        Ok(())
                    })
                    .unwrap();
            }
        }
        assert!(plan.execute(context()).await.is_err(), "cause={cause}");
        assert_eq!(f.calls.load(Ordering::SeqCst), 0, "cause={cause}");
    }
}
