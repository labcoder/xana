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
    managed_terminal::{
        ManagedChatConfig, ManagedOneShotRequest, run_codex_chat, run_codex_one_shot,
    },
    oneshot::OneShotSuccess,
    orchestration::{
        ChildExecutionOwnerFactory, ChildSupervisor, OrchestrationBudget, ParentExecution,
    },
    paths::XanaPaths,
    permission::PermissionPolicy,
    presentation::{self, BannerMode},
    prompt::{ProductDocumentationHint, PromptAssembler, PromptEnvironment, PromptSurface},
    provider::{anthropic::AnthropicClient, openai_compat::OpenAiCompatClient},
    runtime::RuntimeHandle,
    session::DurableSession,
    shell::Shell,
    terminal::{self, ChatHeader},
    tool::ToolRegistry,
    tui,
    vision::MediaResolver,
    workspace_host::{ConversationRef, WorkspaceHost, WorkspaceHostError},
};
use anyhow::{Context, Result};
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
    surface: ChatSurface,
    resume: Option<crate::identity::SessionId>,
    continue_chat: bool,
    force_new: bool,
    one_shot: Option<String>,
) -> Result<Option<OneShotSuccess>> {
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
    let selected = manager.selected()?;
    let selected_connection = manager.connection(&selected.connection)?.clone();

    let XanaConfig {
        permission_mode,
        permission_rules,
        shell,
        max_tool_rounds,
        ..
    } = config;
    let provider_name = selected.connection;
    let provider_kind = selected_connection.kind;
    let model = selected.model;
    let shell = Shell::resolve(shell).context("could not resolve configured shell")?;
    let configured_shell = shell.prompt_description();
    let workspace_root = std::env::current_dir()
        .context("could not resolve Xana workspace root")?
        .canonicalize()
        .context("could not canonicalize Xana workspace root")?;
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
        let current = (!force_new && (one_shot.is_none() || continue_chat))
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
        if resume.is_some() {
            anyhow::bail!(
                "Xana durable --resume applies to native conversations; Codex owns managed thread resume"
            )
        }
        let server = CodexAppServer::spawn(&codex_launch(&selected_connection)).await?;
        let managed_config = ManagedChatConfig {
            connection: provider_name,
            model,
            workspace: workspace_root,
            data_root: paths.data_dir().to_owned(),
            artifact_store,
            owner: crate::identity::PrincipalId::new(),
            developer_instructions: crate::prompt::xana_identity(),
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
                .map(Some)
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
                        terminal::ChatExit::Quit
                    }
                };
                continue_after_chat_exit(paths, exit, presentation, restart_tui, tui_required).await
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
    );
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
    );
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
    };

    if let Some(input) = one_shot {
        let mut activity = anstream::stderr().lock();
        return terminal::run_one_shot(
            runtime,
            &header,
            input,
            &mut activity,
            &workspace_host,
            conversation,
        )
        .await
        .map(Some)
        .map_err(anyhow::Error::new);
    }

    let restart_tui = matches!(&surface, ChatSurface::Tui { .. });
    let tui_required = matches!(&surface, ChatSurface::Tui { required: true, .. });
    let exit = match surface {
        ChatSurface::Plain(_) => {
            terminal::run_chat(runtime, header, workspace_host, conversation).await?
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
            terminal::ChatExit::Quit
        }
    };
    continue_after_chat_exit(paths, exit, presentation, restart_tui, tui_required).await
}

async fn continue_after_chat_exit(
    paths: &XanaPaths,
    exit: terminal::ChatExit,
    presentation: presentation::ResolvedPresentation,
    restart_tui: bool,
    tui_required: bool,
) -> Result<Option<OneShotSuccess>> {
    if exit == terminal::ChatExit::Quit {
        return Ok(None);
    }
    let mut force_new_conversation = exit == terminal::ChatExit::NewConversation;
    if let terminal::ChatExit::Setup(request) = &exit {
        let args = crate::setup::args_for_request(request)?;
        force_new_conversation = run_setup_command(&args, paths)
            .await?
            .requires_new_conversation();
    }
    let doctor_resume = if let terminal::ChatExit::Doctor(session_id) = &exit {
        run_doctor_command(&cli::DoctorArgs::default(), paths).await?;
        *session_id
    } else {
        None
    };
    if exit == terminal::ChatExit::Reset {
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
    Box::pin(run(
        paths,
        restart_surface,
        doctor_resume,
        false,
        force_new_conversation,
        None,
    ))
    .await
}
