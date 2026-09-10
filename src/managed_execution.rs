//! Xana execution adapter for a foreign managed-agent runtime.
//!
//! Codex retains its inner loop. This module owns the bounded Xana-facing
//! conversation, one-shot, activity, and TUI-driver projections around it.

mod activity;
mod failure;
mod memory_context;
mod memory_tools;
#[cfg(test)]
pub(crate) use memory_context::prepare as prepare_memory_for_test;
mod tui_driver;

pub(crate) use tui_driver::{ManagedTuiDriver, ManagedTuiEvent};

use crate::{
    app::ChatExit,
    artifact::ArtifactStore,
    frontend::ManagedClientEvent,
    identity::{ConversationId, PrincipalId},
    managed::{
        codex::{AccountStatus, CodexAppServer, CodexError, ManagedTurnInput, ManagedTurnOptions},
        thread_store::ManagedThreadStore,
    },
    model_catalog::{ModelDescriptor, ModelManager, ModelSelection, ReasoningSummary},
    oneshot::{ExitCategory, OneShotFailure, OneShotReporter, OneShotSuccess},
    presentation::{ResolvedPresentation, SemanticToken},
    vision::{ImageIngestor, ImageLimits, PendingImages},
    workspace_host::{ConversationRef, WorkspaceHost},
};
use activity::{ActivityLevel, RetainedActivity, TerminalManagedHandler, render_retained_activity};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use rustyline::{DefaultEditor, error::ReadlineError};
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct ManagedChatConfig {
    pub(crate) profile_guard: Option<std::sync::Arc<ManagedProfileGuard>>,
    pub(crate) memory: Option<crate::memory::MemoryOwner>,
    pub(crate) permission_default: crate::permission::PolicyDecision,
    pub(crate) permission_rules: Vec<crate::permission::PermissionRule>,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) profile_name: String,
    pub(crate) selection: ModelSelection,
    pub(crate) workspace: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) owner: PrincipalId,
    pub(crate) developer_instructions: String,
    pub(crate) identity_version: &'static str,
    pub(crate) presentation: ResolvedPresentation,
    pub(crate) resource_policy: crate::resource::ResourcePolicyV1,
}

pub(crate) struct ManagedOneShotRequest {
    pub(crate) input: String,
    pub(crate) continue_thread: bool,
    pub(crate) conversation: ConversationRef,
}

impl ManagedChatConfig {
    async fn prepare_turn(
        &self,
        conversation: ConversationId,
        input: &crate::tool::OwnerTurnInput,
    ) -> Result<String, String> {
        if let Some(guard) = self.profile_guard.clone() {
            guard.validate().await?;
        }
        memory_context::prepare_turn(self.memory.as_ref(), conversation, input).await
    }
}

pub(crate) struct ManagedProfileGuard {
    pub(crate) paths: crate::paths::XanaPaths,
    pub(crate) profile: crate::profile::ResolvedProfile,
}

impl ManagedProfileGuard {
    pub(crate) async fn validate(self: std::sync::Arc<Self>) -> Result<(), String> {
        tokio::task::spawn_blocking(move || {
            crate::profile::execution::resolve_current(&self.paths, Some(&self.profile)).map(|_| ())
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Profile is unavailable; no Codex turn was sent: {error:#}"))
    }
}

enum ManagedThreadState {
    New {
        conversation_id: ConversationId,
    },
    NeedsResume {
        conversation_id: ConversationId,
        thread_id: String,
        identity_is_current: bool,
    },
    Loaded {
        conversation_id: ConversationId,
        thread_id: String,
    },
}

impl ManagedThreadState {
    fn conversation_id(&self) -> ConversationId {
        match self {
            Self::New { conversation_id }
            | Self::NeedsResume {
                conversation_id, ..
            }
            | Self::Loaded {
                conversation_id, ..
            } => *conversation_id,
        }
    }
}

fn initial_managed_thread(
    conversation: &ConversationRef,
    store: &ManagedThreadStore,
    connection: &str,
    identity_version: &str,
) -> Result<(Option<String>, ManagedThreadState), CodexError> {
    match conversation {
        ConversationRef::NewManaged {
            conversation_id,
            connection: requested,
        } if requested == connection => Ok((
            None,
            ManagedThreadState::New {
                conversation_id: *conversation_id,
            },
        )),
        ConversationRef::Managed {
            conversation_id,
            connection: requested,
            thread_id,
        } if requested == connection => Ok((
            Some(thread_id.clone()),
            ManagedThreadState::NeedsResume {
                conversation_id: *conversation_id,
                thread_id: thread_id.clone(),
                identity_is_current: store.conversation_id() == Some(*conversation_id)
                    && store.thread_id() == Some(thread_id.as_str())
                    && store.identity_version() == Some(identity_version),
            },
        )),
        _ => Err(CodexError::Protocol(
            "managed conversation does not match the selected Codex connection".to_owned(),
        )),
    }
}

pub(crate) async fn run_codex_chat(
    mut server: CodexAppServer,
    models: ModelManager,
    mut config: ManagedChatConfig,
    workspace_host: WorkspaceHost,
    mut conversation: ConversationRef,
) -> Result<ChatExit> {
    if matches!(server.account_status().await?, AccountStatus::LoggedOut) {
        anyhow::bail!(
            "Codex is logged out; run `xana connection login {}` first",
            config.connection
        );
    }
    let mut available = server.models().await?;
    models.write_managed_cache(&config.connection, &available)?;
    if !available.iter().any(|model| model.id == config.model) {
        anyhow::bail!(unavailable_model_message(
            &config.connection,
            &config.model,
            &available
        ));
    }
    let mut selection = config.selection.clone();

    let mut thread_store =
        ManagedThreadStore::open(&config.data_root, &config.connection, &config.workspace)?;
    let (_, mut thread) = initial_managed_thread(
        &conversation,
        &thread_store,
        &config.connection,
        config.identity_version,
    )?;
    let mut activity = ActivityLevel::Normal;
    let mut last_activity = RetainedActivity::default();

    println!(
        "{} {}",
        config
            .presentation
            .paint(SemanticToken::Accent, "Xana managed connection:"),
        config.connection
    );
    println!("execution: managed Codex app-server ({})", server.version);
    println!("model: {}", config.model);
    println!(
        "reasoning: {} (summary: {})",
        selection
            .reasoning_effort
            .as_deref()
            .unwrap_or("model default"),
        selection
            .reasoning_summary
            .map_or_else(|| "provider default".into(), |value| value.to_string())
    );
    println!("workspace: {}", config.workspace.display());
    println!(
        "{}",
        if config.memory.is_some() {
            crate::memory::learning::DISCLOSURE
        } else {
            crate::memory::UNAVAILABLE_NOTICE
        }
    );
    if let ManagedThreadState::NeedsResume {
        thread_id,
        identity_is_current,
        ..
    } = &thread
    {
        println!("managed thread: {thread_id} (will resume on the first turn)");
        if !identity_is_current {
            println!(
                "xana> this thread predates Xana's current managed identity; Codex cannot replace its identity while preserving the thread"
            );
            println!(
                "xana> enter /clear before your first prompt to start as Xana; the old Codex-owned thread will not be deleted"
            );
        }
    }
    println!(
        "/model, /reasoning, /reasoning-summary, /activity, /details, and /usage control this managed conversation; /attach adds an image; /settings opens reviewed preferences; /doctor pauses for read-only diagnostics; /setup reconfigures Xana; /clear starts a new Codex thread; /quit exits"
    );

    let mut editor = DefaultEditor::new().context("could not initialize terminal editor")?;
    let mut pending = PendingImages::default();
    let ingestor = ImageIngestor::new(config.artifact_store.clone(), ImageLimits::default());

    let mut exit = ChatExit::Quit;
    let mut memory_maintenance = None;
    loop {
        let line = match editor.readline("you> ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                println!("xana> input cancelled");
                continue;
            }
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(error).context("terminal input failed"),
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" {
            break;
        }
        if let Some(command) = local_control(input) {
            exit = command;
            break;
        }
        if input == "/doctor" {
            exit = ChatExit::Doctor(None);
            break;
        }
        let settings_request = input
            .strip_prefix("/settings ")
            .map(str::trim)
            .or_else(|| (input == "/settings").then_some(""));
        if let Some(section) = settings_request {
            if !section.is_empty() && crate::settings::SettingsSection::parse(section).is_none() {
                println!(
                    "xana> {}",
                    crate::settings::SettingsError::UnknownSection(section.to_owned())
                );
                continue;
            }
            exit = ChatExit::Settings(section.to_owned());
            break;
        }
        let setup_request = input
            .strip_prefix("/setup ")
            .map(str::trim)
            .or_else(|| (input == "/setup").then_some(""));
        if let Some(section) = setup_request {
            match crate::setup::args_for_request(section) {
                Ok(_) => {
                    exit = ChatExit::Setup(section.to_owned());
                    break;
                }
                Err(error) => {
                    println!("xana> {error}");
                    continue;
                }
            }
        }
        if input == "/clear" {
            thread_store.set_thread(None, None, None)?;
            let conversation_id = ConversationId::new();
            thread = ManagedThreadState::New { conversation_id };
            last_activity = RetainedActivity::default();
            conversation = ConversationRef::NewManaged {
                conversation_id,
                connection: config.connection.clone(),
            };
            let cleared = pending.clear();
            println!("xana> new managed thread will start; cleared {cleared} pending image(s)");
            continue;
        }
        if input == "/details" {
            render_retained_activity(&last_activity)?;
            continue;
        }
        if input == "/usage" {
            if let Some((input, output, total)) = last_activity.usage() {
                println!(
                    "xana> current managed thread usage: input {input}, output {output}, total {total}"
                );
            } else {
                println!("xana> current managed thread usage is unknown until Codex reports it");
            }
            println!(
                "xana> provider quota, reset time, and wallet balance are not exposed by this managed runtime"
            );
            continue;
        }
        if let Some(value) = input.strip_prefix("/activity").map(str::trim) {
            if value.is_empty() {
                println!("xana> activity display: {activity}");
            } else {
                match value.parse::<ActivityLevel>() {
                    Ok(level) => {
                        activity = level;
                        println!("xana> activity display: {activity} (session only)");
                    }
                    Err(error) => println!("xana> usage: /activity quiet|normal|verbose ({error})"),
                }
            }
            continue;
        }
        if let Some(value) = input.strip_prefix("/reasoning-summary").map(str::trim) {
            if value.is_empty() {
                println!(
                    "xana> reasoning summary: {}",
                    selection
                        .reasoning_summary
                        .map_or_else(|| "provider default".into(), |value| value.to_string())
                );
            } else {
                match value.parse::<ReasoningSummary>() {
                    Ok(summary) => match models.update_reasoning_summary(summary) {
                        Ok(updated) => {
                            selection = updated;
                            println!(
                                "xana> reasoning summary: {summary}; the Codex thread and context are unchanged"
                            );
                        }
                        Err(error) => println!("xana> could not set reasoning summary: {error}"),
                    },
                    Err(error) => println!("xana> {error}"),
                }
            }
            continue;
        }
        if let Some(value) = input.strip_prefix("/reasoning").map(str::trim) {
            if value.is_empty() {
                print_reasoning_status(&selection, &available, &config.model);
            } else {
                let requested = (value != "auto").then(|| value.to_owned());
                match models.update_reasoning_effort(requested) {
                    Ok(updated) => {
                        selection = updated;
                        println!(
                            "xana> reasoning effort: {}; the Codex thread and context are unchanged",
                            selection
                                .reasoning_effort
                                .as_deref()
                                .unwrap_or("model default")
                        );
                    }
                    Err(error) => println!("xana> could not set reasoning effort: {error}"),
                }
            }
            continue;
        }
        if let Some(path) = input.strip_prefix("/attach").map(str::trim) {
            if path.is_empty() {
                println!("xana> usage: /attach WORKSPACE_RELATIVE_IMAGE_PATH");
                continue;
            }
            match ingestor.ingest_path(&config.workspace, path, config.owner) {
                Ok(attachment) => {
                    println!(
                        "xana> attached {} ({} bytes, {})",
                        attachment.source_path,
                        attachment.image.byte_len,
                        attachment.image.media_type
                    );
                    pending.push(attachment);
                }
                Err(error) => println!("xana> could not attach {path}: {error}"),
            }
            continue;
        }
        if let Some(requested) = input.strip_prefix("/model").map(str::trim) {
            if requested.is_empty() {
                available = server.models().await?;
                models.write_managed_cache(&config.connection, &available)?;
                print_models(&available, &config.model);
                continue;
            }
            let (connection, requested_model) = requested.split_once('/').map_or(
                (config.connection.as_str(), requested),
                |(connection, model)| (connection, model),
            );
            if connection != config.connection {
                println!(
                    "xana> switching between native and managed runtimes starts a new conversation; run `xana model use {requested}` and restart Xana"
                );
                continue;
            }
            available = server.models().await?;
            models.write_managed_cache(&config.connection, &available)?;
            if !available.iter().any(|model| model.id == requested_model) {
                println!("xana> Codex does not advertise model {requested_model:?}");
                continue;
            }
            selection = models.select(&config.connection, requested_model)?;
            config.model = requested_model.to_owned();
            println!(
                "xana> selected {} with {} reasoning for subsequent turns; the Codex thread and context are unchanged",
                config.model,
                selection
                    .reasoning_effort
                    .as_deref()
                    .unwrap_or("model-default")
            );
            continue;
        }

        if pending.len() > 8 {
            println!("xana> at most 8 images may be sent in one turn");
            continue;
        }
        if pending.len() > 0
            && !available
                .iter()
                .find(|model| model.id == config.model)
                .is_some_and(|model| model.input_modalities.contains("image"))
        {
            println!(
                "xana> {}/{} is not advertised as image-capable",
                config.connection, config.model
            );
            continue;
        }
        let attachments = pending.take_for_turn();
        let total_image_bytes = attachments
            .iter()
            .map(|attachment| attachment.image.byte_len)
            .sum::<u64>();
        if total_image_bytes > 20 * 1024 * 1024 {
            for attachment in attachments {
                pending.push(attachment);
            }
            println!("xana> image attachments exceed the 20 MiB per-turn budget");
            continue;
        }
        let image_urls = match attachments
            .iter()
            .map(|attachment| {
                crate::vision::MediaResolver::new(
                    config.artifact_store.clone(),
                    crate::artifact::MAX_ARTIFACT_BYTES,
                )
                .resolve_openai_data_url(&attachment.image)
                .map_err(anyhow::Error::from)
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(paths) => paths,
            Err(error) => {
                for attachment in attachments {
                    pending.push(attachment);
                }
                println!("xana> could not prepare image attachments: {error}");
                continue;
            }
        };

        let root_lease = match workspace_host
            .acquire_foreground_root(conversation.clone())
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                println!("xana> could not start turn: {error}");
                continue;
            }
        };
        let mut handler = TerminalManagedHandler::new(activity);
        let operation_id = crate::identity::OperationId::new();
        let owner_input = memory_tools::owner_input(operation_id, input, Default::default());
        let mut memory_handler = memory_tools::MemoryManagedHandler::new(
            &config,
            thread.conversation_id(),
            owner_input.clone(),
            &mut handler,
            activity::memory_review(),
        )?;
        let loaded_thread_id = match ensure_thread_loaded(
            &mut server,
            &mut thread,
            &mut thread_store,
            &config,
            &mut memory_handler,
        )
        .await
        {
            Ok(id) => id,
            Err(error) => {
                for attachment in attachments {
                    pending.push(attachment);
                }
                println!("xana> could not open managed thread: {error}");
                if matches!(thread, ManagedThreadState::NeedsResume { .. }) {
                    println!("xana> use /clear to explicitly start a new Codex thread");
                }
                continue;
            }
        };
        server.set_usage_identity(thread.conversation_id().to_string(), operation_id);
        let _foreground = memory_context::foreground(config.memory.as_ref())?;
        let managed_input = match config
            .prepare_turn(thread.conversation_id(), &owner_input)
            .await
        {
            Ok(text) => text,
            Err(error) => {
                for attachment in attachments {
                    pending.push(attachment);
                }
                println!("xana> {error}");
                continue;
            }
        };
        let result = server
            .run_turn(
                &loaded_thread_id,
                &config.model,
                &ManagedTurnOptions {
                    reasoning_effort: selection.reasoning_effort.clone(),
                    reasoning_summary: selection.reasoning_summary,
                },
                ManagedTurnInput {
                    text: managed_input,
                    image_urls,
                },
                &mut memory_handler,
            )
            .await;
        memory_handler.finish().await?;
        if let Err(error) = &result {
            crate::diagnostics::emit_terminal(failure::diagnostic(
                error,
                Some(operation_id),
                thread.conversation_id(),
                &config,
            ));
        }
        handler.finish_stream()?;
        last_activity = handler.into_retained();
        drop(_foreground);
        memory_context::maintain(config.memory.as_ref(), &mut memory_maintenance);
        match result {
            Ok(result) => {
                if !last_activity.assistant_streamed && !result.final_text.is_empty() {
                    println!("xana> {}", result.final_text);
                }
            }
            Err(error) => println!("xana> managed turn failed: {error}"),
        }
        conversation = ConversationRef::Managed {
            conversation_id: thread.conversation_id(),
            connection: config.connection.clone(),
            thread_id: loaded_thread_id,
        };
        drop(root_lease);
    }
    server.shutdown().await?;
    Ok(exit)
}

pub(crate) async fn run_codex_one_shot(
    mut server: CodexAppServer,
    models: ModelManager,
    config: ManagedChatConfig,
    request: ManagedOneShotRequest,
    reporter: &mut OneShotReporter<'_>,
    workspace_host: &WorkspaceHost,
) -> Result<OneShotSuccess, OneShotFailure> {
    let result = run_codex_one_shot_inner(
        &mut server,
        &models,
        &config,
        request,
        reporter,
        workspace_host,
    )
    .await;
    let shutdown = server.shutdown().await;
    match (result, shutdown) {
        (Ok(success), Ok(())) => Ok(success),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(OneShotFailure::new(
            ExitCategory::Runtime,
            error.to_string(),
        )),
    }
}

async fn run_codex_one_shot_inner(
    server: &mut CodexAppServer,
    models: &ModelManager,
    config: &ManagedChatConfig,
    request: ManagedOneShotRequest,
    reporter: &mut OneShotReporter<'_>,
    workspace_host: &WorkspaceHost,
) -> Result<OneShotSuccess, OneShotFailure> {
    let conversation_id = request
        .conversation
        .conversation_id()
        .expect("composed managed one-shot has a Conversation identity");
    let account = server
        .account_status()
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Connection, error.to_string()))?;
    if matches!(account, AccountStatus::LoggedOut) {
        return Err(OneShotFailure::new(
            ExitCategory::Connection,
            format!(
                "Codex is logged out; run `xana connection login {}` first",
                config.connection
            ),
        ));
    }
    let available = server
        .models()
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Connection, error.to_string()))?;
    models
        .write_managed_cache(&config.connection, &available)
        .map_err(|error| OneShotFailure::new(ExitCategory::Configuration, error.to_string()))?;
    if !available.iter().any(|model| model.id == config.model) {
        return Err(OneShotFailure::new(
            ExitCategory::Connection,
            unavailable_model_message(&config.connection, &config.model, &available),
        ));
    }
    let selection = config.selection.clone();
    let mut store =
        ManagedThreadStore::open(&config.data_root, &config.connection, &config.workspace)
            .map_err(|error| OneShotFailure::new(ExitCategory::Configuration, error.to_string()))?;
    if request.continue_thread {
        require_one_shot_target(&store, &request.conversation, &config.connection)
            .map_err(|error| OneShotFailure::new(ExitCategory::InvalidInput, error.to_string()))?;
    }
    let mut handler = OneShotManagedHandler::new(reporter);
    let operation_id = crate::identity::OperationId::new();
    let owner_input = memory_tools::owner_input(operation_id, &request.input, Default::default());
    let memory_approval_required = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let required = memory_approval_required.clone();
    let mut memory_handler = memory_tools::MemoryManagedHandler::new(
        config,
        conversation_id,
        owner_input.clone(),
        &mut handler,
        memory_tools::MemoryReview::new(move |_| {
            required.store(true, std::sync::atomic::Ordering::Release);
            Box::pin(async { crate::permission::ControllerDecision::Deny })
        }),
    )
    .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    let thread_id = if request.continue_thread {
        match store.thread_id() {
            Some(thread_id) => {
                require_memory_tools(&store, thread_id, config.memory.is_some()).map_err(
                    |error| OneShotFailure::new(ExitCategory::Connection, error.to_string()),
                )?;
                server
                    .resume_thread(
                        thread_id,
                        &config.model,
                        &config.workspace,
                        &config.developer_instructions,
                        &mut memory_handler,
                    )
                    .await
            }
            None => {
                return Err(OneShotFailure::new(
                    ExitCategory::InvalidInput,
                    "--continue found no managed Codex conversation for this workspace",
                ));
            }
        }
    } else {
        server
            .start_thread(
                &config.model,
                &config.workspace,
                &config.developer_instructions,
                &mut memory_handler,
            )
            .await
    }
    .map_err(|error| OneShotFailure::new(ExitCategory::Connection, error.to_string()))?;
    store
        .set_thread(
            request.conversation.conversation_id(),
            Some(thread_id.clone()),
            Some(config.identity_version),
        )
        .map_err(|error| OneShotFailure::new(ExitCategory::Configuration, error.to_string()))?;
    if !request.continue_thread {
        store
            .mark_memory_tools_current(&thread_id, config.memory.is_some())
            .map_err(|error| OneShotFailure::new(ExitCategory::Configuration, error.to_string()))?;
    }

    let _root_lease = workspace_host
        .acquire_foreground_root(request.conversation)
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    let _foreground = memory_context::foreground(config.memory.as_ref())
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    let managed_input = config
        .prepare_turn(conversation_id, &owner_input)
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error))?;
    server.set_usage_identity(conversation_id.to_string(), operation_id);
    let turn = server
        .run_turn(
            &thread_id,
            &config.model,
            &ManagedTurnOptions {
                reasoning_effort: selection.reasoning_effort,
                reasoning_summary: selection.reasoning_summary,
            },
            ManagedTurnInput {
                text: managed_input,
                image_urls: Vec::new(),
            },
            &mut memory_handler,
        )
        .await;
    memory_handler
        .finish()
        .await
        .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    handler.approval_required |=
        memory_approval_required.load(std::sync::atomic::Ordering::Acquire);
    if let Err(error) = &turn {
        let diagnostic = failure::diagnostic(error, Some(operation_id), conversation_id, config);
        crate::diagnostics::emit_terminal(diagnostic.clone());
        handler
            .reporter
            .managed_observation(&ManagedClientEvent::TerminalDiagnostic(diagnostic))
            .map_err(|error| OneShotFailure::new(ExitCategory::Runtime, error.to_string()))?;
    }
    match turn {
        Ok(_) if handler.approval_required => Err(OneShotFailure::new(
            ExitCategory::Approval,
            "one-shot managed execution required interactive approval and was denied",
        )),
        Ok(result) => Ok(OneShotSuccess {
            text: if result.final_text.is_empty() {
                handler.assistant
            } else {
                result.final_text
            },
            session_id: None,
            conversation_id,
            execution_owner: "managed_codex",
        }),
        Err(error) if handler.approval_required => Err(OneShotFailure::new(
            ExitCategory::Approval,
            format!("managed approval was denied: {error}"),
        )),
        Err(error) => Err(OneShotFailure::new(
            ExitCategory::Runtime,
            error.to_string(),
        )),
    }
}

struct OneShotManagedHandler<'a, 'output> {
    reporter: &'a mut OneShotReporter<'output>,
    assistant: String,
    approval_required: bool,
}

impl<'a, 'output> OneShotManagedHandler<'a, 'output> {
    fn new(reporter: &'a mut OneShotReporter<'output>) -> Self {
        Self {
            reporter,
            assistant: String::new(),
            approval_required: false,
        }
    }
}

impl crate::managed::codex::ManagedEventHandler for OneShotManagedHandler<'_, '_> {
    fn notification(
        &mut self,
        notification: crate::managed::codex::ManagedNotification,
    ) -> std::result::Result<(), CodexError> {
        let Some(event) = ManagedClientEvent::from_notification(notification) else {
            return Ok(());
        };
        self.reporter
            .managed_observation(&event)
            .map_err(|error| CodexError::Io(error.to_string()))?;
        match event {
            ManagedClientEvent::AssistantDelta(delta) => self.assistant.push_str(&delta),
            ManagedClientEvent::Warning(message) => {
                self.reporter
                    .activity("managed.warning", &format!("Codex warning: {message}"))
                    .map_err(|error| CodexError::Io(error.to_string()))?;
            }
            ManagedClientEvent::ItemStarted(item) => {
                self.reporter
                    .activity(
                        "managed.item_started",
                        &format!("Codex started {}", item.label),
                    )
                    .map_err(|error| CodexError::Io(error.to_string()))?;
            }
            ManagedClientEvent::ItemCompleted(item) => {
                self.reporter
                    .activity(
                        "managed.item_completed",
                        &format!("Codex finished {}", item.label),
                    )
                    .map_err(|error| CodexError::Io(error.to_string()))?;
            }
            ManagedClientEvent::ModelRerouted {
                from_model,
                to_model,
                reason,
            } => {
                self.reporter
                    .activity(
                        "managed.model_rerouted",
                        &format!("Codex rerouted {from_model} to {to_model}: {reason}"),
                    )
                    .map_err(|error| CodexError::Io(error.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn approve<'a>(
        &'a mut self,
        request: crate::managed::codex::ApprovalRequest,
    ) -> BoxFuture<'a, std::result::Result<crate::managed::codex::ApprovalDecision, CodexError>>
    {
        self.approval_required = true;
        let decision = if request.available_decisions.contains("decline") {
            Ok(crate::managed::codex::ApprovalDecision::Decline)
        } else if request.available_decisions.contains("cancel") {
            Ok(crate::managed::codex::ApprovalDecision::Cancel)
        } else {
            Err(CodexError::Protocol(
                "managed approval offered no fail-closed decision".to_owned(),
            ))
        };
        Box::pin(async move { decision })
    }
}

fn unavailable_model_message(
    connection: &str,
    requested: &str,
    available: &[ModelDescriptor],
) -> String {
    const MAX_SHOWN: usize = 8;

    let mut advertised = available
        .iter()
        .take(MAX_SHOWN)
        .map(|model| {
            if model.is_default {
                format!("{} (default)", model.id)
            } else {
                model.id.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if available.len() > MAX_SHOWN {
        advertised.push_str(&format!(", and {} more", available.len() - MAX_SHOWN));
    }
    if advertised.is_empty() {
        advertised.push_str("none");
    }

    format!(
        "Codex does not advertise model {requested:?}. Advertised models: {advertised}. \
Select one with `xana model use {connection}/MODEL` (from a source checkout: \
`cargo run -- model use {connection}/MODEL`)"
    )
}

async fn ensure_thread_loaded<H: crate::managed::codex::ManagedEventHandler>(
    server: &mut CodexAppServer,
    thread: &mut ManagedThreadState,
    store: &mut ManagedThreadStore,
    config: &ManagedChatConfig,
    handler: &mut H,
) -> Result<String, CodexError> {
    let expected = if config.memory.is_some() {
        crate::memory::tools::definitions()
    } else {
        Vec::new()
    };
    if handler.memory_tool_definitions() != expected {
        return Err(CodexError::Protocol("managed foreground thread requires the exact personal-memory tool definitions; no thread was opened".into()));
    }
    let id = match thread {
        ManagedThreadState::New { conversation_id } => {
            let id = server
                .start_thread(
                    &config.model,
                    &config.workspace,
                    &config.developer_instructions,
                    handler,
                )
                .await?;
            store
                .set_thread(
                    Some(*conversation_id),
                    Some(id.clone()),
                    Some(config.identity_version),
                )
                .map_err(|error| CodexError::Io(error.to_string()))?;
            store
                .mark_memory_tools_current(&id, config.memory.is_some())
                .map_err(|error| CodexError::Io(error.to_string()))?;
            (*conversation_id, id)
        }
        ManagedThreadState::NeedsResume {
            conversation_id,
            thread_id,
            ..
        } => {
            require_memory_tools(store, thread_id, config.memory.is_some())?;
            server
                .resume_thread(
                    thread_id,
                    &config.model,
                    &config.workspace,
                    &config.developer_instructions,
                    handler,
                )
                .await?;
            (*conversation_id, thread_id.clone())
        }
        ManagedThreadState::Loaded { thread_id, .. } => {
            require_memory_tools(store, thread_id, config.memory.is_some())?;
            return Ok(thread_id.clone());
        }
    };
    *thread = ManagedThreadState::Loaded {
        conversation_id: id.0,
        thread_id: id.1.clone(),
    };
    Ok(id.1)
}

fn require_memory_tools(
    store: &ManagedThreadStore,
    thread_id: &str,
    available: bool,
) -> Result<(), CodexError> {
    if !store.memory_tools_current(thread_id, available) {
        return Err(CodexError::Protocol(
            "This managed thread's personal-memory tool registration is unknown or differs from the current contract or storage availability. Start a new conversation (/clear in chat); the old Codex thread is retained. No turn was started.".into(),
        ));
    }
    Ok(())
}

fn require_one_shot_target(
    store: &ManagedThreadStore,
    conversation: &ConversationRef,
    connection: &str,
) -> Result<(), CodexError> {
    if let ConversationRef::Managed {
        conversation_id,
        connection: requested,
        thread_id,
    } = conversation
        && requested == connection
        && store.conversation_id() == Some(*conversation_id)
        && store.thread_id() == Some(thread_id.as_str())
    {
        return Ok(());
    }
    Err(CodexError::Protocol(
        "--continue requires the selected managed thread to match the requested Conversation and connection; no owner input or model turn was sent. Select the intended conversation or start a new one.".into(),
    ))
}

fn print_models(models: &[crate::model_catalog::ModelDescriptor], selected: &str) {
    for descriptor in models {
        let marker = if descriptor.id == selected { "*" } else { " " };
        let efforts = descriptor
            .reasoning_efforts
            .iter()
            .map(|effort| effort.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let reasoning = if efforts.is_empty() {
            "reasoning: unspecified".to_owned()
        } else {
            format!(
                "reasoning: {efforts}; default {}",
                descriptor
                    .default_reasoning_effort
                    .as_deref()
                    .unwrap_or("unspecified")
            )
        };
        println!(
            "xana> {marker} {} - {} ({reasoning})",
            descriptor.id, descriptor.display_name
        );
    }
}

fn print_reasoning_status(
    selection: &crate::model_catalog::ModelSelection,
    models: &[crate::model_catalog::ModelDescriptor],
    model: &str,
) {
    println!(
        "xana> reasoning effort: {}",
        selection
            .reasoning_effort
            .as_deref()
            .unwrap_or("model default")
    );
    if let Some(descriptor) = models.iter().find(|descriptor| descriptor.id == model) {
        let options = descriptor
            .reasoning_efforts
            .iter()
            .map(|effort| effort.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "xana> advertised efforts: {}",
            if options.is_empty() {
                "none; refresh the Codex catalog"
            } else {
                &options
            }
        );
        for effort in &descriptor.reasoning_efforts {
            if !effort.description.is_empty() {
                println!("  {}: {}", effort.id, effort.description);
            }
        }
    }
}

fn local_control(input: &str) -> Option<ChatExit> {
    let parsed =
        crate::command_catalog::parse(input, crate::command_catalog::CommandSurface::Plain).ok()?;
    let (family, default) = crate::command_catalog::suspended_chat_control(parsed.stable_id)?;
    Some(ChatExit::ControlCommand {
        family: family.into(),
        arguments: if parsed.arguments.is_empty() {
            default.into()
        } else {
            parsed.arguments
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        managed::codex::{ApprovalRequest, ManagedEventHandler, ManagedNotification},
        model_catalog::DescriptorSource,
    };
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    #[test]
    fn managed_memory_contract_receipt_gates_legacy_resume_and_survives_selection() {
        let home = tempdir().unwrap();
        let workspace = home.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let conversation = ConversationId::new();
        let mut store = ManagedThreadStore::open(home.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(conversation),
                Some("legacy-thread".into()),
                Some("identity-v1"),
            )
            .unwrap();
        assert!(require_memory_tools(&store, "legacy-thread", true).is_err());
        assert!(require_memory_tools(&store, "legacy-thread", false).is_err());
        store
            .mark_memory_tools_current("legacy-thread", true)
            .unwrap();
        store.select_thread("legacy-thread").unwrap();
        assert!(require_memory_tools(&store, "legacy-thread", true).is_ok());
        assert!(require_memory_tools(&store, "legacy-thread", false).is_err());
        store
            .set_thread(
                Some(ConversationId::new()),
                Some("without-memory".into()),
                Some("identity-v1"),
            )
            .unwrap();
        store
            .mark_memory_tools_current("without-memory", false)
            .unwrap();
        assert!(require_memory_tools(&store, "without-memory", false).is_ok());
        assert!(require_memory_tools(&store, "without-memory", true).is_err());
        drop(store);
        let reopened = ManagedThreadStore::open(home.path(), "codex", &workspace).unwrap();
        assert!(require_memory_tools(&reopened, "legacy-thread", true).is_ok());
        assert!(require_memory_tools(&reopened, "unknown-thread", true).is_err());
        assert!(require_memory_tools(&reopened, "without-memory", false).is_ok());
        assert!(require_memory_tools(&reopened, "without-memory", true).is_err());
    }

    #[test]
    fn managed_one_shot_owner_source_cannot_follow_a_different_selected_thread() {
        let home = tempdir().unwrap();
        let workspace = home.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let conversation_id = ConversationId::new();
        let mut store = ManagedThreadStore::open(home.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(conversation_id),
                Some("selected-thread".into()),
                Some("identity-v1"),
            )
            .unwrap();
        let target = |conversation_id, connection: &str, thread: &str| ConversationRef::Managed {
            conversation_id,
            connection: connection.into(),
            thread_id: thread.into(),
        };
        assert!(
            require_one_shot_target(
                &store,
                &target(conversation_id, "codex", "selected-thread"),
                "codex"
            )
            .is_ok()
        );
        for requested in [
            target(ConversationId::new(), "codex", "selected-thread"),
            target(conversation_id, "codex", "another-thread"),
            target(conversation_id, "another-connection", "selected-thread"),
        ] {
            assert!(require_one_shot_target(&store, &requested, "codex").is_err());
        }
    }

    #[test]
    fn local_accounting_commands_never_become_managed_prompts() {
        for (input, family, arguments) in [
            ("/budget", "budget", ""),
            (
                "/budget --daily-requests 50",
                "budget",
                "--daily-requests 50",
            ),
            ("/usage ledger --root abc", "usage", "ledger --root abc"),
            ("/storage", "storage", "status"),
        ] {
            assert_eq!(
                local_control(input),
                Some(ChatExit::ControlCommand {
                    family: family.into(),
                    arguments: arguments.into()
                })
            );
        }
        assert!(local_control("/usage").is_none());
        assert!(local_control("tell me about budgets").is_none());
    }

    fn model(id: &str, is_default: bool) -> ModelDescriptor {
        ModelDescriptor {
            id: id.to_owned(),
            display_name: id.to_owned(),
            input_modalities: BTreeSet::new(),
            output_modalities: BTreeSet::new(),
            tools: Some(true),
            reasoning: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            context_tokens: None,
            max_output_tokens: None,
            pricing: crate::model_catalog::ModelPricing::default(),
            source: DescriptorSource::ManagedRuntime,
            is_default,
        }
    }

    #[test]
    fn unavailable_model_guidance_names_live_choices_and_both_launch_forms() {
        let message = unavailable_model_message(
            "codex",
            "not-advertised",
            &[model("model-a", false), model("model-b", true)],
        );

        assert!(message.contains("model-a, model-b (default)"));
        assert!(message.contains("xana model use codex/MODEL"));
        assert!(message.contains("cargo run -- model use codex/MODEL"));
        assert!(!message.contains("model refresh"));
    }

    #[test]
    fn explicit_new_managed_conversation_does_not_resume_the_selected_thread() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut store = ManagedThreadStore::open(directory.path(), "codex", &workspace).unwrap();
        store
            .set_thread(
                Some(ConversationId::new()),
                Some("existing-thread".to_owned()),
                Some("identity-v1"),
            )
            .unwrap();

        let conversation_id = ConversationId::new();
        let (initial, state) = initial_managed_thread(
            &ConversationRef::NewManaged {
                conversation_id,
                connection: "codex".to_owned(),
            },
            &store,
            "codex",
            "identity-v1",
        )
        .unwrap();

        assert_eq!(initial, None);
        assert!(matches!(
            state,
            ManagedThreadState::New {
                conversation_id: actual
            } if actual == conversation_id
        ));
    }

    #[test]
    fn one_shot_managed_projection_collects_output_without_vendor_ids() {
        let mut activity = Vec::new();
        {
            let mut reporter = OneShotReporter::text(&mut activity);
            let mut handler = OneShotManagedHandler::new(&mut reporter);
            handler
                .notification(ManagedNotification::AssistantDelta {
                    item_id: Some("private-item-id".to_owned()),
                    delta: "hello".to_owned(),
                })
                .expect("assistant delta");
            handler
                .notification(ManagedNotification::Warning("bounded warning".to_owned()))
                .expect("warning");

            assert_eq!(handler.assistant, "hello");
        }
        let rendered = String::from_utf8(activity).expect("UTF-8 activity");
        assert!(rendered.contains("bounded warning"));
        assert!(!rendered.contains("private-item-id"));
    }

    #[tokio::test]
    async fn one_shot_managed_approval_fails_closed() {
        let mut activity = Vec::new();
        {
            let mut reporter = OneShotReporter::text(&mut activity);
            let mut handler = OneShotManagedHandler::new(&mut reporter);
            let decision = handler
                .approve(ApprovalRequest {
                    item_id: Some("private-item-id".to_owned()),
                    method: "item/commandExecution/requestApproval".to_owned(),
                    available_decisions: ["decline".to_owned()].into_iter().collect(),
                    reason: None,
                    command: Some("echo secret".to_owned()),
                    cwd: None,
                })
                .await
                .expect("decline is available");

            assert_eq!(decision, crate::managed::codex::ApprovalDecision::Decline);
            assert!(handler.approval_required);
        }
        assert!(activity.is_empty());
    }
}
