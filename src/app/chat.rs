//! Composition of one interactive or one-shot chat launch.
//!
//! The interface accepts resolved process-edge intent. Configuration, provider,
//! session, runtime, and frontend ownership stay inside this module so command
//! routing does not need to know their construction order.

use super::{
    ChatExit, ChatHeader, codex_launch, model_manager, run_doctor_command, run_reset_command,
    run_setup_command,
};
use crate::{
    agent::Agent,
    artifact::ArtifactStore,
    cli,
    config::{ProviderKind, XanaConfig},
    context::{ContextBudget, ContextPlanReport},
    identity::ConversationId,
    managed::codex::CodexAppServer,
    managed_execution::{
        ManagedChatConfig, ManagedOneShotRequest, run_codex_chat, run_codex_one_shot,
    },
    native_runtime::RuntimeHandle,
    oneshot::{OneShotReporter, OneShotSuccess, StreamSequence},
    orchestration::{
        ChildExecutionOwnerFactory, ChildSupervisor, OrchestrationBudget, ParentExecution,
        compose_native_provider,
    },
    paths::XanaPaths,
    permission::PermissionPolicy,
    plain_terminal,
    presentation::{self, BannerMode},
    prompt::{
        ModelBudgetFacts, ProductDocumentationHint, PromptAssembler, PromptBudgetPlan,
        PromptEnvironment, PromptSurface,
    },
    session::DurableSession,
    shell::Shell,
    tool::ToolRegistry,
    tui,
    workspace_host::{ConversationRef, WorkspaceHost, WorkspaceHostError},
};
use anyhow::{Context, Result};
use clap::Parser as _;
use std::{io::Write, sync::Arc};

mod configuration;
mod native;

#[cfg(test)]
mod tests;

pub(super) enum ChatSurface {
    Plain(BannerMode),
    Tui {
        prepared: tui::PreparedTui,
        required: bool,
    },
    Hosted {
        bind: std::net::IpAddr,
        port: u16,
        presentation: presentation::ResolvedPresentation,
    },
    Desktop {
        bridge: crate::desktop::Bridge,
        presentation: presentation::ResolvedPresentation,
        workspace: std::path::PathBuf,
    },
}

impl ChatSurface {
    fn profile(&self) -> presentation::ResolvedPresentation {
        match self {
            Self::Plain(mode) => mode.profile(),
            Self::Tui { prepared, .. } => prepared.profile(),
            Self::Hosted { presentation, .. } => *presentation,
            Self::Desktop { presentation, .. } => *presentation,
        }
    }

    fn workspace(&self) -> Option<&std::path::Path> {
        match self {
            Self::Desktop { workspace, .. } => Some(workspace),
            _ => None,
        }
    }
}

pub(super) async fn run(
    paths: &XanaPaths,
    surface: ChatSurface,
    resume: Option<crate::identity::SessionId>,
    continue_chat: bool,
    force_new: bool,
    one_shot: Option<OneShotInput>,
    stream_sequence: Option<StreamSequence>,
) -> Result<Option<OneShotSuccess>> {
    run_with_target(
        paths,
        surface,
        ChatLaunch {
            resume,
            continue_chat,
            force_new,
            one_shot,
            stream_sequence,
            conversation_target: None,
        },
    )
    .await
}

pub(super) async fn run_attached(
    paths: &XanaPaths,
    surface: ChatSurface,
    conversation: ConversationRef,
) -> Result<Option<OneShotSuccess>> {
    run_with_target(
        paths,
        surface,
        ChatLaunch {
            conversation_target: Some(conversation),
            ..ChatLaunch::default()
        },
    )
    .await
}

#[derive(Default)]
struct ChatLaunch {
    resume: Option<crate::identity::SessionId>,
    continue_chat: bool,
    force_new: bool,
    one_shot: Option<OneShotInput>,
    stream_sequence: Option<StreamSequence>,
    conversation_target: Option<ConversationRef>,
}

pub(super) struct OneShotInput {
    pub(super) input: String,
    pub(super) contract: crate::completion_evidence::CompletionContract,
}

async fn run_with_target(
    paths: &XanaPaths,
    mut surface: ChatSurface,
    launch: ChatLaunch,
) -> Result<Option<OneShotSuccess>> {
    let ChatLaunch {
        mut resume,
        mut continue_chat,
        mut force_new,
        mut one_shot,
        stream_sequence,
        mut conversation_target,
    } = launch;
    loop {
        match run_once(
            paths,
            surface,
            ChatIntent {
                resume,
                conversation_target: conversation_target.clone(),
                continue_chat,
                force_new,
                one_shot,
                stream_sequence: stream_sequence.clone(),
            },
        )
        .await?
        {
            ChatRun::Complete(result) => return Ok(result),
            ChatRun::Exited(state) => {
                let Some(restart) = continue_after_chat_exit(paths, *state).await? else {
                    return Ok(None);
                };
                surface = restart.surface;
                resume = restart.resume;
                conversation_target = restart.conversation_target;
                continue_chat = restart.continue_chat;
                force_new = restart.force_new;
                one_shot = None;
            }
        }
    }
}

enum ChatRun {
    Complete(Option<OneShotSuccess>),
    Exited(Box<ChatExitState>),
}

struct ChatExitState {
    exit: ChatExit,
    presentation: presentation::ResolvedPresentation,
    restart_tui: bool,
    tui_required: bool,
    tui_continuation: Option<tui::TuiContinuation>,
    desktop_restart: Option<DesktopRestart>,
    conversation: ConversationRef,
}

struct DesktopRestart {
    bridge: crate::desktop::Bridge,
    workspace: std::path::PathBuf,
}

struct ChatRestart {
    surface: ChatSurface,
    resume: Option<crate::identity::SessionId>,
    conversation_target: Option<ConversationRef>,
    continue_chat: bool,
    force_new: bool,
}

struct ChatIntent {
    resume: Option<crate::identity::SessionId>,
    conversation_target: Option<ConversationRef>,
    continue_chat: bool,
    force_new: bool,
    one_shot: Option<OneShotInput>,
    stream_sequence: Option<StreamSequence>,
}

fn automatic_conversation(
    snapshot: &crate::workspace_host::WorkspaceSnapshot,
    connection: &str,
    kind: ProviderKind,
    require_existing: bool,
) -> Result<Option<ConversationRef>> {
    if kind == ProviderKind::Codex {
        return Ok(snapshot.conversations.iter().find_map(|row| {
            matches!(&row.conversation, ConversationRef::Managed { connection: name, .. }
                if name == connection && row.selected)
            .then(|| row.conversation.clone())
        }));
    }
    if snapshot.active.is_some() && require_existing {
        return Err(anyhow::Error::new(WorkspaceHostError::Busy(
            snapshot.active.clone().map(Box::new),
        )));
    }
    let retained = snapshot
        .active
        .is_none()
        .then(|| {
            snapshot.conversations.iter().find_map(|row| {
                matches!(row.conversation, ConversationRef::Native { .. })
                    .then(|| row.conversation.clone())
            })
        })
        .flatten();
    if require_existing && retained.is_none() {
        anyhow::bail!("--continue found no inactive native conversation for this workspace");
    }
    Ok(retained)
}

async fn run_once(paths: &XanaPaths, surface: ChatSurface, intent: ChatIntent) -> Result<ChatRun> {
    let ChatIntent {
        resume,
        conversation_target,
        continue_chat,
        force_new,
        one_shot,
        stream_sequence,
    } = intent;
    let presentation = surface.profile();
    let desktop_restart = match &surface {
        ChatSurface::Desktop {
            bridge, workspace, ..
        } => Some(DesktopRestart {
            bridge: bridge.clone(),
            workspace: workspace.clone(),
        }),
        _ => None,
    };
    match (&surface, one_shot.is_none()) {
        (ChatSurface::Plain(mode), true) => {
            let mut output = anstream::stdout().lock();
            presentation::write_banner(&mut output, *mode)
                .context("could not write Xana banner")?;
            writeln!(
                output,
                "loading Xana config from {}",
                paths.config_file().display()
            )?;
        }
        // A full-screen surface must only be mutated through Ratatui. Direct
        // stderr output here survives the redraw and corrupts the composer.
        (ChatSurface::Tui { .. } | ChatSurface::Desktop { .. }, _) => {}
        _ => {
            writeln!(
                anstream::stderr().lock(),
                "loading Xana config from {}",
                paths.config_file().display()
            )?;
        }
    }

    let config = match XanaConfig::load_from(paths.config_file()) {
        Ok(config) => config,
        Err(error) if error.is_missing_config() => {
            anyhow::bail!(
                "Xana is not initialized at {}\nrun `xana setup` to create it",
                paths.config_file().display()
            );
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to load config from {}",
                    paths.config_file().display()
                )
            });
        }
    };
    crate::private_state::ensure_interoperable_records(paths).context(
        "could not prepare Xana's private state; run `xana config migrate --apply` and retry",
    )?;

    let manager = model_manager(paths)?;
    let child_registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load child route registry")?;
    let notification_policy = child_registry.notifications.clone();
    let selected = manager.selected()?;
    let workspace_root = surface
        .workspace()
        .map(std::path::Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
    let workspace_host = WorkspaceHost::open(paths.data_dir(), &workspace_root)?;
    let host_snapshot = workspace_host.snapshot()?;
    let exact_target = conversation_target.is_some();
    // Select identity before resolving authority or constructing tools. Implicit
    // resume and exact-ID resume retain the same historical Profile identity;
    // current settings receive a separate execution revision, never a re-freeze.
    let automatic_target = if conversation_target.is_none()
        && resume.is_none()
        && !force_new
        && (continue_chat || one_shot.is_none())
    {
        automatic_conversation(
            &host_snapshot,
            &selected.connection,
            manager.connection(&selected.connection)?.kind,
            continue_chat,
        )?
    } else {
        None
    };
    let implicit_managed = matches!(automatic_target, Some(ConversationRef::Managed { .. }));
    let conversation_target = conversation_target.or(automatic_target);
    let profile_key = conversation_target
        .as_ref()
        .and_then(ConversationRef::conversation_id)
        .map(|conversation_id| conversation_id.to_string())
        .or_else(|| resume.map(|session_id| session_id.to_string()));
    let saved_profile = profile_key
        .as_deref()
        .map(|key| {
            crate::profile::ProfileStore::open(paths)
                .resolved_snapshot(key)
                .map_err(anyhow::Error::new)
        })
        .transpose()?
        .flatten();
    let mut retained_native = match conversation_target.as_ref() {
        Some(ConversationRef::Native { session_id }) => {
            Some(DurableSession::resume(paths.data_dir(), *session_id)?)
        }
        _ => resume
            .map(|id| DurableSession::resume(paths.data_dir(), id))
            .transpose()?,
    };
    let pending = retained_native
        .as_ref()
        .is_some_and(|(session, _)| session.has_unfinished_work());
    let execution_configuration = if pending {
        retained_native
            .as_ref()
            .and_then(|(session, _)| session.execution_configuration().cloned())
            .map(Ok)
            .unwrap_or_else(|| {
                let profile = saved_profile.clone().context(
                    "Unfinished Conversation has no saved Profile; use operation recovery",
                )?;
                Ok::<_, anyhow::Error>(crate::profile::execution::ExecutionConfiguration {
                    version: 1,
                    profile,
                    inputs_digest: configuration::inputs_digest(paths)?,
                })
            })?
    } else {
        configuration::resolve(paths, saved_profile.as_ref())?
    };
    let frozen_profile = Some(execution_configuration.profile.clone());
    if exact_target
        && matches!(conversation_target, Some(ConversationRef::Managed { .. }))
        && saved_profile.is_none()
    {
        anyhow::bail!(
            "the selected managed Conversation has no frozen Profile and cannot be attached safely"
        );
    }
    let selected_connection_name = frozen_profile
        .as_ref()
        .map_or(selected.connection.as_str(), |profile| {
            profile.connection.value.as_str()
        });
    // Codex's existing in-thread model/reasoning controls are selection state,
    // not changes to the Conversation's frozen permission/capability ceiling.
    let selection_profile = frozen_profile.as_ref().filter(|_| !implicit_managed);
    let selected_model = selection_profile
        .map_or(selected.model.as_str(), |profile| {
            profile.model.value.as_str()
        })
        .to_owned();
    let selected_connection = manager.connection(selected_connection_name)
        .context("Conversation connection is unavailable; run `xana doctor` and restore a compatible connection without changing saved history")?
        .clone();
    let profile_name = frozen_profile.as_ref().map_or_else(
        || child_registry.default_profile.clone(),
        |profile| profile.name.clone(),
    );

    let XanaConfig {
        permission_rules,
        resources: resource_policy,
        ..
    } = config;
    let permission_mode = execution_configuration.profile.permission_mode.value;
    let provider_name = selected_connection_name.to_owned();
    let provider_kind = selected_connection.kind;
    let model = selected_model;
    let managed_reasoning_summary = match selection_profile {
        Some(profile) => profile
            .reasoning_summary
            .value
            .as_deref()
            .map(str::parse)
            .transpose()
            .context("frozen Profile reasoning summary is invalid")?,
        None => selected.reasoning_summary,
    };
    let managed_selection = crate::model_catalog::ModelSelection {
        connection: provider_name.clone(),
        model: model.clone(),
        reasoning_effort: selection_profile.map_or_else(
            || selected.reasoning_effort.clone(),
            |profile| profile.reasoning_effort.value.clone(),
        ),
        reasoning_summary: managed_reasoning_summary,
    };
    let artifact_store = ArtifactStore::open(paths.data_dir())?;

    if let Some(target) = &conversation_target {
        let owner_matches = match target {
            ConversationRef::Native { .. } => provider_kind != ProviderKind::Codex,
            ConversationRef::Managed { connection, .. } => {
                provider_kind == ProviderKind::Codex && connection == &provider_name
            }
            ConversationRef::NewNative | ConversationRef::NewManaged { .. } => false,
        };
        if !owner_matches {
            anyhow::bail!(
                "Conversation {target} requires its original execution owner; select a compatible model to continue here. History was not changed"
            );
        }
        if exact_target {
            let state = host_snapshot
                .conversations
                .iter()
                .find(|projection| projection.conversation == *target)
                .map(|projection| projection.state);
            match state {
                Some(crate::workspace_host::ConversationState::Inactive) => {}
                Some(state) => anyhow::bail!(
                    "Conversation {target} became {state} before control could be acquired; preview it or retry after its active Run stops"
                ),
                None => {
                    anyhow::bail!("Conversation {target} is no longer retained in this workspace")
                }
            }
        }
    }
    let resume = conversation_target
        .as_ref()
        .and_then(|target| match target {
            ConversationRef::Native { session_id } => Some(*session_id),
            _ => None,
        })
        .or(resume);
    if provider_kind != ProviderKind::Codex
        && (resume.is_some() || continue_chat)
        && host_snapshot.active.is_some()
    {
        return Err(anyhow::Error::new(WorkspaceHostError::Busy(
            host_snapshot.active.clone().map(Box::new),
        )));
    }
    let conversation = if let Some(target) = conversation_target {
        target
    } else if provider_kind == ProviderKind::Codex {
        ConversationRef::NewManaged {
            conversation_id: resume.map_or_else(ConversationId::new, ConversationId::for_native),
            connection: provider_name.clone(),
        }
    } else {
        resume.map_or(ConversationRef::NewNative, |session_id| {
            ConversationRef::Native { session_id }
        })
    };
    if provider_kind == ProviderKind::Codex {
        let profile_skills = frozen_profile.as_ref().map_or_else(
            || {
                child_registry
                    .profiles
                    .get(&child_registry.default_profile)
                    .map(|profile| profile.skills.clone())
                    .unwrap_or_default()
            },
            |profile| profile.skills.value.clone(),
        );
        let plugin_revisions = match &frozen_profile {
            Some(profile) => profile.plugin_revisions.clone(),
            None => {
                let plugins = child_registry
                    .profiles
                    .get(&child_registry.default_profile)
                    .map(|profile| profile.plugins.as_slice())
                    .unwrap_or_default();
                let (revisions, readiness) = crate::plugin::PluginManager::open(paths)
                    .resolve_profile_plugins(
                        plugins,
                        &crate::plugin::PluginScope::Profile {
                            project: None,
                            profile: child_registry.default_profile.clone(),
                        },
                    )?;
                if !readiness.is_empty() {
                    anyhow::bail!(
                        "default profile plugin requirements are not ready: {}",
                        readiness.join("; ")
                    );
                }
                revisions
            }
        };
        let plugin_skill_sources = crate::plugin::PluginManager::open(paths)
            .skill_sources_for_revisions(&plugin_revisions)?;
        let skill_catalog = super::skills::catalog(&workspace_root, plugin_skill_sources)?;
        let activated_skills = skill_catalog
        .activate_all(profile_skills.iter().map(String::as_str))
        .context(
            "profile Agent Skill activation failed; run `xana skill list` and qualify collisions",
        )?;
        let managed_skill_instructions = activated_skills
            .iter()
            .map(crate::skill::ActivatedSkill::prompt_text)
            .collect::<Vec<_>>()
            .join("\n\n");

        if resume.is_some() && frozen_profile.is_none() {
            anyhow::bail!(
                "Xana durable --resume applies to native conversations or a planned managed continuation with a frozen profile; Codex owns ordinary managed thread resume"
            )
        }
        if saved_profile.is_none() {
            let conversation_id = conversation
                .conversation_id()
                .expect("managed conversations always have a Xana identity");
            crate::profile::ProfileStore::open(paths).freeze(
                &conversation_id.to_string(),
                &execution_configuration.profile,
            )?;
        }
        let mut server = CodexAppServer::spawn(&codex_launch(&selected_connection)).await?;
        server.set_usage_budget(super::usage_commands::compose_budget(
            paths,
            conversation
                .conversation_id()
                .expect("managed identity")
                .to_string(),
            crate::usage_budget::DispatchFacts {
                owner: Some("managed_codex".into()),
                connection: Some(provider_name.clone()),
                model: Some(model.clone()),
                profile: Some(profile_name.clone()),
                reasoning: managed_selection.reasoning_effort.clone(),
                project: None,
            },
        )?);
        let developer_instructions = if managed_skill_instructions.is_empty() {
            crate::prompt::xana_identity().to_owned()
        } else {
            format!(
                "{}\n\n<activated_agent_skills>\n{}\n</activated_agent_skills>",
                crate::prompt::xana_identity(),
                managed_skill_instructions
            )
        };
        let memory = super::memory_commands::compose(
            paths,
            &artifact_store,
            &conversation
                .conversation_id()
                .context("managed memory needs Conversation identity")?
                .to_string(),
            execution_configuration.profile.profile_id,
        )?;
        let managed_config = ManagedChatConfig {
            memory,
            permission_default: permission_mode.into(),
            permission_rules,
            connection: provider_name,
            model,
            profile_name,
            selection: managed_selection,
            workspace: workspace_root,
            data_root: paths.data_dir().to_owned(),
            artifact_store,
            owner: crate::identity::PrincipalId::new(),
            developer_instructions,
            identity_version: crate::prompt::XANA_IDENTITY_VERSION,
            presentation,
            resource_policy,
        };
        return match one_shot {
            Some(OneShotInput { input, contract }) => {
                anyhow::ensure!(
                    contract.conditions.is_empty(),
                    "declared completion checks are unavailable for managed Codex; no vendor turn was started"
                );
                let conversation_id = conversation
                    .conversation_id()
                    .expect("composed managed one-shot has a Conversation identity");
                let mut event_output: Box<dyn Write> = if stream_sequence.is_some() {
                    Box::new(anstream::stdout())
                } else {
                    Box::new(anstream::stderr())
                };
                let mut reporter = match stream_sequence {
                    Some(sequence) => OneShotReporter::stream_json(
                        event_output.as_mut(),
                        sequence,
                        "managed_codex",
                        conversation_id,
                    ),
                    None => OneShotReporter::text(event_output.as_mut()),
                };
                run_codex_one_shot(
                    server,
                    manager,
                    managed_config,
                    ManagedOneShotRequest {
                        input,
                        continue_thread: continue_chat,
                        conversation,
                    },
                    &mut reporter,
                    &workspace_host,
                )
                .await
                .map(|result| ChatRun::Complete(Some(result)))
                .map_err(anyhow::Error::new)
            }
            None => {
                let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
                let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
                let retained_conversation = conversation.clone();
                let (exit, tui_continuation) = match surface {
                    ChatSurface::Plain(_) => {
                        let exit = run_codex_chat(
                            server,
                            manager,
                            managed_config,
                            workspace_host,
                            conversation,
                        )
                        .await?;
                        (exit, None)
                    }
                    ChatSurface::Tui { prepared, .. } => {
                        let outcome = tui::run_managed(
                            prepared,
                            server,
                            manager,
                            managed_config,
                            workspace_host,
                            conversation,
                        )
                        .await?;
                        (outcome.exit, Some(outcome.continuation))
                    }
                    ChatSurface::Hosted { bind, port, .. } => {
                        crate::local_host::run_managed_host(
                            paths.runtime_dir(),
                            bind,
                            port,
                            crate::local_host::ManagedHostExecution {
                                server,
                                models: manager,
                                config: managed_config,
                                workspace_host,
                                conversation,
                            },
                        )
                        .await?;
                        (ChatExit::Quit, None)
                    }
                    ChatSurface::Desktop { bridge, .. } => {
                        let exit = crate::desktop::run_managed(
                            server,
                            manager,
                            managed_config,
                            workspace_host,
                            conversation,
                            bridge,
                            paths,
                            notification_policy,
                        )
                        .await?;
                        (exit, None)
                    }
                };
                Ok(ChatRun::Exited(Box::new(ChatExitState {
                    exit,
                    presentation,
                    restart_tui,
                    tui_required,
                    tui_continuation,
                    desktop_restart,
                    conversation: retained_conversation,
                })))
            }
        };
    }

    let (mut session, resumed, repair_truncate_to, unfinished, restored_children) =
        match retained_native.take() {
            Some((session, summary)) => {
                anyhow::ensure!(
                    session.workspace_root() == workspace_root,
                    "Conversation workspace differs; no history was changed"
                );
                (
                    session,
                    true,
                    summary.repair_truncate_to,
                    summary.unfinished,
                    summary.children,
                )
            }
            None => (
                DurableSession::create(paths.data_dir(), workspace_root.clone())?,
                false,
                None,
                Vec::new(),
                Vec::new(),
            ),
        };
    if saved_profile.is_none()
        && let Err(error) = crate::profile::ProfileStore::open(paths).freeze(
            &session.session_id().to_string(),
            &execution_configuration.profile,
        )
    {
        if !resumed {
            session.discard_unstarted()?;
        }
        return Err(error).context("could not record initial Conversation Profile");
    }
    let artifact_owner = session.artifact_owner();
    let browser = artifact_store.protected_home().map(|store| {
        crate::browser::BrowserOwner::new(
            paths.clone(),
            store.clone(),
            artifact_owner,
            session.session_id(),
        )
    });
    let composed = native::compose(
        paths,
        &session,
        execution_configuration.clone(),
        browser.clone(),
    )
    .await?;
    anyhow::ensure!(
        pending || execution_configuration.inputs_digest == configuration::inputs_digest(paths)?,
        "Settings changed during preparation; retry startup. Conversation history is retained."
    );
    let resume_configuration_error = if pending
        && (session.execution_configuration().is_none()
            || !configuration::resolve(paths, Some(&execution_configuration.profile))
                .is_ok_and(|current| current == execution_configuration))
    {
        Some("This interrupted operation's execution settings cannot be reconstructed after configuration changes. Choose Stop (not Continue), then send a new turn in this same Conversation. No effects were replayed.".to_owned())
    } else {
        None
    };
    if !pending {
        session.configure_execution(execution_configuration)?;
    }
    let native::Composed {
        execution,
        endpoint,
        mut context_report,
        vision,
    } = composed;
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let round_budget_suspension = session.round_budget_suspension();
    if pending {
        context_report.push_str("\nUnfinished work retains its execution settings. Stop or reconcile it before applying newer settings.\n");
    }
    if execution.memory.is_some() {
        context_report.push_str(crate::memory::learning::DISCLOSURE);
    } else {
        context_report.push_str(crate::memory::UNAVAILABLE_NOTICE);
    }
    let refresh = Arc::new(configuration::Refresh {
        paths: paths.clone(),
        browser: browser.clone(),
    });
    let runtime =
        RuntimeHandle::spawn_configurable(execution, session, refresh, resume_configuration_error)?
            .with_browser(browser);
    let conversation = match conversation {
        ConversationRef::NewNative => ConversationRef::Native { session_id },
        conversation => conversation,
    };
    let header = ChatHeader {
        provider_name,
        model,
        profile_name,
        permission_mode,
        endpoint,
        context_report,
        session_id,
        session_path,
        resumed,
        repair_truncate_to,
        unfinished,
        round_budget_suspension,
        children: restored_children,
        workspace_root: workspace_root.clone(),
        artifact_store,
        owner: artifact_owner,
        models: manager,
        presentation,
        resource_policy,
        notification_policy,
        vision,
    };

    if let Some(OneShotInput { input, contract }) = one_shot {
        let conversation_id = conversation
            .conversation_id()
            .expect("composed native one-shot has a Conversation identity");
        let mut event_output: Box<dyn Write> = if stream_sequence.is_some() {
            Box::new(anstream::stdout())
        } else {
            Box::new(anstream::stderr())
        };
        let mut reporter = match stream_sequence {
            Some(sequence) => OneShotReporter::stream_json(
                event_output.as_mut(),
                sequence,
                "native",
                conversation_id,
            ),
            None => OneShotReporter::text(event_output.as_mut()),
        };
        return plain_terminal::run_one_shot(
            runtime,
            &header,
            input,
            &mut reporter,
            &workspace_host,
            conversation,
            contract,
        )
        .await
        .map(|result| ChatRun::Complete(Some(result)))
        .map_err(anyhow::Error::new);
    }

    let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
    let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
    let retained_conversation = conversation.clone();
    let (exit, tui_continuation) = match surface {
        ChatSurface::Plain(_) => {
            let exit =
                plain_terminal::run_chat(runtime, header, workspace_host, conversation, paths)
                    .await?;
            (exit, None)
        }
        ChatSurface::Tui { prepared, .. } => {
            let outcome =
                tui::run_native(prepared, runtime, &header, workspace_host, conversation).await?;
            (outcome.exit, Some(outcome.continuation))
        }
        ChatSurface::Hosted { bind, port, .. } => {
            crate::local_host::run_native_host(
                paths.runtime_dir(),
                bind,
                port,
                runtime,
                header,
                workspace_host,
                conversation,
            )
            .await?;
            (ChatExit::Quit, None)
        }
        ChatSurface::Desktop { bridge, .. } => {
            let exit = crate::desktop::run_native(
                runtime,
                &header,
                workspace_host,
                conversation,
                bridge,
                paths,
            )
            .await?;
            (exit, None)
        }
    };
    Ok(ChatRun::Exited(Box::new(ChatExitState {
        exit,
        presentation,
        restart_tui,
        tui_required,
        tui_continuation,
        desktop_restart,
        conversation: retained_conversation,
    })))
}

async fn continue_after_chat_exit(
    paths: &XanaPaths,
    state: ChatExitState,
) -> Result<Option<ChatRestart>> {
    let ChatExitState {
        exit,
        mut presentation,
        restart_tui,
        tui_required,
        tui_continuation,
        desktop_restart,
        conversation: current_conversation,
    } = state;
    if exit == ChatExit::Quit {
        return Ok(None);
    }
    if let ChatExit::ControlCommand { family, arguments } = &exit
        && family == "storage"
        && arguments.trim() == "lock"
    {
        // The execution owner has finished its bounded shutdown. Drop retained
        // terminal drafts/selections before releasing keys. A different live
        // owner still makes the lifecycle lease report busy, never false Locked.
        drop(tui_continuation);
        drop(desktop_restart);
        super::storage_commands::run(
            &cli::StorageCommand::Lock,
            paths,
            &mut std::io::stdout().lock(),
        )?;
        return Ok(None);
    }
    let mut force_new_conversation = matches!(
        exit,
        ChatExit::NewConversation | ChatExit::DesktopNewConversation { .. }
    );
    let mut conversation_target = match &exit {
        ChatExit::SwitchConversation(conversation) => Some(conversation.clone()),
        ChatExit::DesktopSwitchConversation { conversation, .. } => Some(conversation.clone()),
        _ => Some(current_conversation),
    };
    let mut continue_chat = false;
    if let ChatExit::ControlCommand { family, arguments } = &exit {
        if matches!(family.as_str(), "conversation" | "session" | "sessions")
            && arguments.trim() == "new"
        {
            force_new_conversation = true;
        } else if matches!(family.as_str(), "conversation" | "session" | "sessions")
            && arguments.trim() == "continue"
        {
            continue_chat = true;
        } else if matches!(family.as_str(), "conversation" | "session" | "sessions")
            && arguments.trim_start().starts_with("attach ")
        {
            let selector = arguments
                .trim_start()
                .strip_prefix("attach ")
                .expect("checked prefix")
                .trim();
            match super::sessions::resolve_attach_target(paths, selector) {
                Ok(conversation) => conversation_target = Some(conversation),
                Err(error) => eprintln!("xana: {error:#}"),
            }
        } else if let Err(error) =
            run_chat_control_command(paths, family, arguments, &mut std::io::stdout().lock()).await
        {
            eprintln!("xana: {error:#}");
        }
    }
    if let ChatExit::Setup(request) = &exit {
        let mut args = crate::setup::args_for_request(request)?;
        args.plain = !restart_tui;
        run_setup_command(&args, paths).await?;
    }
    if let ChatExit::Settings(request) = &exit {
        let settings_profile = super::resolved_presentation(paths, true, true);
        tui::run_settings(
            paths,
            (!request.is_empty()).then_some(request.as_str()),
            None,
            settings_profile,
        )?;
        presentation = super::resolved_presentation(paths, true, true);
    }
    let doctor_resume = if let ChatExit::Doctor(session_id) = &exit {
        run_doctor_command(&cli::DoctorArgs::default(), paths).await?;
        *session_id
    } else {
        None
    };
    if exit == ChatExit::Reset {
        run_reset_command(&cli::ResetArgs::default(), paths)?;
        if XanaConfig::load_from(paths.config_file()).is_err() {
            return Ok(None);
        }
    }
    if force_new_conversation {
        conversation_target = None;
    }
    let restart_surface = if let Some(mut desktop) = desktop_restart {
        match &exit {
            ChatExit::DesktopNewConversation { workspace }
            | ChatExit::DesktopSwitchConversation { workspace, .. } => {
                desktop.workspace = workspace.clone();
            }
            _ => {}
        }
        ChatSurface::Desktop {
            bridge: desktop.bridge,
            presentation,
            workspace: desktop.workspace,
        }
    } else if restart_tui {
        let preferences =
            presentation::PresentationPreferences::load(&paths.presentation_file()).preferences;
        let presentation = super::resolved_presentation(paths, true, true);
        match tui::prepare(
            presentation,
            preferences,
            paths.presentation_file(),
            paths.clone(),
        ) {
            Ok(prepared) => ChatSurface::Tui {
                prepared: prepared.with_continuation(tui_continuation),
                required: tui_required,
            },
            Err(error) if !tui_required => {
                eprintln!(
                    "xana: could not restart the full-screen terminal ({error}); falling back to --plain"
                );
                ChatSurface::Plain(BannerMode::hidden(presentation))
            }
            Err(error) => {
                return Err(error).context("could not restart the required Xana TUI");
            }
        }
    } else {
        ChatSurface::Plain(BannerMode::hidden(presentation))
    };
    Ok(Some(ChatRestart {
        surface: restart_surface,
        resume: doctor_resume,
        conversation_target,
        continue_chat,
        force_new: force_new_conversation,
    }))
}

pub(super) async fn run_chat_control_command<W: Write>(
    paths: &XanaPaths,
    family: &str,
    arguments: &str,
    output: &mut W,
) -> Result<()> {
    if arguments.len() > 16 * 1024 {
        anyhow::bail!("control command exceeds the 16 KiB input limit");
    }
    let values = shlex::split(arguments)
        .ok_or_else(|| anyhow::anyhow!("control command contains an unterminated quote"))?;
    if values.len() > 128 {
        anyhow::bail!("control command exceeds the 128-argument limit");
    }
    let arguments = std::iter::once("xana".to_owned())
        .chain(std::iter::once(family.to_owned()))
        .chain(values)
        .collect::<Vec<_>>();
    let command = cli::Cli::try_parse_from(arguments)
        .map_err(|error| anyhow::anyhow!(error.render().ansi().to_string()))?
        .command;
    match command {
        Some(cli::Command::Budget(args)) => super::usage_commands::budget(args, paths, output),
        Some(cli::Command::Memory(args)) => super::memory_commands::run(args, paths, output).await,
        Some(cli::Command::Worker(args)) => {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let execution =
                super::worker_commands::execute(paths.clone(), args.command, cancellation.clone());
            tokio::pin!(execution);
            let value = tokio::select! {
                result=&mut execution=>result?,
                interrupted=tokio::signal::ctrl_c()=>{interrupted?;cancellation.cancel();execution.await?}
            };
            writeln!(output, "{}", serde_json::to_string_pretty(&value)?)?;
            Ok(())
        }
        Some(cli::Command::Autonomy(args)) => {
            super::autonomy_commands::control(args, paths, output)
        }
        Some(cli::Command::Usage(args)) => super::usage_commands::run(args, paths, output).await,
        Some(cli::Command::Storage(args)) => {
            super::storage_commands::run(&args.command, paths, output)
        }
        Some(cli::Command::Project(args)) => {
            super::projects::run_command(args.command, paths, output)
        }
        Some(cli::Command::Profile(args)) => {
            super::profiles::run_command(args.command, paths, output)
        }
        Some(cli::Command::Skill(args)) => super::skills::run_command(args.command, paths, output),
        Some(cli::Command::Plugin(args)) => {
            super::plugins::run_command(args.command, paths, output)
        }
        Some(cli::Command::Mcp(args)) => {
            super::mcp_commands::run(args.command, paths, output).await
        }
        Some(cli::Command::ExternalAgent(args)) => {
            super::external_agents::run(args.command, paths, output).await
        }
        Some(cli::Command::Image(args)) => {
            let stdin = std::io::stdin();
            super::image_commands::run(args.command, paths, &mut stdin.lock(), output).await
        }
        Some(cli::Command::Connection(args)) => {
            super::run_connection_command(args.command, paths, output, args.json).await
        }
        Some(cli::Command::Logs(args)) => {
            super::diagnostics_commands::run(args.command, paths, output)
        }
        Some(cli::Command::Outbound(args)) => {
            super::outbound_commands::run(args.command, paths, output)
        }
        Some(cli::Command::Operation(args)) => {
            super::operations::run_operation(args.command, paths, output).await
        }
        Some(cli::Command::Route(args)) => {
            super::operations::run_route(args.command, paths, output)
        }
        Some(cli::Command::Connect(args)) => super::run_connect_command(args, paths, output).await,
        Some(cli::Command::Session(args))
            if !matches!(
                &args.command,
                cli::SessionCommand::New
                    | cli::SessionCommand::Continue
                    | cli::SessionCommand::Attach { .. }
            ) =>
        {
            super::sessions::run_command(args.command, paths, output)
        }
        Some(cli::Command::Capabilities(args)) => super::capabilities::run(args, paths, output),
        _ => {
            anyhow::bail!("this command is not available from the interactive management path")
        }
    }
}
