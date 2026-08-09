use super::{
    ChildExecution, ChildExecutionContext, ChildExecutionFactory, ChildExecutionOutput, ChildUsage,
    ExecutionOwner, PreparedChild, RouteResolver, SpawnAgentRequest,
};
use crate::{
    agent::Agent,
    artifact::ArtifactStore,
    config::{ConnectionConfig, ConnectionRegistry, PermissionMode, ProviderKind},
    context::{
        ContextBudget, ContextSource, SourceOrigin, SourceProvenance, TransientSourceId,
        TrustClass, canonical_text, estimate_tokens, read_project_instructions,
    },
    credential::CredentialResolver,
    message::{ContentBlock, Message, Role},
    model::ModelManager,
    permission::{PermissionPolicy, PermissionRule},
    prompt::{ProductDocumentationHint, PromptAssembler, PromptEnvironment, PromptSurface},
    provider::{
        ConversationalProvider, anthropic::AnthropicClient, openai_compat::OpenAiCompatClient,
    },
    self_docs,
    shell::Shell,
    tool::ToolRegistry,
    vision::MediaResolver,
};
use futures::future::BoxFuture;
use std::path::PathBuf;

pub(crate) struct NativeChildFactory {
    registry: ConnectionRegistry,
    models: ModelManager,
    shell: Shell,
    workspace_root: PathBuf,
    artifact_store: ArtifactStore,
    permission_rules: Vec<PermissionRule>,
    configured_shell: String,
}

impl NativeChildFactory {
    pub(crate) fn new(
        registry: ConnectionRegistry,
        models: ModelManager,
        shell: Shell,
        workspace_root: PathBuf,
        artifact_store: ArtifactStore,
        permission_rules: Vec<PermissionRule>,
    ) -> Self {
        let configured_shell = shell.prompt_description();
        Self {
            registry,
            models,
            shell,
            workspace_root,
            artifact_store,
            permission_rules,
            configured_shell,
        }
    }
}

impl ChildExecutionFactory for NativeChildFactory {
    fn prepare(&self, request: &SpawnAgentRequest) -> Result<PreparedChild, String> {
        let resolved = RouteResolver::new(&self.registry, &self.models)
            .resolve(request.route.as_deref())
            .map_err(|error| error.to_string())?;
        if resolved.owner != ExecutionOwner::Native {
            return Err(format!(
                "route {:?} is owned by {}; managed child execution is not enabled yet",
                resolved.route,
                resolved.owner.as_str()
            ));
        }
        if estimate_tokens(&request.task) > resolved.orchestration.max_context_tokens {
            return Err(format!(
                "child task exceeds route {:?}'s {}-token context bound",
                resolved.route, resolved.orchestration.max_context_tokens
            ));
        }
        let connection = self
            .registry
            .connections
            .get(&resolved.connection)
            .ok_or_else(|| {
                format!(
                    "resolved route {:?} lost connection {:?}",
                    resolved.route, resolved.connection
                )
            })?;
        let provider =
            compose_native_provider(connection, &resolved.model.id, self.artifact_store.clone())?;
        let tools = ToolRegistry::builtins_for_snapshot(self.shell.clone(), &resolved.capabilities)
            .map_err(|error| error.to_string())?;
        let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
        let project_sources = project_sources(&self.workspace_root)?;
        let total_tokens = resolved.orchestration.max_context_tokens;
        let reserve_tokens = total_tokens.min(4_096) / 2;
        let assembler = PromptAssembler::new(
            definitions,
            PromptEnvironment {
                operating_system: std::env::consts::OS.to_owned(),
                working_directory: self.workspace_root.clone(),
                configured_shell: self.configured_shell.clone(),
                surface: PromptSurface::Cli,
            },
            resolved
                .capabilities
                .tool_ids()
                .iter()
                .any(|tool| tool.as_str() == "xana_docs")
                .then(|| ProductDocumentationHint {
                    capability: "xana_docs".to_owned(),
                    references: self_docs::default_catalog()
                        .list(None)
                        .into_iter()
                        .map(|entry| entry.id.to_owned())
                        .collect(),
                }),
            ContextBudget {
                total_tokens,
                conversation_reserve_tokens: reserve_tokens,
            },
        );
        let prompt = assembler
            .assemble(&project_sources)
            .map_err(|error| format!("could not assemble child prompt: {error}"))?;
        let history = vec![Message::text(Role::User, request.task.clone())];
        prompt
            .messages_for_request(&history)
            .map_err(|error| format!("child task does not fit its prompt budget: {error}"))?;
        let policy = PermissionPolicy::new(
            policy_decision(resolved.permission_mode),
            self.permission_rules.clone(),
            &self.workspace_root,
        )
        .map_err(|error| format!("could not resolve child permission policy: {error}"))?;
        let agent = Agent::new(
            provider,
            tools,
            self.workspace_root.clone(),
            prompt,
            resolved.max_tool_rounds,
        );
        Ok(PreparedChild::new(
            resolved,
            policy,
            Box::new(NativeChildExecution { agent, history }),
        ))
    }
}

struct NativeChildExecution {
    agent: Agent,
    history: Vec<Message>,
}

impl ChildExecution for NativeChildExecution {
    fn run(
        self: Box<Self>,
        context: ChildExecutionContext,
    ) -> BoxFuture<'static, Result<ChildExecutionOutput, String>> {
        Box::pin(async move {
            let mut history = self.history;
            let run = self.agent.run_turn(
                context.operation_id,
                &mut history,
                context.permissions,
                context.events,
            );
            tokio::pin!(run);
            let message = tokio::select! {
                result = &mut run => result.map_err(|error| error.to_string())?,
                _ = context.cancellation.cancelled() => {
                    return Err("native child was cancelled".to_owned());
                }
            };
            Ok(ChildExecutionOutput {
                text: report_text(&message)?,
                usage: ChildUsage::Unknown,
            })
        })
    }
}

fn compose_native_provider(
    connection: &ConnectionConfig,
    model: &str,
    artifact_store: ArtifactStore,
) -> Result<Box<dyn ConversationalProvider>, String> {
    let base_url = connection
        .base_url
        .clone()
        .ok_or_else(|| format!("connection {:?} has no native endpoint", connection.id))?;
    let media = MediaResolver::new(artifact_store, crate::artifact::MAX_ARTIFACT_BYTES);
    let credentials = CredentialResolver::default();
    match connection.kind {
        ProviderKind::OpenAiCompat | ProviderKind::Ollama => {
            let client = match connection.credential.as_ref() {
                Some(reference) => {
                    let secret = credentials
                        .resolve(reference)
                        .map_err(|error| error.to_string())?;
                    OpenAiCompatClient::with_bearer_and_attribution(
                        base_url,
                        model.to_owned(),
                        secret,
                        None,
                        None,
                    )
                }
                None => OpenAiCompatClient::new(base_url, model.to_owned()),
            }
            .with_media_resolver(media);
            Ok(Box::new(client))
        }
        ProviderKind::OpenAi | ProviderKind::OpenRouter => {
            let reference = connection.credential.as_ref().ok_or_else(|| {
                format!("connection {:?} has no credential reference", connection.id)
            })?;
            let secret = credentials
                .resolve(reference)
                .map_err(|error| error.to_string())?;
            Ok(Box::new(
                OpenAiCompatClient::with_bearer_and_attribution(
                    base_url,
                    model.to_owned(),
                    secret,
                    None,
                    (connection.kind == ProviderKind::OpenRouter).then(|| "Xana".to_owned()),
                )
                .with_media_resolver(media),
            ))
        }
        ProviderKind::Anthropic => {
            let reference = connection.credential.as_ref().ok_or_else(|| {
                format!("connection {:?} has no credential reference", connection.id)
            })?;
            let secret = credentials
                .resolve(reference)
                .map_err(|error| error.to_string())?;
            Ok(Box::new(
                AnthropicClient::new(base_url, secret, model.to_owned()).with_media_resolver(media),
            ))
        }
        ProviderKind::Codex => Err("managed Codex is not a native provider".to_owned()),
    }
}

fn project_sources(workspace_root: &std::path::Path) -> Result<Vec<ContextSource>, String> {
    let Some(bytes) =
        read_project_instructions(workspace_root).map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "root AGENTS.md is not valid UTF-8".to_owned())?;
    Ok(vec![ContextSource {
        id: TransientSourceId::new("project:AGENTS.md"),
        provenance: SourceProvenance {
            display_name: "root AGENTS.md".to_owned(),
            path: Some(PathBuf::from(crate::context::PROJECT_INSTRUCTIONS)),
            origin: SourceOrigin::ProjectFile,
        },
        trust: TrustClass::Project,
        content: canonical_text(text),
        max_tokens: 4_096,
    }])
}

fn report_text(message: &Message) -> Result<String, String> {
    let mut output = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(text) => output.push_str(text),
            ContentBlock::Image(_) | ContentBlock::ToolCall(_) | ContentBlock::ToolResult(_) => {}
        }
    }
    if output.is_empty() {
        Err("native child completed without a textual report".to_owned())
    } else {
        Ok(output)
    }
}

fn policy_decision(mode: PermissionMode) -> crate::permission::PolicyDecision {
    mode.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_text_preserves_text_order_without_serializing_other_blocks() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("one".to_owned()),
                ContentBlock::Text(" two".to_owned()),
            ],
        };

        assert_eq!(report_text(&message), Ok("one two".to_owned()));
    }
}
