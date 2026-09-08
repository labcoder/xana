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

const REQUEST_FIELDS: &[(&str, &[&str])] = &[
    ("open", &["url"]),
    ("launch", &["origins"]),
    ("navigate", &["url"]),
    ("observe", &[]),
    ("screenshot", &[]),
    ("act", &["reference", "effect", "purpose"]),
    ("takeover", &[]),
    ("resume", &[]),
    ("close", &[]),
];

fn parameters() -> Value {
    let fields = json!({
        "op":{"type":"string","enum":REQUEST_FIELDS.iter().map(|(op,_)|op).collect::<Vec<_>>()},
        "origins":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","maxLength":1024},"description":"Only launch: exact HTTPS origins, not page URLs."},
        "url":{"type":"string","maxLength":2048,"description":"Only open or navigate: the requested HTTPS page URL."},
        "reference":{"type":"string","format":"uuid","description":"Only act: a current observation reference."},
        "effect":{"type":"object","oneOf":[
            {"additionalProperties":false,"required":["kind"],"properties":{"kind":{"const":"click"}}},
            {"additionalProperties":false,"required":["kind","text"],"properties":{"kind":{"const":"fill"},"text":{"type":"string","maxLength":4096}}}
        ]},
        "purpose":{"type":"string","minLength":1,"maxLength":1024,"description":"Required ONLY for act. Omit for open, launch, navigate, observe, screenshot, takeover, resume and close."}
    });
    let variants: Vec<_> = REQUEST_FIELDS
        .iter()
        .map(|(op, names)| {
            let mut properties = serde_json::Map::new();
            properties.insert("op".into(), json!({"const":op}));
            let mut required = vec!["op"];
            for name in *names {
                properties.insert((*name).into(), fields[*name].clone());
                required.push(name);
            }
            json!({"additionalProperties":false,"required":required,"properties":properties})
        })
        .collect();
    json!({"type":"object","additionalProperties":false,"required":["op"],"properties":fields,"oneOf":variants})
}

fn parse_request(arguments: &Value) -> Result<BrowserRequest, String> {
    serde_json::from_value(arguments.clone()).map_err(|_| {
        match REQUEST_FIELDS.iter().find(|(op, _)| arguments["op"] == *op) {
            Some((op, fields)) => format!("browser {op} requires exactly op{}; omit fields for other operations. No browser action occurred.", if fields.is_empty() { String::new() } else { format!(" and {}", fields.join(", ")) }),
            None => "browser requires a supported op; use {\"op\":\"open\",\"url\":\"https://example.com/\"} to open and read a page. No browser action occurred.".into(),
        }
    })
}

impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser".into(),
            contract_version: crate::operation::TOOL_CONTRACT_VERSION,
            description: format!(
                "Read or control an optional dedicated local browser. START with {{\"op\":\"open\",\"url\":\"https://example.com/\"}}: after approval it launches if closed, navigates and returns bounded untrusted page content and references. It leaves the browser open. Open accepts only op and url, never purpose. Existing sessions keep their reviewed origins and budgets; close before requesting another origin. launch selects origins without opening a page; navigate and observe require an existing session. screenshot returns a protected artifact. Only act requires a current reference, effect and meaningful purpose; a click is not blanket consent to purchase/publish/send/delete. takeover suspends automation for manual control/login, resume reinspects, close cleans up. Password inputs require manual takeover; file uploads are unsupported. {EGRESS_DISCLOSURE}"
            ),
            parameters: parameters(),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }
    fn plan(&self, arguments: &Value, _workspace: &Path) -> Result<PlannedToolInvocation, String> {
        let request = parse_request(arguments)?;
        let browser = self
            .owner
            .plan(request)
            .map_err(|error| error.to_string())?;
        let snapshot = self.owner.snapshot();
        let origins = match &browser.request {
            BrowserRequest::Launch { origins } => origins.clone(),
            BrowserRequest::Open { url } if snapshot.task.is_none() => {
                vec![super::session::open_origin(url).map_err(|error| error.to_string())?]
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_schemas_match_the_parser_and_require_only_their_own_fields() {
        let schema = parameters();
        let samples = [
            json!({"op":"open","url":"https://example.com/"}),
            json!({"op":"launch","origins":["https://example.com"]}),
            json!({"op":"navigate","url":"https://example.com/page"}),
            json!({"op":"observe"}),
            json!({"op":"screenshot"}),
            json!({"op":"act","reference":"c7b6ad09-a32a-4c9b-b1ed-eaabed50cb14","effect":{"kind":"click"},"purpose":"approved button"}),
            json!({"op":"takeover"}),
            json!({"op":"resume"}),
            json!({"op":"close"}),
        ];
        for (sample, variant) in samples.iter().zip(schema["oneOf"].as_array().unwrap()) {
            let parsed = parse_request(sample).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), *sample);
            assert_eq!(sample["op"], variant["properties"]["op"]["const"]);
            assert_eq!(variant["additionalProperties"], false);
            let keys: Vec<_> = sample.as_object().unwrap().keys().collect();
            assert_eq!(
                keys,
                variant["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            );
            for key in keys {
                assert!(
                    variant["required"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(key))
                );
            }
        }
    }

    #[test]
    fn wrong_operation_fields_get_actionable_guidance_without_silent_dropping() {
        let error = parse_request(
            &json!({"op":"open","url":"https://example.com/secret","purpose":"read page"}),
        )
        .unwrap_err();
        assert!(error.contains("exactly op and url"));
        assert!(!error.contains("secret"));
        assert!(
            parse_request(&json!({"op":"launch","url":"https://example.com/"}))
                .unwrap_err()
                .contains("origins")
        );
        assert!(parse_request(&json!({"op":"observe","script":"1+1"})).is_err());
        for effect in [
            json!({"kind":"fill"}),
            json!({"kind":"click","text":"unexpected"}),
        ] {
            assert!(parse_request(&json!({"op":"act","reference":"c7b6ad09-a32a-4c9b-b1ed-eaabed50cb14","effect":effect,"purpose":"fill"})).is_err());
        }
        assert!(parse_request(&json!({"op":"act","reference":"c7b6ad09-a32a-4c9b-b1ed-eaabed50cb14","effect":{"kind":"fill","text":"value"},"purpose":"fill"})).is_ok());
    }
}
