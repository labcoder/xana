//! Bounded A2A 1.0 task delegation behind Xana's ordinary tool and egress gates.

use super::{A2A_PROTOCOL_VERSION, A2aError, ExternalAgentManager};
use crate::{
    artifact::{ArtifactRecord, ArtifactStore, MAX_ARTIFACT_BYTES},
    config::{ConnectionRegistry, CredentialReference, OutboundDataClass},
    credential::{CredentialResolver, SecretString},
    identity::{OperationId, PrincipalId},
    mcp::{McpHttpEndpoint, McpHttpSecurity, pinned_client},
    native_runtime::{AgentEvent, AgentEventSender},
    outbound::{
        ObservedOutboundAudit, OutboundAuditObserver, OutboundGuard, OutboundItem,
        OutboundPolicyLayers, OutboundRequest, OutboundTransport, OutboundTransportFailure,
        RecipientIdentity, RecipientKind,
    },
    paths::XanaPaths,
    private_state::ExternalAgentStateRecord,
    sse::SseDecoder,
    tool::{
        EffectClass, PlannedToolInvocation, ReplaySafety, Tool, ToolDefinition,
        ToolExecutionContext, ToolRegistry,
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{StreamExt, future::BoxFuture};
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_TASK_BYTES: usize = 64 * 1024;
const MAX_SELECTED_MESSAGES: usize = 16;
const MAX_SELECTED_FILES: usize = 16;
const MAX_SELECTED_ARTIFACTS: usize = 16;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_RETAINED_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_EVENTS: usize = 256;
const MAX_REMOTE_ARTIFACTS: usize = 64;
const MAX_SAFE_REFERENCES: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExternalAgentActivityKind {
    Sending {
        classes: Vec<OutboundDataClass>,
        total_bytes: usize,
    },
    TaskIdentified {
        task_id: String,
        context_id: Option<String>,
    },
    Status {
        state: String,
        message: Option<String>,
    },
    Message {
        text: String,
    },
    Artifact {
        name: String,
        media_type: String,
        byte_len: u64,
    },
    CancellationRequested {
        task_id: String,
    },
    Detached {
        task_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalAgentActivity {
    pub(crate) connection: String,
    pub(crate) agent_name: String,
    pub(crate) ownership: String,
    pub(crate) activity: ExternalAgentActivityKind,
}

pub(crate) struct A2aDelegationActivation<'a> {
    pub(crate) paths: &'a XanaPaths,
    pub(crate) workspace: &'a Path,
    pub(crate) external_agents: &'a [String],
    pub(crate) profile_egress: &'a [OutboundDataClass],
    pub(crate) artifacts: ArtifactStore,
    pub(crate) owner: PrincipalId,
}

pub(crate) fn activate_profile_delegation_tools(
    registry: &ConnectionRegistry,
    activation: A2aDelegationActivation<'_>,
    tools: &mut ToolRegistry,
) -> anyhow::Result<usize> {
    if activation.external_agents.is_empty() {
        return Ok(0);
    }
    let manager = ExternalAgentManager::open(activation.paths);
    let mut count = 0;
    for name in activation.external_agents {
        let declaration = registry
            .external_agents
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("profile references missing external agent {name:?}"))?;
        let trusted = manager
            .trusted_record(name, declaration)
            .map_err(anyhow::Error::new)?;
        let connection_allowed = declaration
            .egress_policy
            .as_deref()
            .and_then(|policy| registry.egress_policies.get(policy))
            .map(|policy| policy.allowed.iter().copied().collect())
            .unwrap_or_default();
        let all = OutboundDataClass::ALL.into_iter().collect();
        let policy = OutboundPolicyLayers {
            connection_allowed,
            user_ceiling: all,
            profile_allowed: activation.profile_egress.iter().copied().collect(),
            conversation_allowed: None,
        };
        tools.register(A2aDelegationTool {
            connection: name.clone(),
            declaration_credential: declaration.credential.clone(),
            trusted,
            paths: activation.paths.clone(),
            workspace: activation.workspace.to_owned(),
            artifacts: activation.artifacts.clone(),
            owner: activation.owner,
            policy,
            security: McpHttpSecurity::default(),
            audit: crate::diagnostics::outbound_audit(activation.paths)?,
        })?;
        count += 1;
    }
    Ok(count)
}

pub(crate) struct A2aDelegationTool {
    connection: String,
    declaration_credential: Option<CredentialReference>,
    trusted: ExternalAgentStateRecord,
    paths: XanaPaths,
    workspace: PathBuf,
    artifacts: ArtifactStore,
    owner: PrincipalId,
    policy: OutboundPolicyLayers,
    security: McpHttpSecurity,
    audit: Arc<dyn OutboundAuditObserver>,
}

impl A2aDelegationTool {
    #[cfg(test)]
    fn allow_loopback(mut self) -> Self {
        self.security.allow_loopback_http = true;
        self
    }

    fn recipient(&self) -> RecipientIdentity {
        RecipientIdentity {
            kind: RecipientKind::ExternalAgent,
            connection: self.connection.clone(),
            destination: self.trusted.interface_url.clone(),
            identity_digest: self.trusted.identity_digest.clone(),
        }
    }

    fn tool_name(&self) -> String {
        format!("a2a__{}__delegate", self.connection)
    }
}

pub(crate) async fn cancel_tracked_task(
    manager: &ExternalAgentManager,
    connection: &str,
    declaration: &crate::config::ExternalAgentDeclaration,
    task_id: &str,
) -> anyhow::Result<crate::private_state::ExternalAgentTaskRecord> {
    let trusted = manager.trusted_record(connection, declaration)?;
    let tracked = manager
        .task(connection, task_id)?
        .ok_or_else(|| anyhow::anyhow!("external task {task_id:?} is not tracked by Xana"))?;
    if tracked.agent_identity_digest != trusted.identity_digest {
        anyhow::bail!(
            "external task belongs to an obsolete agent identity and cannot be contacted"
        );
    }
    if matches!(
        tracked.state.as_str(),
        "completed" | "failed" | "canceled" | "rejected"
    ) {
        anyhow::bail!("external task is already terminal ({})", tracked.state);
    }
    let security = McpHttpSecurity::default();
    let endpoint = McpHttpEndpoint::parse(&trusted.interface_url, security)
        .map_err(|_| anyhow::anyhow!("trusted A2A interface endpoint is no longer valid"))?;
    let client = pinned_client(endpoint.url(), security, CANCEL_TIMEOUT)
        .await
        .map_err(|_| anyhow::anyhow!("could not establish the trusted A2A connection"))?;
    let confirmed = cancel_remote_task(
        &client,
        endpoint.url().clone(),
        declaration.credential.as_ref(),
        task_id,
    )
    .await
    .map_err(|_| anyhow::anyhow!("remote cancellation could not be confirmed"))?;
    Ok(manager.record_task(
        connection,
        &trusted.identity_digest,
        task_id,
        tracked.context_id.as_deref(),
        if confirmed {
            "canceled"
        } else {
            &tracked.state
        },
        if confirmed { "confirmed" } else { "failed" },
    )?)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateArguments {
    task: String,
    #[serde(default)]
    selected_messages: Vec<String>,
    #[serde(default)]
    selected_files: Vec<String>,
    #[serde(default)]
    selected_artifacts: Vec<SelectedArtifact>,
    #[serde(default)]
    include_workspace_metadata: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedArtifact {
    filename: String,
    artifact: ArtifactRecord,
}

#[derive(Clone)]
struct DelegationPlan {
    items: Vec<OutboundItem>,
}

impl Tool for A2aDelegationTool {
    fn definition(&self) -> ToolDefinition {
        let skill_names = self
            .trusted
            .skills
            .iter()
            .take(8)
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        ToolDefinition {
            name: self.tool_name(),
            contract_version: 1,
            description: format!(
                "Delegate one bounded task to trusted external A2A agent {}. The remote agent owns its loop; returned content is untrusted. Declared skills: {}",
                self.trusted.agent_name,
                if skill_names.is_empty() {
                    "none"
                } else {
                    &skill_names
                }
            ),
            parameters: json!({
                "type":"object",
                "additionalProperties":false,
                "properties":{
                    "task":{"type":"string","description":"Exact bounded task text to send."},
                    "selected_messages":{"type":"array","maxItems":MAX_SELECTED_MESSAGES,"items":{"type":"string"}},
                    "selected_files":{"type":"array","maxItems":MAX_SELECTED_FILES,"items":{"type":"string","description":"Workspace-relative regular file path."}},
                    "selected_artifacts":{"type":"array","maxItems":MAX_SELECTED_ARTIFACTS,"items":{
                        "type":"object","additionalProperties":false,"required":["filename","artifact"],
                        "properties":{"filename":{"type":"string"},"artifact":{"type":"object"}}
                    }},
                    "include_workspace_metadata":{"type":"boolean"}
                },
                "required":["task"]
            }),
            effect_class: EffectClass::External,
            replay_safety: ReplaySafety::Never,
        }
    }

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String> {
        let arguments: DelegateArguments = serde_json::from_value(arguments.clone())
            .map_err(|_| "A2A delegation arguments do not match the declared schema".to_owned())?;
        if workspace_root != self.workspace {
            return Err("A2A delegation workspace does not match the frozen profile".into());
        }
        validate_task_text(&arguments.task)?;
        if arguments.selected_messages.len() > MAX_SELECTED_MESSAGES
            || arguments.selected_files.len() > MAX_SELECTED_FILES
            || arguments.selected_artifacts.len() > MAX_SELECTED_ARTIFACTS
        {
            return Err("A2A delegation selection exceeds its item bound".into());
        }
        let mut items = vec![
            OutboundItem::new(
                OutboundDataClass::PromptText,
                "delegated task",
                None,
                "current agent tool request",
                arguments.task.into_bytes(),
            )
            .map_err(|error| error.to_string())?,
        ];
        for (index, message) in arguments.selected_messages.into_iter().enumerate() {
            validate_message_text(&message)?;
            items.push(
                OutboundItem::new(
                    OutboundDataClass::SelectedMessages,
                    format!("selected message {}", index + 1),
                    None,
                    "explicit delegation selection",
                    message.into_bytes(),
                )
                .map_err(|error| error.to_string())?,
            );
        }
        for source in arguments.selected_files {
            let (canonical, bytes) = read_workspace_file(workspace_root, &source)?;
            items.push(
                OutboundItem::new(
                    OutboundDataClass::SelectedFileContents,
                    format!("selected file {source}"),
                    Some(source),
                    canonical.display().to_string(),
                    bytes,
                )
                .map_err(|error| error.to_string())?,
            );
        }
        for selected in arguments.selected_artifacts {
            validate_filename(&selected.filename)?;
            let bytes = self
                .artifacts
                .read_bounded(&selected.artifact, MAX_ARTIFACT_BYTES)
                .map_err(|error| format!("selected artifact is unavailable: {error}"))?;
            items.push(
                OutboundItem::new(
                    OutboundDataClass::SelectedArtifacts,
                    format!(
                        "selected artifact {} ({})",
                        selected.filename, selected.artifact.media_type
                    ),
                    Some(selected.filename),
                    format!(
                        "Xana artifact {}",
                        selected.artifact.reference.content_hash.as_str()
                    ),
                    bytes,
                )
                .map_err(|error| error.to_string())?,
            );
        }
        if arguments.include_workspace_metadata {
            let metadata = serde_json::to_vec(&json!({
                "workspace_name": workspace_root.file_name().and_then(|name| name.to_str()),
                "operating_system": std::env::consts::OS,
            }))
            .map_err(|_| "workspace metadata could not be encoded".to_owned())?;
            items.push(
                OutboundItem::new(
                    OutboundDataClass::WorkspaceMetadata,
                    "bounded workspace metadata",
                    None,
                    "frozen conversation workspace",
                    metadata,
                )
                .map_err(|error| error.to_string())?,
            );
        }
        let final_arguments = json!({
            "recipient":self.connection,
            "classes":items.iter().map(|item| item.class().as_str()).collect::<Vec<_>>(),
            "items":items.len(),
            "bytes":items.iter().map(|item| item.bytes().len()).sum::<usize>(),
        });
        let review = OutboundRequest::new(
            OperationId::new(),
            self.recipient(),
            format!("delegate task to {}", self.trusted.agent_name),
            items.clone(),
        )
        .map_err(|error| error.to_string())?
        .review();
        Ok(PlannedToolInvocation::new(
            final_arguments,
            crate::permission::PermissionScope::External {
                recipient_identity_digest: self.trusted.identity_digest.clone(),
                operation: self.tool_name(),
            },
            DelegationPlan { items },
        )
        .with_outbound_review(review))
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let plan = planned.executable::<DelegationPlan>(&self.tool_name())?;
            let cancellation = CancelOnDrop(CancellationToken::new());
            let classes = plan
                .items
                .iter()
                .map(OutboundItem::class)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            emit(
                context.events.as_ref(),
                context.operation_id,
                &self.connection,
                &self.trusted.agent_name,
                ExternalAgentActivityKind::Sending {
                    classes,
                    total_bytes: plan.items.iter().map(|item| item.bytes().len()).sum(),
                },
            );
            let request = OutboundRequest::new(
                context.operation_id,
                self.recipient(),
                format!("delegate task to {}", self.trusted.agent_name),
                plan.items.clone(),
            )
            .map_err(|error| error.to_string())?;
            let mut controller = context.outbound_approval;
            let mut audit = ObservedOutboundAudit::new(self.audit.as_ref());
            let mut transport = A2aSendTransport {
                interface_url: self.trusted.interface_url.clone(),
                security: self.security,
                credential_reference: self.declaration_credential.clone(),
                recipient_digest: self.trusted.identity_digest.clone(),
                connection: self.connection.clone(),
                agent_name: self.trusted.agent_name.clone(),
                manager: ExternalAgentManager::open(&self.paths),
                artifacts: self.artifacts.clone(),
                owner: self.owner,
                streaming: self.trusted.streaming,
                cancellation: cancellation.0.clone(),
                events: context.events,
                operation_id: context.operation_id,
            };
            let receipt = OutboundGuard::open(&self.paths)
                .map_err(|error| error.to_string())?
                .dispatch(
                    request,
                    &self.policy,
                    controller.as_mut(),
                    &mut transport,
                    &mut audit,
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&receipt)
                .map_err(|_| "A2A delegation result could not be encoded".to_owned())
        })
    }

    fn outbound_disposition(
        &self,
        planned: &PlannedToolInvocation,
    ) -> Result<Option<crate::outbound::OutboundDisposition>, String> {
        let plan = planned.executable::<DelegationPlan>(&self.tool_name())?;
        OutboundGuard::open(&self.paths)
            .map_err(|error| error.to_string())?
            .disposition(
                &self.recipient(),
                plan.items.iter().map(OutboundItem::class),
            )
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct A2aSendTransport {
    interface_url: String,
    security: McpHttpSecurity,
    credential_reference: Option<CredentialReference>,
    recipient_digest: String,
    connection: String,
    agent_name: String,
    manager: ExternalAgentManager,
    artifacts: ArtifactStore,
    owner: PrincipalId,
    streaming: bool,
    cancellation: CancellationToken,
    events: Option<AgentEventSender>,
    operation_id: OperationId,
}

impl OutboundTransport for A2aSendTransport {
    type Receipt = A2aDelegationReceipt;

    fn send<'a>(
        &'a mut self,
        recipient: &'a RecipientIdentity,
        items: &'a [OutboundItem],
    ) -> BoxFuture<'a, Result<Self::Receipt, OutboundTransportFailure>> {
        Box::pin(async move {
            if recipient.kind != RecipientKind::ExternalAgent
                || recipient.identity_digest != self.recipient_digest
                || items.is_empty()
            {
                return Err(OutboundTransportFailure::Rejected);
            }
            let endpoint = McpHttpEndpoint::parse(&self.interface_url, self.security)
                .map_err(|_| OutboundTransportFailure::Rejected)?;
            let client = pinned_client(endpoint.url(), self.security, REQUEST_TIMEOUT)
                .await
                .map_err(|_| OutboundTransportFailure::Unavailable)?;
            let credential = self
                .credential_reference
                .as_ref()
                .map(|reference| CredentialResolver::default().resolve(reference))
                .transpose()
                .map_err(|_| OutboundTransportFailure::Rejected)?;
            self.send_items(items, &client, endpoint.url(), credential.as_ref())
                .await
                .map_err(A2aWireError::classify)
        })
    }
}

impl A2aSendTransport {
    async fn send_items(
        &mut self,
        items: &[OutboundItem],
        client: &reqwest::Client,
        endpoint: &reqwest::Url,
        credential: Option<&SecretString>,
    ) -> Result<A2aDelegationReceipt, A2aWireError> {
        let parts = outbound_parts(items)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let method = if self.streaming {
            "SendStreamingMessage"
        } else {
            "SendMessage"
        };
        let body = json!({
            "jsonrpc":"2.0",
            "id":request_id,
            "method":method,
            "params":{
                "message":{
                    "messageId":uuid::Uuid::new_v4().to_string(),
                    "role":"ROLE_USER",
                    "parts":parts,
                },
                "configuration":{"acceptedOutputModes":["text/plain"]}
            }
        });
        let mut request = client
            .post(endpoint.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .header("A2A-Version", A2A_PROTOCOL_VERSION)
            .header(
                header::ACCEPT,
                if self.streaming {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .json(&body);
        if let Some(credential) = credential {
            request = request.bearer_auth(credential.expose());
        }
        let response = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(A2aWireError::Cancelled),
            response = request.send() => response.map_err(classify_reqwest)?,
        };
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(A2aWireError::Unauthorized);
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(A2aWireError::RateLimited),
            status if status.is_redirection() => return Err(A2aWireError::Protocol),
            status if !status.is_success() => return Err(A2aWireError::Http),
            _ => {}
        }
        let mut collector = DelegationCollector::new(
            self.connection.clone(),
            self.agent_name.clone(),
            self.recipient_digest.clone(),
            self.manager.clone(),
            self.artifacts.clone(),
            self.owner,
            self.events.clone(),
            self.operation_id,
        );
        let mut cancel_guard = RemoteCancelGuard::new(
            client.clone(),
            endpoint.clone(),
            self.credential_reference.clone(),
            self.connection.clone(),
            self.recipient_digest.clone(),
            self.manager.clone(),
            self.events.clone(),
            self.operation_id,
            self.agent_name.clone(),
        );
        let media_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim);
        match media_type {
            Some("text/event-stream") if self.streaming => {
                let mut stream = response.bytes_stream();
                let mut decoder = SseDecoder::default();
                let mut total = 0_usize;
                let mut events = 0_usize;
                loop {
                    let next = tokio::select! {
                        biased;
                        _ = self.cancellation.cancelled() => {
                            cancel_guard.set_task(collector.task_id());
                            return Err(A2aWireError::Cancelled);
                        }
                        next = stream.next() => next,
                    };
                    let Some(chunk) = next else { break };
                    let chunk = chunk.map_err(classify_reqwest)?;
                    total = total.saturating_add(chunk.len());
                    if total > MAX_RESPONSE_BYTES {
                        return Err(A2aWireError::TooLarge);
                    }
                    for event in decoder.push(&chunk).map_err(|_| A2aWireError::Protocol)? {
                        events = events.saturating_add(1);
                        if events > MAX_STREAM_EVENTS {
                            return Err(A2aWireError::TooManyEvents);
                        }
                        collector.consume_envelope(&event.data, &request_id)?;
                        cancel_guard.set_task(collector.task_id());
                    }
                }
                decoder.finish().map_err(|_| A2aWireError::Protocol)?;
            }
            Some("application/json" | "application/a2a+json") if !self.streaming => {
                let bytes = collect_body(response, &self.cancellation).await?;
                collector.consume_envelope(&bytes, &request_id)?;
                cancel_guard.set_task(collector.task_id());
            }
            _ => return Err(A2aWireError::MediaType),
        }
        let receipt = collector.finish()?;
        cancel_guard.disarm();
        Ok(receipt)
    }
}

fn outbound_parts(items: &[OutboundItem]) -> Result<Vec<Value>, A2aWireError> {
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        match item.class() {
            OutboundDataClass::PromptText
            | OutboundDataClass::XanaSummary
            | OutboundDataClass::SelectedMessages => {
                let text = std::str::from_utf8(item.bytes()).map_err(|_| A2aWireError::Protocol)?;
                parts.push(json!({"text":text}));
            }
            OutboundDataClass::WorkspaceMetadata => {
                let data: Value =
                    serde_json::from_slice(item.bytes()).map_err(|_| A2aWireError::Protocol)?;
                parts.push(json!({"data":data,"mediaType":"application/json"}));
            }
            OutboundDataClass::SelectedFileContents | OutboundDataClass::SelectedArtifacts => {
                parts.push(json!({
                    "raw":STANDARD.encode(item.bytes()),
                    "filename":item.reference().unwrap_or(item.label()),
                    "mediaType":"application/octet-stream",
                }));
            }
        }
    }
    Ok(parts)
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct A2aDelegationReceipt {
    ownership: &'static str,
    connection: String,
    agent_name: String,
    agent_identity_digest: String,
    task_id: Option<String>,
    context_id: Option<String>,
    terminal_state: String,
    remote_cancel: String,
    messages: Vec<String>,
    artifacts: Vec<IngestedArtifact>,
    safe_references: Vec<SafeExternalReference>,
    untrusted: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct IngestedArtifact {
    remote_artifact_id: String,
    name: String,
    source_connection: String,
    source_task_id: Option<String>,
    record: ArtifactRecord,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SafeExternalReference {
    remote_artifact_id: String,
    name: String,
    url: String,
    media_type: String,
    source_connection: String,
    source_task_id: Option<String>,
}

struct DelegationCollector {
    connection: String,
    agent_name: String,
    identity_digest: String,
    manager: ExternalAgentManager,
    artifacts: ArtifactStore,
    owner: PrincipalId,
    events: Option<AgentEventSender>,
    operation_id: OperationId,
    task_id: Option<String>,
    context_id: Option<String>,
    state: String,
    messages: Vec<String>,
    message_bytes: usize,
    ingested: Vec<IngestedArtifact>,
    references: Vec<SafeExternalReference>,
    pending: BTreeMap<String, PendingArtifact>,
    artifact_bytes: usize,
    direct_message: bool,
}

struct PendingArtifact {
    remote_id: String,
    name: String,
    media_type: String,
    bytes: Vec<u8>,
}

impl DelegationCollector {
    #[allow(clippy::too_many_arguments)]
    fn new(
        connection: String,
        agent_name: String,
        identity_digest: String,
        manager: ExternalAgentManager,
        artifacts: ArtifactStore,
        owner: PrincipalId,
        events: Option<AgentEventSender>,
        operation_id: OperationId,
    ) -> Self {
        Self {
            connection,
            agent_name,
            identity_digest,
            manager,
            artifacts,
            owner,
            events,
            operation_id,
            task_id: None,
            context_id: None,
            state: "submitted".into(),
            messages: Vec::new(),
            message_bytes: 0,
            ingested: Vec::new(),
            references: Vec::new(),
            pending: BTreeMap::new(),
            artifact_bytes: 0,
            direct_message: false,
        }
    }

    fn task_id(&self) -> Option<String> {
        self.task_id.clone()
    }

    fn consume_envelope(&mut self, bytes: &[u8], expected_id: &str) -> Result<(), A2aWireError> {
        let envelope: Value = serde_json::from_slice(bytes).map_err(|_| A2aWireError::Protocol)?;
        if envelope.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || envelope.get("id").and_then(Value::as_str) != Some(expected_id)
        {
            return Err(A2aWireError::Protocol);
        }
        if envelope.get("error").is_some() {
            return Err(A2aWireError::Remote);
        }
        let result = envelope
            .get("result")
            .and_then(Value::as_object)
            .ok_or(A2aWireError::Protocol)?;
        let variants = ["task", "message", "statusUpdate", "artifactUpdate"]
            .into_iter()
            .filter(|key| result.contains_key(*key))
            .collect::<Vec<_>>();
        if variants.len() != 1 {
            return Err(A2aWireError::Protocol);
        }
        match variants[0] {
            "task" => self.consume_task(&result["task"]),
            "message" => {
                self.direct_message = true;
                self.state = "completed".into();
                self.consume_message(&result["message"])?;
                Ok(())
            }
            "statusUpdate" => self.consume_status_update(&result["statusUpdate"]),
            "artifactUpdate" => self.consume_artifact_update(&result["artifactUpdate"]),
            _ => Err(A2aWireError::Protocol),
        }
    }

    fn consume_task(&mut self, value: &Value) -> Result<(), A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        self.set_task_identity(
            required_id(object.get("id"))?,
            optional_id(object.get("contextId"))?,
        )?;
        self.consume_status(object.get("status").ok_or(A2aWireError::Protocol)?)?;
        if let Some(artifacts) = object.get("artifacts") {
            for artifact in artifacts.as_array().ok_or(A2aWireError::Protocol)? {
                self.consume_artifact(artifact, false, true)?;
            }
        }
        Ok(())
    }

    fn consume_status_update(&mut self, value: &Value) -> Result<(), A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        self.set_task_identity(
            required_id(object.get("taskId"))?,
            optional_id(object.get("contextId"))?,
        )?;
        self.consume_status(object.get("status").ok_or(A2aWireError::Protocol)?)
    }

    fn consume_artifact_update(&mut self, value: &Value) -> Result<(), A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        self.set_task_identity(
            required_id(object.get("taskId"))?,
            optional_id(object.get("contextId"))?,
        )?;
        let append = object
            .get("append")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let finalize = object
            .get("lastChunk")
            .map(|value| value.as_bool().ok_or(A2aWireError::Protocol))
            .transpose()?
            .unwrap_or(!append);
        self.consume_artifact(
            object.get("artifact").ok_or(A2aWireError::Protocol)?,
            append,
            finalize,
        )
    }

    fn set_task_identity(
        &mut self,
        task_id: String,
        context_id: Option<String>,
    ) -> Result<(), A2aWireError> {
        if self
            .task_id
            .as_ref()
            .is_some_and(|current| current != &task_id)
            || self
                .context_id
                .as_ref()
                .zip(context_id.as_ref())
                .is_some_and(|(current, incoming)| current != incoming)
        {
            return Err(A2aWireError::Protocol);
        }
        let is_new = self.task_id.is_none();
        self.task_id = Some(task_id.clone());
        if context_id.is_some() {
            self.context_id = context_id.clone();
        }
        if is_new {
            emit(
                self.events.as_ref(),
                self.operation_id,
                &self.connection,
                &self.agent_name,
                ExternalAgentActivityKind::TaskIdentified {
                    task_id: task_id.clone(),
                    context_id: context_id.clone(),
                },
            );
            self.record_state("submitted", "not_requested")?;
        }
        Ok(())
    }

    fn consume_status(&mut self, value: &Value) -> Result<(), A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        let wire = object
            .get("state")
            .and_then(Value::as_str)
            .ok_or(A2aWireError::Protocol)?;
        let state = normalize_task_state(wire)?;
        self.state = state.to_owned();
        let message = object
            .get("message")
            .map(|message| self.consume_message(message))
            .transpose()?
            .flatten();
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::Status {
                state: state.to_owned(),
                message: message.map(|value| preview(&value, 8192)),
            },
        );
        self.record_state(state, "not_requested")
    }

    fn consume_message(&mut self, value: &Value) -> Result<Option<String>, A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        if object.get("role").and_then(Value::as_str) != Some("ROLE_AGENT") {
            return Err(A2aWireError::Protocol);
        }
        let parts = object
            .get("parts")
            .and_then(Value::as_array)
            .ok_or(A2aWireError::Protocol)?;
        let mut combined = String::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                validate_remote_text(text)?;
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(text);
            }
        }
        if combined.is_empty() {
            return Ok(None);
        }
        self.message_bytes = self.message_bytes.saturating_add(combined.len());
        if self.message_bytes > MAX_RETAINED_MESSAGE_BYTES {
            return Err(A2aWireError::TooLarge);
        }
        self.messages.push(combined.clone());
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::Message {
                text: preview(&combined, 8192),
            },
        );
        Ok(Some(combined))
    }

    fn consume_artifact(
        &mut self,
        value: &Value,
        append: bool,
        finalize: bool,
    ) -> Result<(), A2aWireError> {
        let object = value.as_object().ok_or(A2aWireError::Protocol)?;
        let remote_id = required_id(object.get("artifactId"))?;
        let name = optional_text(object.get("name"), "artifact", 256)?;
        let parts = object
            .get("parts")
            .and_then(Value::as_array)
            .ok_or(A2aWireError::Protocol)?;
        if parts.len() > MAX_REMOTE_ARTIFACTS {
            return Err(A2aWireError::TooManyArtifacts);
        }
        for (index, part) in parts.iter().enumerate() {
            let key = format!("{remote_id}:{index}");
            if let Some(url) = part.get("url").and_then(Value::as_str) {
                if self.references.len() >= MAX_SAFE_REFERENCES {
                    return Err(A2aWireError::TooManyArtifacts);
                }
                let url = safe_reference(url)?;
                let media = optional_text(part.get("mediaType"), "application/octet-stream", 128)?;
                let filename = optional_text(part.get("filename"), &name, 256)?;
                self.references.push(SafeExternalReference {
                    remote_artifact_id: remote_id.clone(),
                    name: filename,
                    url,
                    media_type: media,
                    source_connection: self.connection.clone(),
                    source_task_id: self.task_id.clone(),
                });
                continue;
            }
            let (bytes, media_type, filename) = if let Some(raw) =
                part.get("raw").and_then(Value::as_str)
            {
                if raw.len() > MAX_ARTIFACT_BYTES.saturating_mul(2) {
                    return Err(A2aWireError::TooLarge);
                }
                let bytes = STANDARD.decode(raw).map_err(|_| A2aWireError::Protocol)?;
                let media = optional_text(part.get("mediaType"), "application/octet-stream", 128)?;
                let filename = optional_text(part.get("filename"), &name, 256)?;
                (bytes, media, filename)
            } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                validate_remote_text(text)?;
                (
                    text.as_bytes().to_vec(),
                    "text/plain".to_owned(),
                    name.clone(),
                )
            } else if let Some(data) = part.get("data") {
                let bytes = serde_json::to_vec(data).map_err(|_| A2aWireError::Protocol)?;
                (bytes, "application/json".to_owned(), name.clone())
            } else {
                return Err(A2aWireError::Protocol);
            };
            self.artifact_bytes = self.artifact_bytes.saturating_add(bytes.len());
            if bytes.len() > MAX_ARTIFACT_BYTES || self.artifact_bytes > MAX_RESPONSE_BYTES {
                return Err(A2aWireError::TooLarge);
            }
            let pending = self
                .pending
                .entry(key.clone())
                .or_insert_with(|| PendingArtifact {
                    remote_id: remote_id.clone(),
                    name: filename.clone(),
                    media_type: media_type.clone(),
                    bytes: Vec::new(),
                });
            if !append {
                pending.bytes.clear();
                pending.name = filename;
                pending.media_type = media_type;
            } else if pending.media_type != media_type {
                return Err(A2aWireError::Protocol);
            }
            if pending.bytes.len().saturating_add(bytes.len()) > MAX_ARTIFACT_BYTES {
                return Err(A2aWireError::TooLarge);
            }
            pending.bytes.extend_from_slice(&bytes);
            if finalize {
                self.finalize_pending(&key)?;
            }
        }
        Ok(())
    }

    fn finalize_pending(&mut self, key: &str) -> Result<(), A2aWireError> {
        if self.ingested.len() >= MAX_REMOTE_ARTIFACTS {
            return Err(A2aWireError::TooManyArtifacts);
        }
        let pending = self.pending.remove(key).ok_or(A2aWireError::Protocol)?;
        let (record, _) = self
            .artifacts
            .put(&pending.bytes, &pending.media_type, self.owner)
            .map_err(|_| A2aWireError::Artifact)?;
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::Artifact {
                name: pending.name.clone(),
                media_type: pending.media_type.clone(),
                byte_len: record.byte_len,
            },
        );
        self.ingested.push(IngestedArtifact {
            remote_artifact_id: pending.remote_id,
            name: pending.name,
            source_connection: self.connection.clone(),
            source_task_id: self.task_id.clone(),
            record,
        });
        Ok(())
    }

    fn record_state(&self, state: &str, remote_cancel: &str) -> Result<(), A2aWireError> {
        if let Some(task_id) = &self.task_id {
            self.manager
                .record_task(
                    &self.connection,
                    &self.identity_digest,
                    task_id,
                    self.context_id.as_deref(),
                    state,
                    remote_cancel,
                )
                .map_err(|_| A2aWireError::State)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<A2aDelegationReceipt, A2aWireError> {
        let keys = self.pending.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.finalize_pending(&key)?;
        }
        if !self.direct_message && self.task_id.is_none() {
            return Err(A2aWireError::Protocol);
        }
        if !self.direct_message
            && !matches!(
                self.state.as_str(),
                "completed"
                    | "failed"
                    | "canceled"
                    | "rejected"
                    | "input_required"
                    | "auth_required"
            )
        {
            self.state = "detached_unknown".into();
            self.record_state(&self.state, "unknown")?;
        }
        Ok(A2aDelegationReceipt {
            ownership: "external_a2a_agent",
            connection: self.connection,
            agent_name: self.agent_name,
            agent_identity_digest: self.identity_digest,
            task_id: self.task_id,
            context_id: self.context_id,
            terminal_state: self.state,
            remote_cancel: "not_requested".into(),
            messages: self.messages,
            artifacts: self.ingested,
            safe_references: self.references,
            untrusted: true,
        })
    }
}

struct RemoteCancelGuard {
    armed: bool,
    task_id: Option<String>,
    client: reqwest::Client,
    endpoint: reqwest::Url,
    credential: Option<CredentialReference>,
    connection: String,
    identity_digest: String,
    manager: ExternalAgentManager,
    events: Option<AgentEventSender>,
    operation_id: OperationId,
    agent_name: String,
}

impl RemoteCancelGuard {
    #[allow(clippy::too_many_arguments)]
    fn new(
        client: reqwest::Client,
        endpoint: reqwest::Url,
        credential: Option<CredentialReference>,
        connection: String,
        identity_digest: String,
        manager: ExternalAgentManager,
        events: Option<AgentEventSender>,
        operation_id: OperationId,
        agent_name: String,
    ) -> Self {
        Self {
            armed: true,
            task_id: None,
            client,
            endpoint,
            credential,
            connection,
            identity_digest,
            manager,
            events,
            operation_id,
            agent_name,
        }
    }

    fn set_task(&mut self, task_id: Option<String>) {
        if task_id.is_some() {
            self.task_id = task_id;
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemoteCancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(task_id) = self.task_id.clone() else {
            return;
        };
        let _ = self.manager.record_task(
            &self.connection,
            &self.identity_digest,
            &task_id,
            None,
            "detached_unknown",
            "requested_unconfirmed",
        );
        emit(
            self.events.as_ref(),
            self.operation_id,
            &self.connection,
            &self.agent_name,
            ExternalAgentActivityKind::CancellationRequested {
                task_id: task_id.clone(),
            },
        );
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let credential = self.credential.clone();
        let manager = self.manager.clone();
        let connection = self.connection.clone();
        let identity = self.identity_digest.clone();
        let events = self.events.clone();
        let operation_id = self.operation_id;
        let agent_name = self.agent_name.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let confirmed =
                    cancel_remote_task(&client, endpoint, credential.as_ref(), &task_id)
                        .await
                        .unwrap_or(false);
                let _ = manager.record_task(
                    &connection,
                    &identity,
                    &task_id,
                    None,
                    if confirmed {
                        "canceled"
                    } else {
                        "detached_unknown"
                    },
                    if confirmed { "confirmed" } else { "unknown" },
                );
                if !confirmed {
                    emit(
                        events.as_ref(),
                        operation_id,
                        &connection,
                        &agent_name,
                        ExternalAgentActivityKind::Detached { task_id },
                    );
                }
            });
        }
    }
}

async fn cancel_remote_task(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    credential: Option<&CredentialReference>,
    task_id: &str,
) -> Result<bool, A2aWireError> {
    let body = json!({
        "jsonrpc":"2.0",
        "id":uuid::Uuid::new_v4().to_string(),
        "method":"CancelTask",
        "params":{"id":task_id}
    });
    let mut request = client
        .post(endpoint)
        .timeout(CANCEL_TIMEOUT)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .header("A2A-Version", A2A_PROTOCOL_VERSION)
        .json(&body);
    if let Some(reference) = credential {
        let secret = CredentialResolver::default()
            .resolve(reference)
            .map_err(|_| A2aWireError::Unauthorized)?;
        request = request.bearer_auth(secret.expose());
    }
    let response = request.send().await.map_err(classify_reqwest)?;
    if !response.status().is_success() {
        return Ok(false);
    }
    let bytes = collect_body(response, &CancellationToken::new()).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| A2aWireError::Protocol)?;
    Ok(value
        .pointer("/result/task/status/state")
        .or_else(|| value.pointer("/result/status/state"))
        .and_then(Value::as_str)
        == Some("TASK_STATE_CANCELED"))
}

async fn collect_body(
    response: reqwest::Response,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, A2aWireError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(A2aWireError::TooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(A2aWireError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(classify_reqwest)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(A2aWireError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn read_workspace_file(workspace: &Path, source: &str) -> Result<(PathBuf, Vec<u8>), String> {
    if source.trim().is_empty() {
        return Err("selected file path must be non-empty".into());
    }
    let relative = PathBuf::from(source);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(format!(
            "selected file {source:?} must be workspace-relative"
        ));
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| "delegation workspace is unavailable".to_owned())?;
    let candidate = root.join(relative);
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|_| format!("selected file {source:?} is unavailable"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("selected file {source:?} is not a regular file"));
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|_| format!("selected file {source:?} is unavailable"))?;
    if !canonical.starts_with(&root) {
        return Err(format!(
            "selected file {source:?} resolves outside the workspace"
        ));
    }
    let mut file = fs::File::open(&canonical)
        .map_err(|_| format!("selected file {source:?} cannot be opened"))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_ARTIFACT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| format!("selected file {source:?} cannot be read"))?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "selected file {source:?} exceeds {MAX_ARTIFACT_BYTES} bytes"
        ));
    }
    Ok((canonical, bytes))
}

fn validate_task_text(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_TASK_BYTES || contains_unsafe_controls(value) {
        Err(format!(
            "delegated task must be non-blank, safe text at most {MAX_TASK_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn validate_message_text(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_MESSAGE_BYTES || contains_unsafe_controls(value) {
        Err(format!(
            "selected messages must be safe text at most {MAX_MESSAGE_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn validate_remote_text(value: &str) -> Result<(), A2aWireError> {
    if value.len() > MAX_MESSAGE_BYTES || contains_unsafe_controls(value) {
        Err(A2aWireError::Protocol)
    } else {
        Ok(())
    }
}

fn contains_unsafe_controls(value: &str) -> bool {
    value.chars().any(|character| {
        (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
            || matches!(
                character,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    })
}

fn validate_filename(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || Path::new(value).file_name().and_then(|name| name.to_str()) != Some(value)
    {
        Err("artifact filename must be one safe path component at most 256 bytes".into())
    } else {
        Ok(())
    }
}

fn required_id(value: Option<&Value>) -> Result<String, A2aWireError> {
    optional_id(value)?.ok_or(A2aWireError::Protocol)
}

fn optional_id(value: Option<&Value>) -> Result<Option<String>, A2aWireError> {
    value
        .map(|value| {
            value
                .as_str()
                .filter(|value| {
                    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .ok_or(A2aWireError::Protocol)
        })
        .transpose()
}

fn optional_text(
    value: Option<&Value>,
    default: &str,
    limit: usize,
) -> Result<String, A2aWireError> {
    let value = value
        .map_or(Some(default), Value::as_str)
        .ok_or(A2aWireError::Protocol)?;
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(A2aWireError::Protocol);
    }
    Ok(value.to_owned())
}

fn safe_reference(value: &str) -> Result<String, A2aWireError> {
    let url = reqwest::Url::parse(value).map_err(|_| A2aWireError::Protocol)?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.len() > 2048
    {
        return Err(A2aWireError::Protocol);
    }
    Ok(url.to_string())
}

fn normalize_task_state(value: &str) -> Result<&'static str, A2aWireError> {
    match value {
        "TASK_STATE_SUBMITTED" => Ok("submitted"),
        "TASK_STATE_WORKING" => Ok("working"),
        "TASK_STATE_COMPLETED" => Ok("completed"),
        "TASK_STATE_FAILED" => Ok("failed"),
        "TASK_STATE_CANCELED" => Ok("canceled"),
        "TASK_STATE_REJECTED" => Ok("rejected"),
        "TASK_STATE_INPUT_REQUIRED" => Ok("input_required"),
        "TASK_STATE_AUTH_REQUIRED" => Ok("auth_required"),
        _ => Err(A2aWireError::Protocol),
    }
}

fn preview(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit.saturating_sub(3);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn emit(
    events: Option<&AgentEventSender>,
    operation_id: OperationId,
    connection: &str,
    agent_name: &str,
    activity: ExternalAgentActivityKind,
) {
    if let Some(events) = events {
        let _ = events.send(AgentEvent::ExternalAgentActivity {
            operation_id,
            activity: ExternalAgentActivity {
                connection: connection.to_owned(),
                agent_name: agent_name.to_owned(),
                ownership: "external_a2a_agent".into(),
                activity,
            },
        });
    }
}

fn classify_reqwest(error: reqwest::Error) -> A2aWireError {
    if error.is_timeout() {
        A2aWireError::TimedOut
    } else if error.is_connect() {
        A2aWireError::Unavailable
    } else {
        A2aWireError::Protocol
    }
}

#[derive(Debug)]
enum A2aWireError {
    Unavailable,
    TimedOut,
    Cancelled,
    Unauthorized,
    RateLimited,
    Http,
    MediaType,
    Protocol,
    Remote,
    TooLarge,
    TooManyEvents,
    TooManyArtifacts,
    Artifact,
    State,
}

impl A2aWireError {
    fn classify(self) -> OutboundTransportFailure {
        match self {
            Self::Unavailable => OutboundTransportFailure::Unavailable,
            Self::TimedOut => OutboundTransportFailure::TimedOut,
            Self::Cancelled => OutboundTransportFailure::Cancelled,
            Self::Unauthorized | Self::RateLimited | Self::Http | Self::Remote => {
                OutboundTransportFailure::Rejected
            }
            Self::MediaType
            | Self::Protocol
            | Self::TooLarge
            | Self::TooManyEvents
            | Self::TooManyArtifacts
            | Self::Artifact
            | Self::State => OutboundTransportFailure::Protocol,
        }
    }
}

impl From<A2aError> for A2aWireError {
    fn from(_: A2aError) -> Self {
        Self::State
    }
}

#[cfg(test)]
mod tests;
