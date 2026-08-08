//! Terminal projection for a foreign managed-agent runtime.

mod activity;

use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    managed::{
        codex::{AccountStatus, CodexAppServer, CodexError, ManagedTurnInput, ManagedTurnOptions},
        thread_store::ManagedThreadStore,
    },
    model::{ModelDescriptor, ModelManager, ReasoningSummary},
    vision::{ImageIngestor, ImageLimits, PendingImages},
};
use activity::{ActivityLevel, RetainedActivity, TerminalManagedHandler, render_retained_activity};
use anyhow::{Context, Result};
use rustyline::{DefaultEditor, error::ReadlineError};
use std::path::{Path, PathBuf};

pub(crate) struct ManagedChatConfig {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) workspace: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) owner: PrincipalId,
}

enum ManagedThreadState {
    New,
    NeedsResume(String),
    Loaded(String),
}

pub(crate) async fn run_codex_chat(
    mut server: CodexAppServer,
    models: ModelManager,
    mut config: ManagedChatConfig,
) -> Result<()> {
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
    let mut selection = models.selected()?;
    config.model = selection.model.clone();

    let mut thread_store =
        ManagedThreadStore::open(&config.data_root, &config.connection, &config.workspace)?;
    let mut thread = thread_store
        .thread_id()
        .map(|id| ManagedThreadState::NeedsResume(id.to_owned()))
        .unwrap_or(ManagedThreadState::New);
    let mut activity = ActivityLevel::Normal;
    let mut last_activity = RetainedActivity::default();

    println!("provider connection: {}", config.connection);
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
    if let ManagedThreadState::NeedsResume(id) = &thread {
        println!("managed thread: {id} (will resume on the first turn)");
    }
    println!(
        "/model, /reasoning, /reasoning-summary, /activity, and /details control this managed conversation; /attach adds an image; /clear starts a new Codex thread; /quit exits"
    );

    let mut editor = DefaultEditor::new().context("could not initialize terminal editor")?;
    let mut pending = PendingImages::default();
    let ingestor = ImageIngestor::new(config.artifact_store.clone(), ImageLimits::default());

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
        if input == "/clear" {
            thread_store.set_thread_id(None)?;
            thread = ManagedThreadState::New;
            last_activity = RetainedActivity::default();
            let cleared = pending.clear();
            println!("xana> new managed thread will start; cleared {cleared} pending image(s)");
            continue;
        }
        if input == "/details" {
            render_retained_activity(&last_activity)?;
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
        let local_images = match attachments
            .iter()
            .map(|attachment| checked_original_path(&config.workspace, &attachment.source_path))
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

        let mut handler = TerminalManagedHandler::new(activity);
        let loaded_thread_id = match ensure_thread_loaded(
            &mut server,
            &mut thread,
            &mut thread_store,
            &config,
            &mut handler,
        )
        .await
        {
            Ok(id) => id,
            Err(error) => {
                for attachment in attachments {
                    pending.push(attachment);
                }
                println!("xana> could not open managed thread: {error}");
                if matches!(thread, ManagedThreadState::NeedsResume(_)) {
                    println!("xana> use /clear to explicitly start a new Codex thread");
                }
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
                    text: input.to_owned(),
                    local_images,
                },
                &mut handler,
            )
            .await;
        handler.finish_stream()?;
        last_activity = handler.into_retained();
        match result {
            Ok(result) => {
                if !last_activity.assistant_streamed && !result.final_text.is_empty() {
                    println!("xana> {}", result.final_text);
                }
            }
            Err(error) => println!("xana> managed turn failed: {error}"),
        }
    }
    server.shutdown().await?;
    Ok(())
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

async fn ensure_thread_loaded(
    server: &mut CodexAppServer,
    thread: &mut ManagedThreadState,
    store: &mut ManagedThreadStore,
    config: &ManagedChatConfig,
    handler: &mut TerminalManagedHandler,
) -> Result<String, CodexError> {
    let id = match thread {
        ManagedThreadState::New => {
            server
                .start_thread(&config.model, &config.workspace, handler)
                .await?
        }
        ManagedThreadState::NeedsResume(id) => {
            server
                .resume_thread(id, &config.model, &config.workspace, handler)
                .await?
        }
        ManagedThreadState::Loaded(id) => return Ok(id.clone()),
    };
    store
        .set_thread_id(Some(id.clone()))
        .map_err(|error| CodexError::Io(error.to_string()))?;
    *thread = ManagedThreadState::Loaded(id.clone());
    Ok(id)
}

fn print_models(models: &[crate::model::ModelDescriptor], selected: &str) {
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
    selection: &crate::model::ModelSelection,
    models: &[crate::model::ModelDescriptor],
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

fn checked_original_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let root = workspace
        .canonicalize()
        .context("could not canonicalize managed workspace")?;
    let path = root
        .join(relative)
        .canonicalize()
        .with_context(|| format!("could not resolve attached image {relative:?}"))?;
    if !path.starts_with(&root) {
        anyhow::bail!("attached image resolves outside the managed workspace")
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DescriptorSource;
    use std::collections::BTreeSet;

    fn model(id: &str, is_default: bool) -> ModelDescriptor {
        ModelDescriptor {
            id: id.to_owned(),
            display_name: id.to_owned(),
            input_modalities: BTreeSet::new(),
            tools: Some(true),
            reasoning: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            context_tokens: None,
            max_output_tokens: None,
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
}
