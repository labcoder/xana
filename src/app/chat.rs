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
    one_shot: Option<String>,
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
    one_shot: Option<String>,
    stream_sequence: Option<StreamSequence>,
    conversation_target: Option<ConversationRef>,
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
            ChatRun::Exited {
                exit,
                presentation,
                restart_tui,
                tui_required,
                tui_continuation,
                desktop_restart,
            } => {
                let Some(restart) = continue_after_chat_exit(
                    paths,
                    exit,
                    presentation,
                    restart_tui,
                    tui_required,
                    tui_continuation,
                    desktop_restart,
                )
                .await?
                else {
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
    Exited {
        exit: ChatExit,
        presentation: presentation::ResolvedPresentation,
        restart_tui: bool,
        tui_required: bool,
        tui_continuation: Option<tui::TuiContinuation>,
        desktop_restart: Option<DesktopRestart>,
    },
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
    one_shot: Option<String>,
    stream_sequence: Option<StreamSequence>,
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
    let profile_key = conversation_target
        .as_ref()
        .and_then(ConversationRef::conversation_id)
        .map(|conversation_id| conversation_id.to_string())
        .or_else(|| resume.map(|session_id| session_id.to_string()));
    let frozen_profile = profile_key
        .as_deref()
        .map(|key| {
            crate::profile::ProfileStore::open(paths)
                .snapshot(key)
                .map_err(anyhow::Error::new)
        })
        .transpose()?
        .flatten()
        .map(|snapshot| {
            serde_json::from_value::<crate::profile::ResolvedProfile>(snapshot.resolved)
                .context("frozen profile snapshot is invalid")
        })
        .transpose()?;
    let selected = manager.selected()?;
    if matches!(conversation_target, Some(ConversationRef::Managed { .. }))
        && frozen_profile.is_none()
    {
        anyhow::bail!(
            "the selected managed Conversation has no frozen Profile and cannot be attached safely"
        );
    }
    let launch_profile = if frozen_profile.is_none() {
        Some(
            crate::profile::ProfileStore::open(paths)
                .resolve_global_for_selection(&child_registry.default_profile, &selected)?,
        )
    } else {
        None
    };
    let selected_connection_name = frozen_profile
        .as_ref()
        .map_or(selected.connection.as_str(), |profile| {
            profile.connection.value.as_str()
        });
    let selected_model = frozen_profile
        .as_ref()
        .map_or(selected.model.as_str(), |profile| {
            profile.model.value.as_str()
        })
        .to_owned();
    let selected_connection = manager.connection(selected_connection_name)?.clone();
    let profile_name = frozen_profile.as_ref().map_or_else(
        || child_registry.default_profile.clone(),
        |profile| profile.name.clone(),
    );

    let XanaConfig {
        mut permission_mode,
        permission_rules,
        shell,
        mut max_tool_rounds,
        context: prompt_budget_policy,
        resources: resource_policy,
        ..
    } = config;
    if let Some(profile) = &frozen_profile {
        permission_mode = profile.permission_mode.value;
        max_tool_rounds = profile.max_tool_rounds.value;
    }
    let provider_name = selected_connection_name.to_owned();
    let provider_kind = selected_connection.kind;
    let model = selected_model;
    let managed_reasoning_summary = match &frozen_profile {
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
        reasoning_effort: frozen_profile.as_ref().map_or_else(
            || selected.reasoning_effort.clone(),
            |profile| profile.reasoning_effort.value.clone(),
        ),
        reasoning_summary: managed_reasoning_summary,
    };
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let workspace_root = surface
        .workspace()
        .map(std::path::Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
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
    let plugin_skill_sources =
        crate::plugin::PluginManager::open(paths).skill_sources_for_revisions(&plugin_revisions)?;
    let skill_catalog = super::skills::catalog(&workspace_root, plugin_skill_sources)?;
    let activated_skills = skill_catalog
        .activate_all(profile_skills.iter().map(String::as_str))
        .context(
            "profile Agent Skill activation failed; run `xana skill list` and qualify collisions",
        )?;
    let skill_sources = activated_skills
        .iter()
        .map(crate::skill::ActivatedSkill::context_source)
        .collect::<Vec<_>>();
    let managed_skill_instructions = activated_skills
        .iter()
        .map(crate::skill::ActivatedSkill::prompt_text)
        .collect::<Vec<_>>()
        .join("\n\n");
    let artifact_store = ArtifactStore::open(paths.data_dir())?;

    let workspace_host = WorkspaceHost::open(paths.data_dir(), &workspace_root)?;
    debug_assert_eq!(workspace_host.workspace(), workspace_root);
    let host_snapshot = workspace_host.snapshot()?;
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
                "Conversation {target} does not match the execution owner in its frozen Profile"
            );
        }
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
            None => anyhow::bail!("Conversation {target} is no longer retained in this workspace"),
        }
    }
    let resume = conversation_target
        .as_ref()
        .and_then(|target| match target {
            ConversationRef::Native { session_id } => Some(*session_id),
            _ => None,
        })
        .or(resume);
    let resume = if provider_kind != ProviderKind::Codex {
        if (resume.is_some() || continue_chat) && host_snapshot.active.is_some() {
            return Err(anyhow::Error::new(WorkspaceHostError::Busy(
                host_snapshot.active.clone().map(Box::new),
            )));
        }
        if !force_new && resume.is_none() && (continue_chat || one_shot.is_none()) {
            let latest = if host_snapshot.active.is_none() {
                DurableSession::latest_for_workspace(paths.data_dir(), &workspace_root)?
            } else {
                None
            };
            if continue_chat && latest.is_none() {
                anyhow::bail!(
                    "--continue found no inactive native conversation for this workspace"
                );
            }
            if one_shot.is_none() && host_snapshot.active.is_some() {
                writeln!(
                    anstream::stdout().lock(),
                    "another Xana root is active in this workspace; opening a new inactive conversation for drafting. Submitting work waits until the controlling root ends"
                )?;
            }
            latest
        } else {
            resume
        }
    } else {
        resume
    };
    let conversation = if let Some(target) = conversation_target {
        target
    } else if provider_kind == ProviderKind::Codex {
        let current = (resume.is_none() && !force_new && (one_shot.is_none() || continue_chat))
            .then(|| {
                host_snapshot.conversations.iter().find_map(|projection| {
                    match &projection.conversation {
                        ConversationRef::Managed {
                            conversation_id,
                            connection,
                            thread_id,
                        } if connection == &provider_name && projection.selected => {
                            Some(ConversationRef::Managed {
                                conversation_id: *conversation_id,
                                connection: connection.clone(),
                                thread_id: thread_id.clone(),
                            })
                        }
                        _ => None,
                    }
                })
            })
            .flatten();
        current.unwrap_or_else(|| ConversationRef::NewManaged {
            conversation_id: resume.map_or_else(ConversationId::new, ConversationId::for_native),
            connection: provider_name.clone(),
        })
    } else {
        resume.map_or(ConversationRef::NewNative, |session_id| {
            ConversationRef::Native { session_id }
        })
    };
    if provider_kind == ProviderKind::Codex {
        if resume.is_some() && frozen_profile.is_none() {
            anyhow::bail!(
                "Xana durable --resume applies to native conversations or a planned managed continuation with a frozen profile; Codex owns ordinary managed thread resume"
            )
        }
        if frozen_profile.is_none() {
            let conversation_id = conversation
                .conversation_id()
                .expect("managed conversations always have a Xana identity");
            crate::profile::ProfileStore::open(paths).freeze(
                &conversation_id.to_string(),
                launch_profile
                    .as_ref()
                    .expect("fresh launches resolve one Profile"),
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
            frozen_profile
                .as_ref()
                .or(launch_profile.as_ref())
                .context("memory needs a resolved Profile")?
                .profile_id,
        )?;
        let managed_config = ManagedChatConfig {
            memory,
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
            Some(input) => {
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
                Ok(ChatRun::Exited {
                    exit,
                    presentation,
                    restart_tui,
                    tui_required,
                    tui_continuation,
                    desktop_restart,
                })
            }
        };
    }

    let descriptor = manager
        .descriptor(&provider_name, &model)
        .context("could not resolve selected model metadata for prompt planning")?;
    let prompt_budget = PromptBudgetPlan::derive(
        &prompt_budget_policy,
        ModelBudgetFacts {
            connection: provider_name.clone(),
            model: model.clone(),
            context_tokens: descriptor.context_tokens,
            max_output_tokens: descriptor.max_output_tokens,
            reasoning: descriptor.reasoning == Some(true),
        },
    )
    .context("could not derive a safe native prompt budget")?;

    let (provider, endpoint) =
        compose_native_provider(&selected_connection, &model, artifact_store.clone(), false)
            .map_err(anyhow::Error::msg)?;
    let mut tools =
        ToolRegistry::builtins(shell.clone()).context("could not build tool registry")?;
    let (profile_mcp_servers, profile_mcp_allowlists, profile_egress) =
        frozen_profile.as_ref().map_or_else(
            || {
                child_registry
                    .profiles
                    .get(&child_registry.default_profile)
                    .map(|profile| {
                        let egress = profile
                            .egress_policy
                            .as_deref()
                            .and_then(|policy| child_registry.egress_policies.get(policy))
                            .map(|policy| policy.allowed.clone())
                            .unwrap_or_default();
                        (
                            profile.mcp_servers.clone(),
                            profile.mcp_allowlists.clone(),
                            egress,
                        )
                    })
                    .unwrap_or_default()
            },
            |profile| {
                (
                    profile.mcp_servers.value.clone(),
                    profile.mcp_allowlists.value.clone(),
                    profile.egress.value.clone(),
                )
            },
        );
    let profile_external_agents = frozen_profile.as_ref().map_or_else(
        || {
            child_registry
                .profiles
                .get(&child_registry.default_profile)
                .map(|profile| profile.external_agents.clone())
                .unwrap_or_default()
        },
        |profile| profile.external_agents.value.clone(),
    );
    super::mcp_commands::activate_profile_tools(
        &child_registry,
        paths,
        &workspace_root,
        &profile_mcp_servers,
        &profile_mcp_allowlists,
        &profile_egress,
        &mut tools,
    )
    .await?;
    let (session, permission_policy, resumed, repair_truncate_to, unfinished, restored_children) =
        match resume {
            Some(session_id) => {
                let (session, summary) = DurableSession::resume(paths.data_dir(), session_id)?;
                if session.workspace_root() != workspace_root {
                    anyhow::bail!(
                        "session {session_id} belongs to workspace {}; current workspace is {}",
                        session.workspace_root().display(),
                        workspace_root.display()
                    );
                }
                let unfinished = summary.unfinished.clone();
                let permission_policy = PermissionPolicy::new(
                    permission_mode.into(),
                    permission_rules.clone(),
                    session.workspace_root(),
                )
                .context("could not resolve permission policy for the session workspace")?;
                (
                    session,
                    permission_policy,
                    true,
                    summary.repair_truncate_to,
                    unfinished,
                    summary.children,
                )
            }
            None => {
                let permission_policy = PermissionPolicy::new(
                    permission_mode.into(),
                    permission_rules.clone(),
                    &workspace_root,
                )
                .context("could not resolve permission policy for the launch workspace")?;
                (
                    DurableSession::create(paths.data_dir(), workspace_root.clone())?,
                    permission_policy,
                    false,
                    None,
                    Vec::new(),
                    Vec::new(),
                )
            }
        };
    if frozen_profile.is_none()
        && let Err(error) = crate::profile::ProfileStore::open(paths).freeze(
            &session.session_id().to_string(),
            launch_profile
                .as_ref()
                .expect("fresh launches resolve one Profile"),
        )
    {
        if !resumed {
            session.discard_unstarted().with_context(|| {
                format!("Profile freeze failed ({error}); empty session cleanup also failed")
            })?;
        }
        return Err(error).context("could not freeze the Conversation Profile");
    }
    let workspace_root = session.workspace_root().to_owned();
    if let Some(store) = artifact_store.protected_home() {
        tools
            .register(crate::recall::RecallTool {
                owner: crate::recall::RecallOwner {
                    store: store.clone(),
                    paths: paths.clone(),
                    conversation: session.session_id(),
                },
                route: crate::session::compaction::semantic::route_digest(
                    &selected_connection,
                    &model,
                ),
            })
            .context("could not register bounded Project recall")?;
    }
    let artifact_owner = session.artifact_owner();
    let browser = artifact_store.protected_home().map(|store| {
        crate::browser::BrowserOwner::new(paths.clone(), store.clone(), artifact_owner)
    });
    if let Some(owner) = browser.as_ref().filter(|owner| owner.snapshot().available) {
        crate::browser::register_tools(
            &mut tools,
            owner.clone(),
            profile_egress.iter().copied().collect(),
        )
        .context("could not activate the dedicated local browser")?;
    }
    crate::a2a::activate_profile_delegation_tools(
        &child_registry,
        crate::a2a::A2aDelegationActivation {
            paths,
            workspace: &workspace_root,
            external_agents: &profile_external_agents,
            profile_egress: &profile_egress,
            artifacts: artifact_store.clone(),
            owner: artifact_owner,
        },
        &mut tools,
    )
    .context("could not activate profile external-agent delegation tools")?;
    let profile_service_routes = frozen_profile.as_ref().map_or_else(
        || {
            child_registry
                .profiles
                .get(&child_registry.default_profile)
                .map(|profile| profile.service_routes.clone())
                .unwrap_or_default()
        },
        |profile| profile.service_routes.value.clone(),
    );
    crate::focused_service::activate_profile_image_tool(
        &child_registry,
        &profile_service_routes,
        &profile_egress,
        artifact_store.clone(),
        artifact_owner,
        paths,
        &mut tools,
    )
    .map_err(anyhow::Error::msg)
    .context("could not activate profile image-generation tool")?;
    let vision = super::vision::VisionTurnService::new(
        child_registry.clone(),
        crate::outbound::OutboundGuard::open(paths)
            .context("could not open the outbound policy gate for vision")?,
        profile_service_routes.clone(),
        profile_egress.clone(),
        permission_mode,
        artifact_store.clone(),
        artifact_owner,
    )
    .with_outbound_audit(crate::diagnostics::outbound_audit(paths)?);
    let restored_plans = session.started_orchestration_plans();
    let child_supervisor = if child_registry.routes.is_empty() {
        None
    } else {
        let root_profile = child_registry
            .profiles
            .get(&child_registry.default_profile)
            .context("validated configuration lost its default profile")?;
        let budget = OrchestrationBudget::new(
            root_profile.orchestration.clone(),
            root_profile.max_tool_rounds,
        );
        let factory = ChildExecutionOwnerFactory::new(
            child_registry,
            model_manager(paths)?,
            shell,
            workspace_root.clone(),
            artifact_store.clone(),
            permission_rules,
        )
        .with_usage_budget(super::usage_commands::compose_budget(
            paths,
            session.session_id().to_string(),
            crate::usage_budget::DispatchFacts::default(),
        )?);
        let (handle, supervisor) = ChildSupervisor::with_restored(
            ParentExecution {
                agent_id: session.agent_id(),
                thread_id: session.thread_id(),
            },
            Arc::new(factory),
            restored_children.clone(),
            restored_plans,
            budget,
            artifact_store.clone(),
            artifact_owner,
        );
        let supervisor =
            supervisor.with_consumed_reservations(&session.orchestration_reservations()?);
        tools
            .enable_child_delegation(handle.clone())
            .context("could not register child delegation tool")?;
        Some((handle, supervisor))
    };
    let environment = PromptEnvironment {
        connection: provider_name.clone(),
        model: model.clone(),
        operating_system: std::env::consts::OS.to_owned(),
        working_directory: workspace_root.clone(),
        configured_shell,
        surface: PromptSurface::Cli,
    };
    let definitions = tools.definitions().into_iter().cloned().collect::<Vec<_>>();
    let prompt_assembler = PromptAssembler::new(
        definitions,
        environment,
        Some(ProductDocumentationHint {
            capability: "xana_docs".to_owned(),
            references: crate::self_docs::default_catalog()
                .list(None)
                .into_iter()
                .map(|entry| entry.id.to_owned())
                .collect(),
        }),
        ContextBudget {
            total_tokens: prompt_budget.input_budget_tokens,
            conversation_reserve_tokens: prompt_budget.conversation_reserve_tokens,
        },
    )
    .with_budget_plan(prompt_budget)
    .with_context_sources(skill_sources);
    let prompt = prompt_assembler
        .assemble(&[])
        .context("could not assemble Xana base prompt")?;
    let mut context_report = ContextPlanReport::render(&prompt.context_plan)
        .as_str()
        .to_owned();
    context_report.push('\n');
    context_report.push_str(crate::memory::learning::DISCLOSURE);
    let agent = Agent::new(
        provider,
        tools,
        workspace_root.clone(),
        prompt,
        max_tool_rounds,
    )
    .with_runtime_telemetry(crate::diagnostics::runtime_telemetry())
    .with_semantic_compaction(super::sessions::semantic_policy(
        paths,
        &selected_connection,
        &model,
    )?)
    .with_usage_budget(super::usage_commands::compose_budget(
        paths,
        session.session_id().to_string(),
        crate::usage_budget::DispatchFacts {
            owner: Some("native".into()),
            connection: Some(provider_name.clone()),
            model: Some(model.clone()),
            profile: Some(profile_name.clone()),
            reasoning: selected.reasoning_effort.clone(),
            project: None,
        },
    )?);
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let round_budget_suspension = session.round_budget_suspension();
    let memory = super::memory_commands::compose(
        paths,
        &artifact_store,
        &session_id.to_string(),
        frozen_profile
            .as_ref()
            .or(launch_profile.as_ref())
            .context("memory needs a resolved Profile")?
            .profile_id,
    )?;
    let runtime = match child_supervisor {
        Some((handle, supervisor)) => RuntimeHandle::spawn_persistent_with_supervisor(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
            handle,
            supervisor,
            memory,
        )?,
        None => RuntimeHandle::spawn_persistent(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
            memory,
        )?,
    };
    let runtime = runtime.with_browser(browser);
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

    if let Some(input) = one_shot {
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
        )
        .await
        .map(|result| ChatRun::Complete(Some(result)))
        .map_err(anyhow::Error::new);
    }

    let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
    let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
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
    Ok(ChatRun::Exited {
        exit,
        presentation,
        restart_tui,
        tui_required,
        tui_continuation,
        desktop_restart,
    })
}

async fn continue_after_chat_exit(
    paths: &XanaPaths,
    exit: ChatExit,
    mut presentation: presentation::ResolvedPresentation,
    restart_tui: bool,
    tui_required: bool,
    tui_continuation: Option<tui::TuiContinuation>,
    desktop_restart: Option<DesktopRestart>,
) -> Result<Option<ChatRestart>> {
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
        _ => None,
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
        force_new_conversation = run_setup_command(&args, paths)
            .await?
            .requires_new_conversation();
    }
    if let ChatExit::Settings(request) = &exit {
        let settings_profile = super::resolved_presentation(paths, true, true);
        let outcome = tui::run_settings(
            paths,
            (!request.is_empty()).then_some(request.as_str()),
            None,
            settings_profile,
        )?;
        force_new_conversation |= outcome.requires_new_conversation;
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
