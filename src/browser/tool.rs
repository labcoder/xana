//! Model and client requests share one browser owner; this wrapper adds Xana's
//! existing exact outbound and tool approval gates before dispatch.

use super::{BrowserOwner, BrowserPlan, BrowserReceipt, BrowserRequest, EGRESS_DISCLOSURE};
use crate::{
    config::OutboundDataClass,
    identity::OperationId,
    outbound::{
        ObservedOutboundAudit, OutboundDisposition, OutboundGuard, OutboundItem,
        OutboundPolicyLayers, OutboundRequest, OutboundTransport, OutboundTransportFailure,
        RecipientIdentity, RecipientKind,
    },
    permission::PermissionScope,
    tool::{
        EffectClass, PlannedToolInvocation, RegistryError, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext, ToolRegistry,
    },
};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

pub(crate) fn register_tools(
    registry: &mut ToolRegistry,
    owner: BrowserOwner,
    profile_egress: BTreeSet<OutboundDataClass>,
) -> Result<(), RegistryError> {
    registry.register(BrowserTool {
        owner,
        profile_egress,
    })
}
struct BrowserTool {
    owner: BrowserOwner,
    profile_egress: BTreeSet<OutboundDataClass>,
}
struct Plan {
    browser: BrowserPlan,
    recipient: RecipientIdentity,
    items: Vec<OutboundItem>,
}
impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: format!(
                "Optional dedicated local browser. launch chooses exact HTTPS origins, navigate opens an allowed URL, observe gives bounded untrusted page evidence and opaque references; screenshot returns a protected artifact. act requires a current reference and meaningful purpose; a click is not blanket consent to purchase/publish/send/delete. takeover suspends automation for manual control/login, resume reinspects, close cleans up. Password inputs require manual takeover; file uploads are unsupported. {EGRESS_DISCLOSURE}"
            ),
            parameters: json!({"type":"object","additionalProperties":false,"required":["op"],"properties":{
                "op":{"enum":["launch","navigate","observe","screenshot","act","takeover","resume","close"]},
                "origins":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","maxLength":1024}},
                "url":{"type":"string","maxLength":2048},"reference":{"type":"string","format":"uuid"},
                "effect":{"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"enum":["click","fill"]},"text":{"type":"string","maxLength":4096}}},
                "purpose":{"type":"string","minLength":1,"maxLength":1024}
            }}),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }
    fn plan(&self, arguments: &Value, _workspace: &Path) -> Result<PlannedToolInvocation, String> {
        let request: BrowserRequest = serde_json::from_value(arguments.clone())
            .map_err(|_| "browser arguments are invalid".to_owned())?;
        let browser = self
            .owner
            .plan(request)
            .map_err(|error| error.to_string())?;
        let snapshot = self.owner.snapshot();
        let origins = match &browser.request {
            BrowserRequest::Launch { origins } => origins.clone(),
            _ => snapshot.origins.clone(),
        };
        let final_arguments = json!({"request":browser.request,"observed_target":browser.review,"task":snapshot.task,"revision":snapshot.revision,"mode":EGRESS_DISCLOSURE});
        let identity = serde_json::to_vec(&json!({"origins":origins,"task":snapshot.task,"revision":snapshot.revision,"request":browser.request,"observed_target":browser.review})).map_err(|_| "could not encode browser authority")?;
        let destination = if origins.is_empty() {
            "local browser lifecycle".into()
        } else {
            origins.join(", ")
        };
        let recipient =
            RecipientIdentity::new(RecipientKind::Browser, "browser", destination, &identity)
                .map_err(|error| error.to_string())?;
        let items = vec![
            OutboundItem::new(
                OutboundDataClass::PromptText,
                "exact browser request and page revision",
                None,
                "explicit browser request",
                serde_json::to_vec(
                    &json!({"request":browser.request,"observed_target":browser.review}),
                )
                .map_err(|_| "could not encode browser request")?,
            )
            .map_err(|error| error.to_string())?,
        ];
        let review = OutboundRequest::new(OperationId::new(), recipient.clone(), "Dedicated exact-recipient browser; page behavior is not an effect sandbox; review exact action separately", items.clone()).map_err(|error| error.to_string())?.review();
        Ok(PlannedToolInvocation::new(
            final_arguments,
            PermissionScope::External {
                recipient_identity_digest: recipient.identity_digest.clone(),
                operation: "browser".into(),
            },
            Plan {
                browser,
                recipient,
                items,
            },
        )
        .with_outbound_review(review))
    }
    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<Plan>("browser")?;
            let request = OutboundRequest::new(context.operation_id, plan.recipient.clone(), "Dedicated exact-recipient browser; page behavior is not an effect sandbox; review exact action separately", plan.items.clone()).map_err(|error| error.to_string())?;
            let classes = BTreeSet::from([OutboundDataClass::PromptText]);
            let policy = OutboundPolicyLayers {
                connection_allowed: classes.clone(),
                user_ceiling: classes,
                profile_allowed: self.profile_egress.clone(),
                conversation_allowed: None,
            };
            let observer = crate::diagnostics::outbound_audit(self.owner.paths())
                .map_err(|error| error.to_string())?;
            let mut audit = ObservedOutboundAudit::new(observer.as_ref());
            let mut transport = Transport {
                owner: self.owner.clone(),
                plan: plan.browser.clone(),
                operation: context.operation_id,
            };
            let mut approval = context.outbound_approval;
            let result = OutboundGuard::open(self.owner.paths())
                .map_err(|error| error.to_string())?
                .dispatch(
                    request,
                    &policy,
                    approval.as_mut(),
                    &mut transport,
                    &mut audit,
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&result).map_err(|_| "could not encode browser receipt".into())
        })
    }
    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<OutboundDisposition>, String> {
        let plan = planned.executable::<Plan>("browser")?;
        OutboundGuard::open(self.owner.paths())
            .map_err(|error| error.to_string())?
            .disposition(&plan.recipient, plan.items.iter().map(OutboundItem::class))
            .map(Some)
            .map_err(|error| error.to_string())
    }
}
struct Transport {
    owner: BrowserOwner,
    plan: BrowserPlan,
    operation: OperationId,
}
impl OutboundTransport for Transport {
    type Receipt = BrowserReceipt;
    fn send<'a>(
        &'a mut self,
        _: &'a RecipientIdentity,
        _: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<BrowserReceipt, OutboundTransportFailure>> {
        Box::pin(async move {
            self.owner
                .execute(self.plan.clone(), self.operation)
                .await
                .map_err(|_| OutboundTransportFailure::Protocol)
        })
    }
}
