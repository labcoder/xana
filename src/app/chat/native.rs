//! Native execution composition shared by startup and safe between-turn refresh.
use super::*;
use crate::native_runtime::configuration::PreparedExecution;
use crate::profile::{ResolvedProfile, execution::ExecutionConfiguration};

pub(super) struct Composed {
    pub(super) execution: PreparedExecution,
    pub(super) endpoint: String,
    pub(super) context_report: String,
    pub(super) vision: crate::app::vision::VisionTurnService,
}

pub(super) async fn compose(
    paths: &XanaPaths,
    session: &DurableSession,
    configuration: ExecutionConfiguration,
    browser: Option<crate::browser::BrowserOwner>,
) -> Result<Composed> {
    let config = XanaConfig::load_from(paths.config_file())?;
    let child_registry = XanaConfig::load_registry_from(paths.config_file())?;
    let manager = model_manager(paths)?;
    let profile: &ResolvedProfile = &configuration.profile;
    anyhow::ensure!(
        profile.is_ready(),
        "Profile is not ready: {}",
        profile.readiness.join("; ")
    );
    let provider_name = profile.connection.value.clone();
    let model = profile.model.value.clone();
    let profile_name = profile.name.clone();
    let permission_mode = profile.permission_mode.value;
    let max_tool_rounds = profile.max_tool_rounds.value;
    let selected_connection = manager.connection(&provider_name)?.clone();
    anyhow::ensure!(
        selected_connection.kind != ProviderKind::Codex,
        "This native Conversation cannot change to a managed execution owner; select a native model to keep working here. No history was changed."
    );
    let selected = crate::model_catalog::ModelSelection {
        connection: provider_name.clone(),
        model: model.clone(),
        reasoning_effort: profile.reasoning_effort.value.clone(),
        reasoning_summary: None,
    };
    let workspace_root = session.workspace_root().to_owned();
    let shell = Shell::resolve(config.shell)?;
    let configured_shell = shell.prompt_description();
    let prompt_budget_policy = config.context;
    let permission_rules = config.permission_rules;
    let permission_policy = PermissionPolicy::new(
        permission_mode.into(),
        permission_rules.clone(),
        &workspace_root,
    )?;
    let artifact_store = ArtifactStore::open(paths.data_dir())?;
    let plugin_sources = crate::plugin::PluginManager::open(paths)
        .skill_sources_for_revisions(&profile.plugin_revisions)?;
    let catalog = crate::app::skills::catalog(&workspace_root, plugin_sources)?;
    let skill_sources = catalog
        .activate_all(profile.skills.value.iter().map(String::as_str))?
        .iter()
        .map(crate::skill::ActivatedSkill::context_source)
        .collect::<Vec<_>>();
    let restored_children = session.child_inspections();
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
    let capabilities = crate::capability::resolve_builtin_capability_snapshot(
        profile.capabilities.value.as_deref(),
    )?;
    let mut tools = ToolRegistry::builtins_for_snapshot(shell.clone(), &capabilities)
        .context("could not build tool registry")?;
    let profile_mcp_servers = profile.mcp_servers.value.clone();
    let profile_mcp_allowlists = profile.mcp_allowlists.value.clone();
    let profile_egress = profile.egress.value.clone();
    let profile_external_agents = profile.external_agents.value.clone();
    if capabilities
        .tool_ids()
        .iter()
        .any(|id| id.as_str() == "web_fetch")
    {
        tools
            .configure_web(paths, &child_registry.web, &profile_egress)
            .context("could not configure web tools")?;
    }
    if !session.has_unfinished_work()
        || configuration.inputs_digest == super::configuration::inputs_digest(paths)?
    {
        crate::app::mcp_commands::activate_profile_tools(
            &child_registry,
            paths,
            &workspace_root,
            &profile_mcp_servers,
            &profile_mcp_allowlists,
            &profile_egress,
            &mut tools,
        )
        .await?;
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
    let profile_service_routes = profile.service_routes.value.clone();
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
    let vision = crate::app::vision::VisionTurnService::new(
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
        // Child admission intersects the actual root revision. This derived
        // execution registry must not replace the authored registry retained
        // by services for stale-plan / configuration-equality checks.
        let mut child_registry = child_registry.clone();
        let root = child_registry
            .profiles
            .get_mut(&child_registry.default_profile)
            .context("default Profile is unavailable")?;
        root.connection = provider_name.clone();
        root.model = model.clone();
        root.permission_mode = Some(permission_mode);
        root.capabilities = profile.capabilities.value.clone();
        root.max_tool_rounds = max_tool_rounds;
        root.orchestration = profile.orchestration.value.clone();
        let root_profile = child_registry
            .profiles
            .get(&child_registry.default_profile)
            .context("validated configuration lost its default profile")?;
        let budget = OrchestrationBudget::new(
            root_profile.orchestration.clone(),
            root_profile.max_tool_rounds,
        );
        let factory = ChildExecutionOwnerFactory::new(
            child_registry.clone(),
            model_manager(paths)?,
            shell,
            workspace_root.clone(),
            artifact_store.clone(),
            permission_rules,
        )
        .with_usage_budget(crate::app::usage_commands::compose_budget(
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
    let memory = crate::app::memory_commands::compose(
        paths,
        &artifact_store,
        &session.session_id().to_string(),
        profile.profile_id,
    )?;
    crate::memory::tools::register(&mut tools, memory.clone())
        .context("could not register personal memory tools")?;
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
    let agent = Agent::new(
        provider,
        tools,
        workspace_root.clone(),
        prompt,
        max_tool_rounds,
    )
    .with_runtime_telemetry(crate::diagnostics::runtime_telemetry())
    .with_semantic_compaction(crate::app::sessions::semantic_policy(
        paths,
        &selected_connection,
        &model,
    )?)
    .with_usage_budget(crate::app::usage_commands::compose_budget(
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

    Ok(Composed {
        execution: PreparedExecution {
            agent,
            policy: permission_policy,
            prompt_assembler,
            child_supervisor,
            memory,
            configuration,
        },
        endpoint,
        context_report,
        vision,
    })
}
