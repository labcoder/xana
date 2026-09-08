//! Application-injected execution refresh; the engine never reads settings.
use super::*;
use crate::profile::execution::ExecutionConfiguration;

pub(crate) struct PreparedExecution {
    pub(crate) agent: Agent,
    pub(crate) policy: PermissionPolicy,
    pub(crate) prompt_assembler: PromptAssembler,
    pub(crate) child_supervisor: Option<(ChildSupervisorHandle, ChildSupervisor)>,
    pub(crate) memory: Option<crate::memory::MemoryOwner>,
    pub(crate) configuration: ExecutionConfiguration,
}

pub(crate) trait ExecutionRefresh: Send + Sync {
    fn prepare<'a>(
        &'a self,
        session: &'a DurableSession,
        current: &'a ExecutionConfiguration,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<Option<PreparedExecution>>>;
}

impl Runtime {
    pub(super) async fn refresh_configuration(&mut self) -> Result<(), String> {
        let (Some(refresh), Some(current), Some(session)) = (
            &self.execution_refresh,
            &self.execution_configuration,
            &self.session,
        ) else {
            return Ok(());
        };
        // Never change the owner of an unresolved effect, suspended root, or
        // retained live child. A settings edit cannot authorize its continuation.
        if session.has_unfinished_work() {
            return Err("Unfinished work keeps its original settings. Stop the suspended operation or reconcile it with xana operation before sending a new turn; your Conversation is retained.".into());
        }
        let prepared = tokio::select! {
            biased;
            command = self.commands.recv() => {
                // Do not recursively handle commands while preparation borrows
                // the session. Retain exactly one and dispatch it in the owner
                // loop, with no new operation admitted in the meantime.
                self.deferred_command = Some(command.unwrap_or(RuntimeCommand::Shutdown));
                return Err("Settings preparation interrupted by a control command; this message was not sent. Retry it when ready.".into());
            }
            result = tokio::time::timeout(std::time::Duration::from_secs(10), refresh.prepare(session, current)) => {
                result.map_err(|_| "Settings refresh timed out; no new turn was admitted".to_owned())?
                    .map_err(|error| format!("Settings could not be applied; no new turn was admitted: {error:#}"))?
            }
        };
        let Some(prepared) = prepared else {
            return Ok(());
        };
        if session.live_children() {
            return Err("Settings changed while child work is retained. Wait for or stop that work before applying settings; this Conversation is unchanged.".into());
        }
        self.session
            .as_mut()
            .expect("durable configuration")
            .configure_execution(prepared.configuration.clone())
            .map_err(|error| format!("Could not record execution configuration: {error:#}"))?;
        self.permissions
            .replace_policy(prepared.policy)
            .await
            .map_err(|_| "Could not apply permission policy; no turn was admitted".to_owned())?;
        self.shutdown_children().await;
        let (handle, task) = match prepared.child_supervisor {
            Some((handle, supervisor)) => (
                Some(handle),
                Some(tokio::spawn(
                    supervisor.run(self._child_commit_sender.clone(), self.events.clone()),
                )),
            ),
            None => (None, None),
        };
        self.child_supervisor = handle;
        self.child_supervisor_task = task;
        self.agent = Arc::new(prepared.agent);
        self.prompt_assembler = Some(prepared.prompt_assembler);
        self.memory = prepared.memory;
        self.execution_configuration = Some(prepared.configuration.clone());
        self.emit(AgentEvent::ExecutionConfigurationChanged {
            connection: prepared.configuration.profile.connection.value,
            model: prepared.configuration.profile.model.value,
            profile: prepared.configuration.profile.name,
            approval_policy: prepared
                .configuration
                .profile
                .permission_mode
                .value
                .as_str()
                .into(),
            reasoning_effort: prepared.configuration.profile.reasoning_effort.value,
        });
        Ok(())
    }
}
