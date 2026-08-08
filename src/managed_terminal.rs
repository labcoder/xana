//! Terminal projection for a foreign managed-agent runtime.

use crate::{
    artifact::ArtifactStore,
    identity::PrincipalId,
    managed::codex::{
        AccountStatus, ApprovalDecision, ApprovalRequest, CodexAppServer, CodexError,
        ManagedEventHandler, ManagedNotification, ManagedTurnInput,
    },
    model::ModelManager,
    vision::{ImageIngestor, ImageLimits, PendingImages},
};
use anyhow::{Context, Result};
use rustyline::{DefaultEditor, error::ReadlineError};
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

pub(crate) struct ManagedChatConfig {
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) workspace: PathBuf,
    pub(crate) artifact_store: ArtifactStore,
    pub(crate) owner: PrincipalId,
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
    if !available.iter().any(|model| model.id == config.model) {
        anyhow::bail!(
            "Codex does not advertise model {:?}; run `xana model refresh {}`",
            config.model,
            config.connection
        );
    }
    models.write_managed_cache(&config.connection, &available)?;

    println!("provider connection: {}", config.connection);
    println!("execution: managed Codex app-server ({})", server.version);
    println!("model: {}", config.model);
    println!("workspace: {}", config.workspace.display());
    println!(
        "/model lists or switches Codex models; /attach adds an image; /clear starts a new Codex thread; /quit exits"
    );

    let mut editor = DefaultEditor::new().context("could not initialize terminal editor")?;
    let mut thread_id = None::<String>;
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
            thread_id = None;
            let cleared = pending.clear();
            println!("xana> new managed thread will start; cleared {cleared} pending image(s)");
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
        if let Some(selection) = input.strip_prefix("/model").map(str::trim) {
            if selection.is_empty() {
                available = server.models().await?;
                models.write_managed_cache(&config.connection, &available)?;
                for descriptor in &available {
                    let marker = if descriptor.id == config.model {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "xana> {marker} {} - {}",
                        descriptor.id, descriptor.display_name
                    );
                }
                continue;
            }
            let (connection, requested_model) = selection.split_once('/').map_or(
                (config.connection.as_str(), selection),
                |(connection, model)| (connection, model),
            );
            if connection != config.connection {
                println!(
                    "xana> switching between native and managed runtimes starts a new conversation; run `xana model use {selection}` and restart Xana"
                );
                continue;
            }
            available = server.models().await?;
            models.write_managed_cache(&config.connection, &available)?;
            if !available.iter().any(|model| model.id == requested_model) {
                println!("xana> Codex does not advertise model {requested_model:?}");
                continue;
            }
            models.select(&config.connection, requested_model)?;
            config.model = requested_model.to_owned();
            println!("xana> selected {} for subsequent turns", config.model);
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
        let mut handler = TerminalManagedHandler::default();
        let result = server
            .run_turn(
                &config.model,
                &config.workspace,
                thread_id.as_deref(),
                ManagedTurnInput {
                    text: input.to_owned(),
                    local_images,
                },
                &mut handler,
            )
            .await;
        match result {
            Ok(result) => {
                thread_id = Some(result.thread_id);
                if !handler.streamed && !result.final_text.is_empty() {
                    println!("xana> {}", result.final_text);
                } else if handler.streamed {
                    println!();
                }
            }
            Err(error) => println!("xana> managed turn failed: {error}"),
        }
    }
    server.shutdown().await?;
    Ok(())
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

#[derive(Default)]
struct TerminalManagedHandler {
    streamed: bool,
    pending_items: BTreeMap<String, (String, String)>,
}

impl ManagedEventHandler for TerminalManagedHandler {
    fn notification(&mut self, notification: ManagedNotification) -> Result<(), CodexError> {
        match notification {
            ManagedNotification::TextDelta(delta) => {
                if !self.streamed {
                    print!("xana> ");
                    self.streamed = true;
                }
                print!("{delta}");
                io::stdout()
                    .flush()
                    .map_err(|error| CodexError::Io(error.to_string()))?;
            }
            ManagedNotification::Warning(message) => {
                if self.streamed {
                    println!();
                    self.streamed = false;
                }
                println!("xana> Codex warning: {message}");
            }
            ManagedNotification::ItemStarted {
                item_id,
                kind,
                summary,
            } => {
                self.pending_items.insert(item_id, (kind, summary));
            }
            ManagedNotification::TurnCompleted { status, .. } if status != "completed" => {
                println!("xana> Codex turn ended with status {status}");
            }
            ManagedNotification::TurnCompleted { .. } | ManagedNotification::Other { .. } => {}
        }
        Ok(())
    }

    fn approve(&mut self, request: ApprovalRequest) -> Result<ApprovalDecision, CodexError> {
        if self.streamed {
            println!();
            self.streamed = false;
        }
        println!("xana> Codex requests approval");
        println!("request: {}", request.method);
        if let Some(item_id) = &request.item_id
            && let Some((kind, summary)) = self.pending_items.remove(item_id)
        {
            println!("proposed {kind}:");
            println!("{summary}");
        }
        if let Some(command) = request.command {
            println!("command: {command}");
        }
        if let Some(cwd) = request.cwd {
            println!("cwd: {cwd}");
        }
        if let Some(reason) = request.reason {
            println!("reason: {reason}");
        }
        let safe_rejection = if request.available_decisions.contains("decline") {
            Some(ApprovalDecision::Decline)
        } else if request.available_decisions.contains("cancel") {
            Some(ApprovalDecision::Cancel)
        } else {
            None
        };
        if !io::stdin().is_terminal() {
            return safe_rejection.ok_or_else(|| {
                CodexError::Protocol(
                    "approval request offered no supported fail-closed decision".into(),
                )
            });
        }
        let once_allowed = request.available_decisions.contains("accept");
        let session_allowed = request.available_decisions.contains("acceptForSession");
        if once_allowed && session_allowed {
            print!("Allow? [y] once, [a] session, [n] decline, [c] cancel: ");
        } else if once_allowed {
            print!("Allow? [y] once, [n] decline, [c] cancel: ");
        } else {
            print!("Xana cannot grant the advertised amendment safely; [n] decline, [c] cancel: ");
        }
        io::stdout()
            .flush()
            .map_err(|error| CodexError::Io(error.to_string()))?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|error| CodexError::Io(error.to_string()))?;
        Ok(match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" if once_allowed => ApprovalDecision::AcceptOnce,
            "a" | "session" if session_allowed => ApprovalDecision::AcceptForSession,
            "c" | "cancel" if request.available_decisions.contains("cancel") => {
                ApprovalDecision::Cancel
            }
            _ => safe_rejection.ok_or_else(|| {
                CodexError::Protocol(
                    "approval request offered no supported fail-closed decision".into(),
                )
            })?,
        })
    }
}
