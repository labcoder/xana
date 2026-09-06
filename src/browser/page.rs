//! One browser document owns opaque references. Exact typed effects recheck the
//! observed element and URL in one fixed script before invoking its DOM method.

use super::{BrowserEffect, BrowserError, MAX_OBSERVATION_BYTES, cdp::Cdp, proxy::EgressPolicy};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_REFERENCES: usize = 128;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;
// Fixed private functions, never caller-supplied JavaScript. Absent attributes
// and visible labels are bound too: adding href/formaction after review is stale.
const DESCRIBE: &str = concat!(
    "function(){",
    include_str!("page/snapshot.js"),
    "return snapshot(this);}"
);
const ACTION: &str = concat!(
    "function(expected,url,kind,text){",
    include_str!("page/snapshot.js"),
    r#"
    // JSON object member order is not semantic. Rust may serialize maps in
    // sorted order; normalize both bounded snapshots before comparing them.
    function canonical(value) {
        if (Array.isArray(value)) return value.map(canonical);
        if (value && typeof value === 'object')
            return Object.keys(value).sort().map(key => [key, canonical(value[key])]);
        return value;
    }
    if (!this.isConnected || this.ownerDocument.location.href !== url ||
        JSON.stringify(canonical(snapshot(this))) !== JSON.stringify(canonical(expected))) return {stale:true};
    if (expected.unsupported || kind === 'click' && expected.form?.unsupported) return {unsupported:true};
    if (kind === 'fill') {
        if (!['INPUT','TEXTAREA'].includes(this.tagName)) return {unsupported:true};
        const prototype = this.tagName === 'INPUT' ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
        Object.getOwnPropertyDescriptor(prototype,'value').set.call(this,text);
        this.dispatchEvent(new Event('input',{bubbles:true}));
        this.dispatchEvent(new Event('change',{bubbles:true}));
    } else { HTMLElement.prototype.click.call(this); }
    return {dispatched:true};
}"#
);

#[derive(Clone, Debug, Serialize)]
pub(super) struct Reference {
    pub(super) id: String,
    pub(super) role: String,
    pub(super) label: String,
}
struct Target {
    backend: i64,
    epoch: u64,
    expected: Value,
    role: String,
    label: String,
}
#[derive(Serialize)]
pub(super) struct Observation {
    pub(super) url: String,
    pub(super) text: String,
    pub(super) references: Vec<Reference>,
    pub(super) truncated: bool,
}
pub(super) struct Page {
    connection: Cdp,
    session: String,
    policy: EgressPolicy,
    url: String,
    world: Option<(u64, i64)>,
    targets: BTreeMap<String, Target>,
}

impl Page {
    pub(super) fn preview(
        &self,
        reference: &str,
        effect: &BrowserEffect,
    ) -> Result<Value, BrowserError> {
        let target = self.targets.get(reference).ok_or(BrowserError::Stale)?;
        if target.epoch != self.epoch()? {
            return Err(BrowserError::Stale);
        }
        if matches!(effect, BrowserEffect::Click {})
            && target.expected["form"]["unsupported"] == true
        {
            return Err(BrowserError::InvalidInput);
        }
        if matches!(effect, BrowserEffect::Click {}) {
            for destination in [
                target.expected["destination"].as_str(),
                target.expected["form"]["action"].as_str(),
            ]
            .into_iter()
            .flatten()
            {
                if !self.policy.permits(destination) {
                    return Err(BrowserError::UnsupportedEgress);
                }
            }
        }
        Ok(
            json!({"url":self.url,"label":target.label,"role":target.role,"element":target.expected}),
        )
    }
    #[cfg(test)]
    pub(super) async fn mutate_fixture_target(&self) -> Result<(), BrowserError> {
        let document = self.call("DOM.getDocument", json!({"depth":1})).await?;
        let root = document["root"]["nodeId"]
            .as_i64()
            .ok_or(BrowserError::Protocol)?;
        let target = self
            .call(
                "DOM.querySelector",
                json!({"nodeId":root,"selector":"button"}),
            )
            .await?;
        let node = target["nodeId"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or(BrowserError::Protocol)?;
        self.call(
            "DOM.setAttributeValue",
            json!({"nodeId":node,"name":"formaction","value":"/changed-after-review"}),
        )
        .await?;
        Ok(())
    }
    #[cfg(test)]
    pub(super) async fn mutate_fixture_form(&self, mutation: &str) -> Result<(), BrowserError> {
        let document = self.call("DOM.getDocument", json!({"depth":1})).await?;
        let root = document["root"]["nodeId"]
            .as_i64()
            .ok_or(BrowserError::Protocol)?;
        let (selector, name, value) = match mutation {
            "action" => ("form", "action", "/changed-after-review"),
            "value" => ("#value", "value", "changed-after-review"),
            "secret" => (
                "input[type=password]",
                "value",
                "XANA_BROWSER_SECRET_CANARY",
            ),
            _ => return Err(BrowserError::InvalidInput),
        };
        let target = self
            .call(
                "DOM.querySelector",
                json!({"nodeId":root,"selector":selector}),
            )
            .await?;
        let node = target["nodeId"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or(BrowserError::Protocol)?;
        self.call(
            "DOM.setAttributeValue",
            json!({"nodeId":node,"name":name,"value":value}),
        )
        .await?;
        if mutation == "secret" {
            let form = self
                .call(
                    "DOM.querySelector",
                    json!({"nodeId":root,"selector":"form"}),
                )
                .await?;
            let form = form["nodeId"].as_i64().ok_or(BrowserError::Protocol)?;
            self.call(
                "DOM.setAttributeValue",
                json!({"nodeId":form,"name":"id","value":"fixture-form"}),
            )
            .await?;
            self.call(
                "DOM.setAttributeValue",
                json!({"nodeId":node,"name":"form","value":"fixture-form"}),
            )
            .await?;
        }
        Ok(())
    }
    pub(super) async fn start(connection: Cdp, policy: EgressPolicy) -> Result<Self, BrowserError> {
        let version = connection
            .call("Browser.getVersion", json!({}), None)
            .await?;
        // A browser update requires repeating its request/lifecycle qualification;
        // unsupported native targets stay unavailable, not silently weaker.
        if version["product"] != "Edg/152.0.4191.66" || version["protocolVersion"] != "1.3" {
            return Err(BrowserError::UnsupportedBrowser);
        }
        connection
            .call(
                "Browser.setDownloadBehavior",
                json!({"behavior":"deny"}),
                None,
            )
            .await?;
        connection
            .call(
                "Target.setAutoAttach",
                json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
                None,
            )
            .await?;
        let targets = connection
            .call("Target.getTargets", json!({}), None)
            .await?;
        let target = targets["targetInfos"]
            .as_array()
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|v| v["type"] == "page" && v["url"] == "about:blank")
            })
            .and_then(|v| v["targetId"].as_str())
            .ok_or(BrowserError::Protocol)?;
        let session = connection.ready_session(target).await?;
        Ok(Self {
            connection,
            session,
            policy,
            url: "about:blank".into(),
            world: None,
            targets: BTreeMap::new(),
        })
    }
    async fn call(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        self.connection
            .call(method, params, Some(&self.session))
            .await
    }
    pub(super) fn epoch(&self) -> Result<u64, BrowserError> {
        self.connection.epoch(&self.session)
    }
    pub(super) fn invalidate(&mut self) {
        self.targets.clear();
        self.world = None;
    }
    async fn prepare_world(&mut self, epoch: u64) -> Result<(), BrowserError> {
        let frame = self.call("Page.getFrameTree", json!({})).await?;
        let frame = frame["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or(BrowserError::Protocol)?;
        let world = self
            .call(
                "Page.createIsolatedWorld",
                json!({"frameId":frame,"worldName":"xana-typed-browser"}),
            )
            .await?;
        let context = world["executionContextId"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or(BrowserError::Protocol)?;
        if self.epoch()? != epoch {
            return Err(BrowserError::Stale);
        }
        self.world = Some((epoch, context));
        Ok(())
    }
    fn world(&self) -> Result<i64, BrowserError> {
        let epoch = self.epoch()?;
        self.world
            .filter(|(document, _)| *document == epoch)
            .map(|(_, id)| id)
            .ok_or(BrowserError::Stale)
    }
    pub(super) async fn navigate(&mut self, url: &str) -> Result<(), BrowserError> {
        if !self.policy.permits(url) {
            return Err(BrowserError::UnsupportedEgress);
        }
        self.invalidate();
        let response = self.call("Page.navigate", json!({"url":url})).await?;
        if response.get("errorText").is_some() {
            return Err(BrowserError::UnsupportedEgress);
        }
        let frame = response["frameId"].as_str().ok_or(BrowserError::Protocol)?;
        if let Some(loader) = response["loaderId"].as_str() {
            self.connection
                .ready_document(&self.session, frame, loader)
                .await?;
        }
        self.url = url.to_owned();
        Ok(())
    }
    async fn current_url(&self) -> Result<String, BrowserError> {
        let frame = self.call("Page.getFrameTree", json!({})).await?;
        let url = frame["frameTree"]["frame"]["url"]
            .as_str()
            .ok_or(BrowserError::Protocol)?;
        if !self.policy.permits(url) {
            return Err(BrowserError::UnsupportedEgress);
        }
        Ok(url.to_owned())
    }
    pub(super) async fn observe(&mut self) -> Result<Observation, BrowserError> {
        self.invalidate();
        self.url = self.current_url().await?;
        let epoch = self.epoch()?;
        self.prepare_world(epoch).await?;
        let response = self
            .call("Accessibility.getFullAXTree", json!({"depth":8}))
            .await?;
        let nodes = response["nodes"].as_array().ok_or(BrowserError::Protocol)?;
        let mut text = String::new();
        let mut references = Vec::new();
        let mut truncated = false;
        for node in nodes.iter().take(4096) {
            if node["ignored"] == true {
                continue;
            }
            let role = node["role"]["value"].as_str().unwrap_or("");
            let label = node["name"]["value"].as_str().unwrap_or("");
            if label.len() > MAX_LABEL_BYTES {
                truncated = true;
                continue;
            }
            if text.len().saturating_add(label.len() + role.len() + 4) > MAX_OBSERVATION_BYTES / 2 {
                truncated = true;
                break;
            }
            if !label.is_empty() {
                text.push_str(role);
                text.push_str(": ");
                text.push_str(label);
                text.push('\n');
            }
            if references.len() >= MAX_REFERENCES {
                truncated = true;
                continue;
            }
            if !matches!(
                role,
                "button" | "link" | "textbox" | "searchbox" | "checkbox" | "radio"
            ) {
                continue;
            }
            let Some(backend) = node["backendDOMNodeId"].as_i64().filter(|v| *v > 0) else {
                continue;
            };
            let expected = self.describe(backend).await?;
            if expected["unsupported"] == true {
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            self.targets.insert(
                id.clone(),
                Target {
                    backend,
                    epoch,
                    expected,
                    role: role.to_owned(),
                    label: label.to_owned(),
                },
            );
            references.push(Reference {
                id,
                role: role.to_owned(),
                label: label.to_owned(),
            });
        }
        if nodes.len() > 4096 {
            truncated = true;
        }
        if self.epoch()? != epoch {
            self.invalidate();
            return Err(BrowserError::Stale);
        }
        let result = Observation {
            url: self.url.clone(),
            text,
            references,
            truncated,
        };
        if serde_json::to_vec(&result)
            .map_err(|_| BrowserError::Protocol)?
            .len()
            > MAX_OBSERVATION_BYTES
        {
            self.invalidate();
            return Err(BrowserError::Limit);
        }
        Ok(result)
    }
    async fn describe(&self, backend: i64) -> Result<Value, BrowserError> {
        let resolved = self
            .call(
                "DOM.resolveNode",
                json!({"backendNodeId":backend,"objectGroup":"xana-browser-inspection","executionContextId":self.world()?}),
            )
            .await?;
        let object = resolved["object"]["objectId"]
            .as_str()
            .ok_or(BrowserError::Stale)?;
        let response = self
            .call(
                "Runtime.callFunctionOn",
                json!({"objectId":object,"functionDeclaration":DESCRIBE,"returnByValue":true}),
            )
            .await?;
        self.call(
            "Runtime.releaseObjectGroup",
            json!({"objectGroup":"xana-browser-inspection"}),
        )
        .await?;
        if response.get("exceptionDetails").is_some() {
            return Err(BrowserError::Stale);
        }
        let value = response["result"]["value"].clone();
        if !value.is_object() {
            return Err(BrowserError::Protocol);
        }
        if serde_json::to_vec(&value)
            .map_err(|_| BrowserError::Protocol)?
            .len()
            > 12 * 1024
        {
            return Err(BrowserError::Limit);
        }
        Ok(value)
    }
    pub(super) async fn act(
        &mut self,
        reference: &str,
        effect: &BrowserEffect,
    ) -> Result<(), BrowserError> {
        let target = self.targets.remove(reference).ok_or(BrowserError::Stale)?;
        if self.epoch()? != target.epoch || self.current_url().await? != self.url {
            self.invalidate();
            return Err(BrowserError::Stale);
        }
        if self.describe(target.backend).await? != target.expected {
            self.invalidate();
            return Err(BrowserError::Stale);
        }
        let resolved = self
            .call(
                "DOM.resolveNode",
                json!({"backendNodeId":target.backend,"objectGroup":"xana-browser-action","executionContextId":self.world()?}),
            )
            .await?;
        let object = resolved["object"]["objectId"]
            .as_str()
            .ok_or(BrowserError::Stale)?;
        let (kind, text) = match effect {
            BrowserEffect::Click {} => ("click", ""),
            BrowserEffect::Fill { text } => ("fill", text.as_str()),
        };
        let result = self.call("Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":ACTION,"arguments":[{"value":target.expected},{"value":self.url},{"value":kind},{"value":text}],"returnByValue":true,"userGesture":true})).await;
        self.invalidate();
        let result = result?;
        if result["result"]["value"]["stale"] == true {
            return Err(BrowserError::Stale);
        }
        if result["result"]["value"]["unsupported"] == true {
            return Err(BrowserError::InvalidInput);
        }
        if result["result"]["value"]["dispatched"] != true
            || result.get("exceptionDetails").is_some()
        {
            return Err(BrowserError::Uncertain);
        }
        self.call(
            "Runtime.releaseObjectGroup",
            json!({"objectGroup":"xana-browser-action"}),
        )
        .await?;
        Ok(())
    }
    pub(super) async fn screenshot(&self) -> Result<Vec<u8>, BrowserError> {
        self.current_url().await?;
        // A popup may have activated a different tab. The requested evidence
        // belongs to this owned page; activate its surface before capture.
        self.call("Page.bringToFront", json!({})).await?;
        let response = self.call("Page.captureScreenshot", json!({"format":"png","captureBeyondViewport":false,"clip":{"x":0,"y":0,"width":1280,"height":720,"scale":1}})).await?;
        let encoded = response["data"].as_str().ok_or(BrowserError::Protocol)?;
        if encoded.len() > MAX_SCREENSHOT_BYTES * 4 / 3 + 4 {
            return Err(BrowserError::Limit);
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| BrowserError::Protocol)?;
        if bytes.len() > MAX_SCREENSHOT_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(BrowserError::Limit);
        }
        Ok(bytes)
    }
}
