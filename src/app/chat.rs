//! Composition of one interactive or one-shot chat launch.
//!
//! The interface accepts resolved process-edge intent. Configuration, provider,
//! session, runtime, and frontend ownership stay inside this module so command
//! routing does not need to know their construction order.

use super::{
    codex_launch, model_manager, run_doctor_command, run_reset_command, run_setup_command,
};
use crate::{
    agent::Agent,
    artifact::ArtifactStore,
    cli,
    config::{ProviderKind, XanaConfig},
    context::{ContextBudget, ContextPlanReport},
    credential::CredentialResolver,
    managed::codex::CodexAppServer,
    managed_execution::{
        ManagedChatConfig, ManagedOneShotRequest, run_codex_chat, run_codex_one_shot,
    },
    native_runtime::RuntimeHandle,
    oneshot::OneShotSuccess,
    orchestration::{
        ChildExecutionOwnerFactory, ChildSupervisor, OrchestrationBudget, ParentExecution,
    },
    paths::XanaPaths,
    permission::PermissionPolicy,
    plain_terminal::{self, ChatHeader},
    presentation::{self, BannerMode},
    prompt::{ProductDocumentationHint, PromptAssembler, PromptEnvironment, PromptSurface},
    provider::{anthropic::AnthropicClient, openai_compat::OpenAiCompatClient},
    session::DurableSession,
    shell::Shell,
    tool::ToolRegistry,
    tui,
    vision::MediaResolver,
    workspace_host::{ConversationRef, WorkspaceHost, WorkspaceHostError},
};
use anyhow::{Context, Result};
use clap::Parser as _;
use std::{io::Write, sync::Arc};

const PROMPT_TOTAL_TOKENS: usize = 32_768;
const PROMPT_CONVERSATION_RESERVE_TOKENS: usize = 8_192;

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
}

impl ChatSurface {
    fn profile(&self) -> presentation::ResolvedPresentation {
        match self {
            Self::Plain(mode) => mode.profile(),
            Self::Tui { prepared, .. } => prepared.profile(),
            Self::Hosted { presentation, .. } => *presentation,
        }
    }
}

pub(super) async fn run(
    paths: &XanaPaths,
    mut surface: ChatSurface,
    mut resume: Option<crate::identity::SessionId>,
    mut continue_chat: bool,
    mut force_new: bool,
    mut one_shot: Option<String>,
) -> Result<Option<OneShotSuccess>> {
    loop {
        match run_once(paths, surface, resume, continue_chat, force_new, one_shot).await? {
            ChatRun::Complete(result) => return Ok(result),
            ChatRun::Exited {
                exit,
                presentation,
                restart_tui,
                tui_required,
            } => {
                let Some(restart) =
                    continue_after_chat_exit(paths, exit, presentation, restart_tui, tui_required)
                        .await?
                else {
                    return Ok(None);
                };
                surface = restart.surface;
                resume = restart.resume;
                continue_chat = false;
                force_new = restart.force_new;
                one_shot = None;
            }
        }
    }
}

enum ChatRun {
    Complete(Option<OneShotSuccess>),
    Exited {
        exit: plain_terminal::ChatExit,
        presentation: presentation::ResolvedPresentation,
        restart_tui: bool,
        tui_required: bool,
    },
}

struct ChatRestart {
    surface: ChatSurface,
    resume: Option<crate::identity::SessionId>,
    force_new: bool,
}

async fn run_once(
    paths: &XanaPaths,
    surface: ChatSurface,
    resume: Option<crate::identity::SessionId>,
    continue_chat: bool,
    force_new: bool,
    one_shot: Option<String>,
) -> Result<ChatRun> {
    let presentation = surface.profile();
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
        (ChatSurface::Tui { .. }, _) => {}
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

    let manager = model_manager(paths)?;
    let child_registry = XanaConfig::load_registry_from(paths.config_file())
        .context("could not load child route registry")?;
    let frozen_profile = resume
        .map(|session_id| {
            crate::profile::ProfileStore::open(paths)
                .snapshot(&session_id.to_string())
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

    let XanaConfig {
        mut permission_mode,
        permission_rules,
        shell,
        mut max_tool_rounds,
        ..
    } = config;
    if let Some(profile) = &frozen_profile {
        permission_mode = profile.permission_mode.value;
        max_tool_rounds = profile.max_tool_rounds.value;
    }
    let provider_name = selected_connection_name.to_owned();
    let provider_kind = selected_connection.kind;
    let model = selected_model;
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let workspace_root = std::env::current_dir()
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
    let artifact_store = ArtifactStore::new(paths.data_dir().join("artifacts"));

    let workspace_host = WorkspaceHost::open(paths.data_dir(), &workspace_root)?;
    debug_assert_eq!(workspace_host.workspace(), workspace_root);
    let host_snapshot = workspace_host.snapshot()?;
    let resume = if provider_kind != ProviderKind::Codex {
        if (resume.is_some() || continue_chat) && host_snapshot.active.is_some() {
            return Err(anyhow::Error::new(WorkspaceHostError::Busy(
                host_snapshot.active.clone(),
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
    let conversation = if provider_kind == ProviderKind::Codex {
        let current = (resume.is_none() && !force_new && (one_shot.is_none() || continue_chat))
            .then(|| {
                host_snapshot
                    .conversations
                    .into_iter()
                    .find_map(|projection| match projection.conversation {
                        ConversationRef::Managed {
                            connection,
                            thread_id,
                        } if connection == provider_name && projection.selected => {
                            Some(ConversationRef::Managed {
                                connection,
                                thread_id,
                            })
                        }
                        _ => None,
                    })
            })
            .flatten();
        current.unwrap_or_else(|| ConversationRef::NewManaged {
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
        let server = CodexAppServer::spawn(&codex_launch(&selected_connection)).await?;
        let developer_instructions = if managed_skill_instructions.is_empty() {
            crate::prompt::xana_identity().to_owned()
        } else {
            format!(
                "{}\n\n<activated_agent_skills>\n{}\n</activated_agent_skills>",
                crate::prompt::xana_identity(),
                managed_skill_instructions
            )
        };
        let managed_config = ManagedChatConfig {
            connection: provider_name,
            model,
            workspace: workspace_root,
            data_root: paths.data_dir().to_owned(),
            artifact_store,
            owner: crate::identity::PrincipalId::new(),
            developer_instructions,
            identity_version: crate::prompt::XANA_IDENTITY_VERSION,
            presentation,
        };
        return match one_shot {
            Some(input) => {
                let mut activity = anstream::stderr().lock();
                run_codex_one_shot(
                    server,
                    manager,
                    managed_config,
                    ManagedOneShotRequest {
                        input,
                        continue_thread: continue_chat,
                        conversation,
                    },
                    &mut activity,
                    &workspace_host,
                )
                .await
                .map(|result| ChatRun::Complete(Some(result)))
                .map_err(anyhow::Error::new)
            }
            None => {
                let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
                let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
                let exit = match surface {
                    ChatSurface::Plain(_) => {
                        run_codex_chat(
                            server,
                            manager,
                            managed_config,
                            workspace_host,
                            conversation,
                        )
                        .await?
                    }
                    ChatSurface::Tui { prepared, .. } => {
                        tui::run_managed(
                            prepared,
                            server,
                            manager,
                            managed_config,
                            workspace_host,
                            conversation,
                        )
                        .await?
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
                        plain_terminal::ChatExit::Quit
                    }
                };
                Ok(ChatRun::Exited {
                    exit,
                    presentation,
                    restart_tui,
                    tui_required,
                })
            }
        };
    }

    let base_url = selected_connection
        .base_url
        .clone()
        .context("selected native connection has no endpoint")?;

    let media = MediaResolver::new(artifact_store.clone(), crate::artifact::MAX_ARTIFACT_BYTES);
    let credentials = CredentialResolver::default();
    let (provider, endpoint): (Box<dyn crate::provider::ConversationalProvider>, String) =
        match provider_kind {
            ProviderKind::OpenAiCompat | ProviderKind::Ollama => {
                let client = match selected_connection.credential.as_ref() {
                    Some(reference) => {
                        let secret = credentials.resolve(reference)?;
                        OpenAiCompatClient::with_bearer_and_attribution(
                            base_url,
                            model.clone(),
                            secret,
                            None,
                            None,
                        )
                    }
                    None => OpenAiCompatClient::new(base_url, model.clone()),
                }
                .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::OpenAi | ProviderKind::OpenRouter => {
                let reference = selected_connection
                    .credential
                    .as_ref()
                    .context("selected API connection has no credential reference")?;
                let secret = credentials.resolve(reference)?;
                let client = OpenAiCompatClient::with_bearer_and_attribution(
                    base_url,
                    model.clone(),
                    secret,
                    None,
                    (provider_kind == ProviderKind::OpenRouter).then(|| "Xana".to_owned()),
                )
                .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::Anthropic => {
                let reference = selected_connection
                    .credential
                    .as_ref()
                    .context("selected Anthropic connection has no credential reference")?;
                let secret = credentials.resolve(reference)?;
                let client = AnthropicClient::new(base_url, secret, model.clone())
                    .with_media_resolver(media);
                let endpoint = client.endpoint().to_owned();
                (Box::new(client), endpoint)
            }
            ProviderKind::Codex => unreachable!("managed Codex was composed above"),
        };
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
    let workspace_root = session.workspace_root().to_owned();
    let artifact_owner = session.artifact_owner();
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
        );
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
            total_tokens: PROMPT_TOTAL_TOKENS,
            conversation_reserve_tokens: PROMPT_CONVERSATION_RESERVE_TOKENS,
        },
    )
    .with_context_sources(skill_sources);
    let prompt = prompt_assembler
        .assemble(&[])
        .context("could not assemble Xana base prompt")?;
    let context_report = ContextPlanReport::render(&prompt.context_plan)
        .as_str()
        .to_owned();
    let agent = Agent::new(
        provider,
        tools,
        workspace_root.clone(),
        prompt,
        max_tool_rounds,
    )
    .with_runtime_telemetry(crate::diagnostics::runtime_telemetry());
    let session_id = session.session_id();
    let session_path = session.path().to_owned();
    let runtime = match child_supervisor {
        Some((handle, supervisor)) => RuntimeHandle::spawn_persistent_with_supervisor(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
            handle,
            supervisor,
        )?,
        None => RuntimeHandle::spawn_persistent(
            agent,
            permission_policy,
            true,
            session,
            prompt_assembler,
        )?,
    };
    let conversation = match conversation {
        ConversationRef::NewNative => ConversationRef::Native { session_id },
        conversation => conversation,
    };
    let header = ChatHeader {
        provider_name,
        model,
        endpoint,
        context_report,
        session_id,
        session_path,
        resumed,
        repair_truncate_to,
        unfinished,
        children: restored_children,
        workspace_root: workspace_root.clone(),
        artifact_store,
        owner: artifact_owner,
        models: manager,
        presentation,
        vision,
    };

    if let Some(input) = one_shot {
        let mut activity = anstream::stderr().lock();
        return plain_terminal::run_one_shot(
            runtime,
            &header,
            input,
            &mut activity,
            &workspace_host,
            conversation,
        )
        .await
        .map(|result| ChatRun::Complete(Some(result)))
        .map_err(anyhow::Error::new);
    }

    let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
    let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
    let exit = match surface {
        ChatSurface::Plain(_) => {
            plain_terminal::run_chat(runtime, header, workspace_host, conversation).await?
        }
        ChatSurface::Tui { prepared, .. } => {
            tui::run_native(prepared, runtime, &header, workspace_host, conversation).await?
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
            plain_terminal::ChatExit::Quit
        }
    };
    Ok(ChatRun::Exited {
        exit,
        presentation,
        restart_tui,
        tui_required,
    })
}

async fn continue_after_chat_exit(
    paths: &XanaPaths,
    exit: plain_terminal::ChatExit,
    presentation: presentation::ResolvedPresentation,
    restart_tui: bool,
    tui_required: bool,
) -> Result<Option<ChatRestart>> {
    if exit == plain_terminal::ChatExit::Quit {
        return Ok(None);
    }
    let mut force_new_conversation = exit == plain_terminal::ChatExit::NewConversation;
    if let plain_terminal::ChatExit::ControlCommand { family, arguments } = &exit
        && let Err(error) = run_chat_control_command(paths, family, arguments).await
    {
        eprintln!("xana: {error:#}");
    }
    if let plain_terminal::ChatExit::Setup(request) = &exit {
        let mut args = crate::setup::args_for_request(request)?;
        args.plain = !restart_tui;
        force_new_conversation = run_setup_command(&args, paths)
            .await?
            .requires_new_conversation();
    }
    let doctor_resume = if let plain_terminal::ChatExit::Doctor(session_id) = &exit {
        run_doctor_command(&cli::DoctorArgs::default(), paths).await?;
        *session_id
    } else {
        None
    };
    if exit == plain_terminal::ChatExit::Reset {
        run_reset_command(&cli::ResetArgs::default(), paths)?;
        if XanaConfig::load_from(paths.config_file()).is_err() {
            return Ok(None);
        }
    }
    let restart_surface = if restart_tui {
        let preferences =
            presentation::PresentationPreferences::load(&paths.presentation_file()).preferences;
        match tui::prepare(presentation, preferences, paths.presentation_file()) {
            Ok(prepared) => ChatSurface::Tui {
                prepared,
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
        force_new: force_new_conversation,
    }))
}

async fn run_chat_control_command(paths: &XanaPaths, family: &str, arguments: &str) -> Result<()> {
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
    let stdout = std::io::stdout();
    match command {
        Some(cli::Command::Project(args)) => {
            super::projects::run_command(args.command, paths, &mut stdout.lock())
        }
        Some(cli::Command::Profile(args)) => {
            super::profiles::run_command(args.command, paths, &mut stdout.lock())
        }
        Some(cli::Command::Skill(args)) => {
            super::skills::run_command(args.command, paths, &mut stdout.lock())
        }
        Some(cli::Command::Plugin(args)) => {
            super::plugins::run_command(args.command, paths, &mut stdout.lock())
        }
        Some(cli::Command::Mcp(args)) => {
            super::mcp_commands::run(args.command, paths, &mut stdout.lock()).await
        }
        Some(cli::Command::ExternalAgent(args)) => {
            super::external_agents::run(args.command, paths, &mut stdout.lock()).await
        }
        Some(cli::Command::Image(args)) => {
            let stdin = std::io::stdin();
            super::image_commands::run(args.command, paths, &mut stdin.lock(), &mut stdout.lock())
                .await
        }
        _ => {
            anyhow::bail!(
                "only project, profile, skill, plugin, MCP, external-agent, and image commands are available from this control path"
            )
        }
    }
}
