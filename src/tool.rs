//! Provider-neutral tool contract, safety metadata, and registry.
//!
//! Concrete tools own argument decoding and typed implementation errors. The
//! registry exposes stable definitions and dispatches model requests without
//! treating effect metadata as permission or containment.

mod child_agent;
mod delegate_agent;
mod discovery;
mod edit_file;
mod find_files;
mod grep_files;
mod list_files;
mod orchestration_plan;
mod read_document;
mod read_file;
mod run_command;
mod web_fetch;
mod web_search;
mod workspace_path;
mod write_file;
mod xana_docs;

use crate::identity::{OperationId, ToolInvocationId};
use crate::message::{ToolCall, ToolResult};
use crate::permission::{
    Authorization, PermissionBrokerHandle, PermissionRequest, PermissionScope,
};
use crate::shell::Shell;
use crate::telemetry::{
    NoopRuntimeTelemetry, RuntimeTelemetry, RuntimeTelemetryEvent, RuntimeTelemetryKind,
};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const MAX_DEFERRED_CLEANUPS: usize = 16;
const DEFERRED_CLEANUP_DEADLINE: Duration = Duration::from_secs(6);

/// Returns bounded, gitignore-aware workspace file candidates for presentation
/// completion. Discovery reveals names only and grants no read authority.
pub(crate) fn complete_workspace_paths(
    workspace_root: &Path,
    query: &str,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, String> {
    if query.len() > discovery::MAX_PATTERN_BYTES {
        return Err("file completion query exceeds its input bound".to_owned());
    }
    let plan = discovery::plan(".".to_owned(), "**/*", 16, workspace_root)
        .map_err(|error| error.to_string())?;
    let query = query.to_ascii_lowercase();
    let mut candidates = Vec::<(usize, String)>::new();
    discovery::visit(&plan, |entry| {
        if cancellation.is_cancelled() {
            return discovery::VisitControl::Stop;
        }
        if !entry.is_file {
            return discovery::VisitControl::Continue;
        }
        if let Some(score) = fuzzy_path_score(&entry.workspace_relative, &query) {
            candidates.push((score, entry.workspace_relative));
            candidates
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
            candidates.truncate(limit);
        }
        discovery::VisitControl::Continue
    })
    .map_err(|error| error.to_string())?;
    if cancellation.is_cancelled() {
        return Ok(Vec::new());
    }
    Ok(candidates.into_iter().map(|(_, path)| path).collect())
}

fn fuzzy_path_score(path: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(0);
    }
    let path = path.to_ascii_lowercase();
    if let Some(index) = path.find(query) {
        return Some(10_000usize.saturating_sub(index));
    }
    let mut score = 1_000usize;
    let mut position = 0usize;
    for expected in query.chars() {
        let relative = path[position..].find(expected)?;
        score = score.saturating_sub(relative);
        position = position.saturating_add(relative + expected.len_utf8());
    }
    Some(score)
}

pub(crate) const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "list_files",
    "find_files",
    "grep_files",
    "write_file",
    "edit_file",
    "run_command",
    "web_fetch",
    "read_document",
    "xana_docs",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EffectClass {
    Read,
    Write,
    Execute,
    Network,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplaySafety {
    Safe,
    Never,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolDefinition {
    pub(crate) name: String,
    pub(crate) contract_version: u32,
    pub(crate) description: String,
    pub(crate) parameters: Value,
    pub(crate) effect_class: EffectClass,
    pub(crate) replay_safety: ReplaySafety,
}

pub(crate) trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn plan(
        &self,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Result<PlannedToolInvocation, String>;

    /// Owner input is attached by the application, never reconstructed from
    /// model arguments, tool output, or a child's conversation.
    fn plan_in_turn(
        &self,
        arguments: &Value,
        workspace_root: &Path,
        _owner_input: Option<&OwnerTurnInput>,
    ) -> Result<PlannedToolInvocation, ToolPlanningError> {
        self.plan(arguments, workspace_root).map_err(Into::into)
    }

    fn execute<'a>(
        &'a self,
        planned: &'a PlannedToolInvocation,
        context: ToolExecutionContext,
    ) -> BoxFuture<'a, Result<String, String>>;

    /// Resolves an exact saved outbound decision without performing I/O.
    /// External tools must override this method so the registry can avoid a
    /// redundant generic prompt while retaining the guard as final authority.
    fn outbound_disposition(
        &self,
        _planned: &PlannedToolInvocation,
    ) -> Result<Option<crate::outbound::OutboundDisposition>, String> {
        Ok(None)
    }
}

/// Preserve a trusted planning category separately from model-visible wording.
#[derive(Debug)]
pub(crate) struct ToolPlanningError {
    pub(crate) message: String,
    pub(crate) failure: Option<crate::message::ToolFailure>,
}

impl From<String> for ToolPlanningError {
    fn from(message: String) -> Self {
        Self {
            message,
            failure: None,
        }
    }
}

/// Immutable authority facts for one admitted foreground owner turn. This is
/// deliberately not serializable: resumption cannot manufacture fresh consent.
#[derive(Clone)]
pub(crate) struct OwnerTurnInput {
    pub(crate) operation_id: OperationId,
    pub(crate) source_id: uuid::Uuid,
    pub(crate) text: Arc<str>,
    pub(crate) cancellation: tokio_util::sync::CancellationToken,
}

#[derive(Clone)]
pub(crate) struct ToolExecutionContext {
    pub(crate) operation_id: OperationId,
    pub(crate) events: Option<crate::native_runtime::AgentEventSender>,
    pub(crate) outbound_approval: Option<crate::outbound::ReviewedOutboundApproval>,
    pub(crate) cleanup: DeferredCleanup,
}

impl ToolExecutionContext {
    pub(crate) fn authorized(
        operation_id: OperationId,
        events: Option<crate::native_runtime::AgentEventSender>,
        authorization: &Authorization,
        cleanup: DeferredCleanup,
    ) -> Self {
        let outbound_approval = match authorization {
            Authorization::Allowed(fact)
                if matches!(fact.request.scope, PermissionScope::External { .. }) =>
            {
                let decision = match fact.controller_decision.as_ref() {
                    Some(crate::permission::ControllerDecision::AllowPublicWebTurn) => {
                        Some(crate::outbound::OutboundApprovalDecision::AllowPublicWebTurn)
                    }
                    Some(crate::permission::ControllerDecision::SaveOutboundAllow) => {
                        Some(crate::outbound::OutboundApprovalDecision::SaveAllow)
                    }
                    Some(crate::permission::ControllerDecision::SaveOutboundDeny) => {
                        Some(crate::outbound::OutboundApprovalDecision::SaveDeny)
                    }
                    Some(
                        crate::permission::ControllerDecision::AllowOnce
                        | crate::permission::ControllerDecision::AllowSession { .. },
                    ) => Some(crate::outbound::OutboundApprovalDecision::AllowOnce),
                    Some(crate::permission::ControllerDecision::Deny) => None,
                    None if fact.policy_evaluation == crate::permission::PolicyDecision::Ask => {
                        Some(crate::outbound::OutboundApprovalDecision::AllowOnce)
                    }
                    None => None,
                };
                decision
                    .zip(fact.request.outbound_review.clone())
                    .map(|(decision, review)| {
                        crate::outbound::ReviewedOutboundApproval::new(review, decision)
                    })
            }
            Authorization::Allowed(_) | Authorization::Denied(_) => None,
        };
        Self {
            operation_id,
            events,
            outbound_approval,
            cleanup,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct DeferredCleanup {
    inner: Arc<CleanupSupervisor>,
}

impl DeferredCleanup {
    pub(crate) fn schedule(&self, cleanup: BoxFuture<'static, ()>) -> bool {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.accepting || state.active >= MAX_DEFERRED_CLEANUPS {
            return false;
        }
        state.active += 1;
        let slot = CleanupSlot {
            inner: Arc::clone(&self.inner),
        };
        let cancelled = self.inner.cancelled.clone();
        self.inner.tracker.spawn_on(
            async move {
                let _slot = slot;
                tokio::select! {
                    biased;
                    () = cancelled.cancelled() => {}
                    () = cleanup => {}
                }
            },
            &runtime,
        );
        true
    }

    pub(crate) async fn drain(&self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.accepting = false;
            self.inner.tracker.close();
        }
        if tokio::time::timeout(DEFERRED_CLEANUP_DEADLINE, self.inner.tracker.wait())
            .await
            .is_err()
        {
            self.inner.cancelled.cancel();
            let _ =
                tokio::time::timeout(Duration::from_millis(250), self.inner.tracker.wait()).await;
        }
    }
}

struct CleanupSupervisor {
    tracker: TaskTracker,
    cancelled: CancellationToken,
    state: Mutex<CleanupState>,
}

impl Default for CleanupSupervisor {
    fn default() -> Self {
        Self {
            tracker: TaskTracker::new(),
            cancelled: CancellationToken::new(),
            state: Mutex::new(CleanupState {
                accepting: true,
                active: 0,
            }),
        }
    }
}

impl Drop for CleanupSupervisor {
    fn drop(&mut self) {
        self.tracker.close();
        self.cancelled.cancel();
    }
}

struct CleanupState {
    accepting: bool,
    active: usize,
}

struct CleanupSlot {
    inner: Arc<CleanupSupervisor>,
}

impl Drop for CleanupSlot {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active = state.active.saturating_sub(1);
    }
}

pub(crate) struct PlannedToolInvocation {
    pub(crate) final_arguments: Value,
    pub(crate) scope: PermissionScope,
    outbound_review: Option<crate::outbound::OutboundApprovalRequest>,
    executable: Box<dyn Any + Send + Sync>,
}

pub(crate) struct PreparedToolInvocation<'a> {
    pub(crate) call_id: String,
    pub(crate) definition: &'a ToolDefinition,
    implementation: &'a dyn Tool,
    planned: PlannedToolInvocation,
}

impl PreparedToolInvocation<'_> {
    pub(crate) fn final_arguments(&self) -> &Value {
        &self.planned.final_arguments
    }

    pub(crate) fn scope(&self) -> &PermissionScope {
        &self.planned.scope
    }

    pub(crate) fn permission_request(
        &self,
        operation_id: OperationId,
        invocation_id: ToolInvocationId,
    ) -> PermissionRequest {
        PermissionRequest {
            operation_id,
            invocation_id,
            tool_name: self.definition.name.clone(),
            effect_class: self.definition.effect_class,
            final_arguments: self.planned.final_arguments.clone(),
            scope: self.planned.scope.clone(),
            outbound_review: self
                .planned
                .outbound_review
                .clone()
                .map(|review| review.for_operation(operation_id)),
        }
    }

    pub(crate) async fn execute(&self, context: ToolExecutionContext) -> Result<String, String> {
        self.implementation.execute(&self.planned, context).await
    }
}

impl PlannedToolInvocation {
    pub(crate) fn new<T>(final_arguments: Value, scope: PermissionScope, executable: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            final_arguments,
            scope,
            outbound_review: None,
            executable: Box::new(executable),
        }
    }

    pub(crate) fn with_outbound_review(
        mut self,
        review: crate::outbound::OutboundApprovalRequest,
    ) -> Self {
        self.outbound_review = Some(review);
        self
    }

    pub(crate) fn executable<T: Any>(&self, tool_name: &str) -> Result<&T, String> {
        self.executable
            .downcast_ref::<T>()
            .ok_or_else(|| format!("{tool_name} received an incompatible invocation plan"))
    }
}

#[derive(Clone)]
pub(crate) struct ToolContext<'a> {
    pub(crate) workspace_root: &'a Path,
    pub(crate) operation_id: OperationId,
    pub(crate) invocation_id: ToolInvocationId,
    pub(crate) permissions: &'a PermissionBrokerHandle,
    pub(crate) events: Option<&'a crate::native_runtime::AgentEventSender>,
    pub(crate) cleanup: DeferredCleanup,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegistryError {
    DuplicateName { name: String },
    Composition(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName { name } => {
                write!(f, "tool {name:?} is already registered")
            }
            Self::Composition(reason) => write!(f, "could not compose tool capabilities: {reason}"),
        }
    }
}

impl Error for RegistryError {}

struct RegisteredTool {
    definition: ToolDefinition,
    implementation: Box<dyn Tool>,
    advertised: bool,
}

pub(crate) struct ToolRegistry {
    tools: Vec<RegisteredTool>,
    unavailable: BTreeMap<&'static str, &'static str>,
    telemetry: Arc<dyn RuntimeTelemetry>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: Vec::new(),
            unavailable: BTreeMap::new(),
            telemetry: Arc::new(NoopRuntimeTelemetry),
        }
    }
}

impl ToolRegistry {
    /// Configure the existing web tools without replacing chat providers or
    /// granting arbitrary MCP/network capabilities. One budget owner per registry.
    pub(crate) fn configure_web(
        &mut self,
        paths: &crate::paths::XanaPaths,
        config: &crate::web::WebConfig,
        profile_egress: &[crate::config::OutboundDataClass],
    ) -> Result<(), RegistryError> {
        config.validate().map_err(RegistryError::Composition)?;
        if !profile_egress.contains(&crate::config::OutboundDataClass::PromptText) {
            self.tools
                .retain(|tool| tool.definition.name != "web_fetch");
            let reason = "Public web is unavailable under this operation's Profile disclosure policy. Run xana connect web to review setup; the next new turn in this Conversation can use the updated settings. Do not guess URLs or bypass this policy with commands or another service.";
            self.register_unavailable("web_fetch", reason)?;
            return self.register_unavailable("web_search", reason);
        }
        let runtime = Arc::new(
            crate::web::WebRuntime::new(config.limits.clone()).with_profile(profile_egress),
        );
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.definition.name == "web_fetch")
        {
            tool.implementation = Box::new(web_fetch::WebFetch::configured(
                paths.clone(),
                config.clone(),
                Arc::clone(&runtime),
            ));
            tool.definition = tool.implementation.definition();
        }
        if config.selected().is_some() {
            self.register(web_search::WebSearch {
                paths: paths.clone(),
                config: config.clone(),
                runtime,
                #[cfg(test)]
                fixture: None,
            })
        } else {
            self.register_unavailable("web_search", "Web search is not configured. Run xana connect web to choose Exa API, Exa hosted MCP, or Brave. Do not invent search results or guess URL paths.")
        }
    }
    pub(crate) fn command_status(
        name: &str,
        output: &str,
    ) -> Option<crate::completion_evidence::CommandStatus> {
        (name == "run_command")
            .then(|| run_command::observed_status(output))
            .flatten()
    }
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register<T>(&mut self, tool: T) -> Result<(), RegistryError>
    where
        T: Tool + 'static,
    {
        self.register_boxed(Box::new(tool))
    }

    pub(crate) fn register_boxed(&mut self, tool: Box<dyn Tool>) -> Result<(), RegistryError> {
        let definition = tool.definition();

        if self.definition(&definition.name).is_some()
            || self.unavailable.contains_key(definition.name.as_str())
        {
            return Err(RegistryError::DuplicateName {
                name: definition.name,
            });
        }

        self.tools.push(RegisteredTool {
            definition,
            implementation: tool,
            advertised: true,
        });
        Ok(())
    }

    /// Decode historical contracts without offering them for new model choices.
    pub(crate) fn register_legacy<T: Tool + 'static>(
        &mut self,
        tool: T,
    ) -> Result<(), RegistryError> {
        self.register(tool)?;
        self.tools.last_mut().expect("registered tool").advertised = false;
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<&ToolDefinition> {
        self.tools
            .iter()
            .filter(|tool| tool.advertised)
            .map(|tool| &tool.definition)
            .collect()
    }

    /// Retain a rejection for stale calls without advertising an unusable tool.
    /// Only composition supplies these fixed facts; model input cannot add them.
    pub(crate) fn register_unavailable(
        &mut self,
        name: &'static str,
        reason: &'static str,
    ) -> Result<(), RegistryError> {
        if self.definition(name).is_some() || self.unavailable.contains_key(name) {
            return Err(RegistryError::DuplicateName { name: name.into() });
        }
        self.unavailable.insert(name, reason);
        Ok(())
    }

    /// Decorate execution without rebuilding or changing the exposed contracts.
    /// Application-owned guards keep their state outside the agent itself.
    pub(crate) fn map_implementations(
        mut self,
        mut decorate: impl FnMut(Box<dyn Tool>) -> Box<dyn Tool>,
    ) -> Self {
        self.tools = self
            .tools
            .into_iter()
            .map(|registered| RegisteredTool {
                definition: registered.definition,
                implementation: decorate(registered.implementation),
                advertised: registered.advertised,
            })
            .collect();
        self
    }

    pub(crate) fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.tools
            .iter()
            .find(|tool| tool.definition.name == name)
            .map(|tool| &tool.definition)
    }

    pub(crate) async fn invoke(&self, call: &ToolCall, context: ToolContext<'_>) -> ToolResult {
        self.invoke_in_turn(call, context, None).await
    }

    pub(crate) async fn invoke_in_turn(
        &self,
        call: &ToolCall,
        context: ToolContext<'_>,
        owner_input: Option<&OwnerTurnInput>,
    ) -> ToolResult {
        if owner_input.is_some_and(|input| input.operation_id != context.operation_id) {
            return ToolResult::error(call.id.clone(), "owner input belongs to another operation");
        }
        let planned = match self.plan_in_turn(call, context.workspace_root, owner_input) {
            Ok(planned) => planned,
            Err(result) => {
                self.record_tool_event(
                    context.operation_id,
                    RuntimeTelemetryKind::ToolFailed,
                    &call.name,
                );
                return result;
            }
        };
        let saved_outbound = if matches!(planned.scope(), PermissionScope::External { .. }) {
            if planned.planned.outbound_review.is_none() {
                self.record_tool_event(
                    context.operation_id,
                    RuntimeTelemetryKind::ToolFailed,
                    &call.name,
                );
                return ToolResult::error(
                    call.id.clone(),
                    "external tool is missing Xana's exact outbound review",
                );
            }
            match planned
                .implementation
                .outbound_disposition(&planned.planned)
            {
                Ok(Some(disposition)) => Some(disposition),
                Ok(None) => {
                    self.record_tool_event(
                        context.operation_id,
                        RuntimeTelemetryKind::ToolFailed,
                        &call.name,
                    );
                    return ToolResult::error(
                        call.id.clone(),
                        "external tool is missing Xana's outbound authorization preflight",
                    );
                }
                Err(error) => {
                    self.record_tool_event(
                        context.operation_id,
                        RuntimeTelemetryKind::ToolFailed,
                        &call.name,
                    );
                    return ToolResult::error(call.id.clone(), error);
                }
            }
        } else {
            None
        };
        if matches!(
            saved_outbound,
            Some(
                crate::outbound::OutboundDisposition::SavedAllow
                    | crate::outbound::OutboundDisposition::SavedDeny
            )
        ) {
            let execution = planned
                .execute(ToolExecutionContext {
                    operation_id: context.operation_id,
                    events: context.events.cloned(),
                    outbound_approval: None,
                    cleanup: context.cleanup.clone(),
                })
                .await;
            return match execution {
                Ok(output) => ToolResult::success(call.id.clone(), output),
                Err(error) => {
                    self.record_tool_event(
                        context.operation_id,
                        RuntimeTelemetryKind::ToolFailed,
                        &call.name,
                    );
                    ToolResult::error(call.id.clone(), error)
                }
            };
        }

        let request = planned.permission_request(context.operation_id, context.invocation_id);
        let authorization = match context.permissions.authorize(request).await {
            Ok(authorization) => authorization,
            Err(error) => {
                self.record_tool_event(
                    context.operation_id,
                    RuntimeTelemetryKind::ToolFailed,
                    &call.name,
                );
                return ToolResult::error(call.id.clone(), error.to_string());
            }
        };
        if matches!(authorization, Authorization::Denied(_)) {
            self.record_tool_event(
                context.operation_id,
                RuntimeTelemetryKind::ToolDenied,
                &call.name,
            );
            return ToolResult::denied(
                call.id.clone(),
                format!("permission denied for tool {:?}", call.name),
            );
        }

        let execution = planned
            .execute(ToolExecutionContext::authorized(
                context.operation_id,
                context.events.cloned(),
                &authorization,
                context.cleanup.clone(),
            ))
            .await;
        match execution {
            Ok(output) => {
                let status = Self::command_status(&call.name, &output);
                let mut result = ToolResult::success(call.id.clone(), output);
                result.command_status = status;
                result
            }
            Err(error) => {
                self.record_tool_event(
                    context.operation_id,
                    RuntimeTelemetryKind::ToolFailed,
                    &call.name,
                );
                ToolResult::error(call.id.clone(), error)
            }
        }
    }

    pub(crate) fn set_runtime_telemetry(&mut self, telemetry: Arc<dyn RuntimeTelemetry>) {
        self.telemetry = telemetry;
    }

    fn record_tool_event(
        &self,
        operation_id: OperationId,
        kind: RuntimeTelemetryKind,
        subject: &str,
    ) {
        self.telemetry.record(RuntimeTelemetryEvent {
            operation_id,
            kind,
            subject: subject.to_owned(),
        });
    }

    pub(crate) fn plan<'a>(
        &'a self,
        call: &ToolCall,
        workspace_root: &Path,
    ) -> Result<PreparedToolInvocation<'a>, ToolResult> {
        self.plan_in_turn(call, workspace_root, None)
    }

    pub(crate) fn plan_in_turn<'a>(
        &'a self,
        call: &ToolCall,
        workspace_root: &Path,
        owner_input: Option<&OwnerTurnInput>,
    ) -> Result<PreparedToolInvocation<'a>, ToolResult> {
        if let Some(reason) = self.unavailable.get(call.name.as_str()) {
            return Err(ToolResult::unavailable(call.id.clone(), *reason));
        }
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.definition.name == call.name.as_str())
        else {
            return Err(ToolResult::error(
                call.id.clone(),
                format!("unknown tool {:?}", call.name),
            ));
        };

        let planned =
            match tool
                .implementation
                .plan_in_turn(&call.arguments, workspace_root, owner_input)
            {
                Ok(planned) => planned,
                Err(error) => {
                    return Err(ToolResult {
                        failure: error.failure,
                        ..ToolResult::error(call.id.clone(), error.message)
                    });
                }
            };
        Ok(PreparedToolInvocation {
            call_id: call.id.clone(),
            definition: &tool.definition,
            implementation: tool.implementation.as_ref(),
            planned,
        })
    }

    pub(crate) fn builtins(shell: Shell) -> Result<Self, RegistryError> {
        let exposed = crate::capability::resolve_builtin_tool_names()
            .map_err(|error| RegistryError::Composition(error.to_string()))?;
        Self::builtins_from_names(shell, &exposed)
    }

    pub(crate) fn builtins_for_snapshot(
        shell: Shell,
        snapshot: &crate::capability::AgentCapabilitySnapshot,
    ) -> Result<Self, RegistryError> {
        let exposed = snapshot
            .tool_ids()
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        Self::builtins_from_names(shell, &exposed)
    }

    pub(crate) fn builtins_from_names(
        shell: Shell,
        exposed: &std::collections::BTreeSet<String>,
    ) -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        if exposed.contains("read_file") {
            registry.register(read_file::ReadFile)?;
        }
        if exposed.contains("list_files") {
            registry.register(list_files::ListFiles)?;
        }
        if exposed.contains("find_files") {
            registry.register(find_files::FindFiles)?;
        }
        if exposed.contains("grep_files") {
            registry.register(grep_files::GrepFiles)?;
        }
        if exposed.contains("write_file") {
            registry.register(write_file::WriteFile)?;
        }
        if exposed.contains("edit_file") {
            registry.register(edit_file::EditFile)?;
        }
        if exposed.contains("run_command") {
            registry.register(run_command::RunCommand::new(shell))?;
        }
        if exposed.contains("web_fetch") {
            registry.register(web_fetch::WebFetch::default())?;
        }
        if exposed.contains("read_document") {
            registry.register(read_document::ReadDocument::default())?;
        }
        if exposed.contains("xana_docs") {
            registry.register(xana_docs::XanaDocs)?;
        }
        Ok(registry)
    }

    pub(crate) fn enable_child_delegation(
        &mut self,
        supervisor: crate::orchestration::ChildSupervisorHandle,
    ) -> Result<(), RegistryError> {
        self.register(child_agent::SpawnAgent::new(supervisor.clone()))?;
        self.register(child_agent::SpawnMany::new(supervisor.clone()))?;
        self.register(child_agent::AwaitAgent::new(supervisor.clone()))?;
        self.register(child_agent::CancelAgent::new(supervisor.clone()))?;
        self.register(child_agent::CollectAgents::new(supervisor.clone()))?;
        self.register(orchestration_plan::ValidateOrchestrationPlan::new(
            supervisor.clone(),
        ))?;
        self.register(orchestration_plan::ExecuteOrchestrationPlan::new(
            supervisor.clone(),
        ))?;
        self.register(delegate_agent::DelegateAgent::new(supervisor))
    }

    #[cfg(test)]
    pub(crate) fn builtins_for_tests() -> Result<Self, RegistryError> {
        let shell = Shell::resolve(crate::shell::ShellConfig::default())
            .expect("the platform shell configuration is valid");
        Self::builtins(shell)
    }

    #[cfg(test)]
    pub(crate) fn execute_for_tests(&self, call: &ToolCall, workspace_root: &Path) -> ToolResult {
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let policy = crate::permission::PermissionPolicy::new(
                crate::permission::PolicyDecision::Allow,
                Vec::new(),
                workspace_root,
            )
            .expect("allow policy");
            let (permissions, _broker) =
                crate::permission::PermissionBroker::spawn(policy, false, events);
            self.invoke(
                call,
                ToolContext {
                    workspace_root,
                    operation_id: OperationId::new(),
                    invocation_id: ToolInvocationId::new(),
                    permissions: &permissions,
                    events: None,
                    cleanup: DeferredCleanup::default(),
                },
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests;
