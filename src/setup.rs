//! Provider-neutral quick setup and its bounded installation transaction.
//!
//! Connection establishment and catalog discovery happen before model choice.
//! Ordinary configuration and secrets remain staged until the final review is
//! confirmed; the installer then rolls credential mutation back if the atomic
//! configuration replacement fails.

mod custom;
mod readiness;
mod ui;

pub(crate) use readiness::{SetupPending, run as run_if_needed};

use crate::{
    cli::SetupArgs,
    config::{
        CredentialReference, InitialConfig, InitialConnection, PermissionMode, ProviderKind,
        XanaConfig,
    },
    credential::{OsSecretStore, SecretStore, SecretString},
    managed::codex::{AccountStatus, CodexAppServer, CodexLaunchConfig},
    model_catalog::{ModelDescriptor, ModelManager},
    paths::XanaPaths,
    presentation::{ResolvedPresentation, SemanticToken},
    shell::ShellConfig,
};
use anyhow::{Context, Result, bail};
use std::{
    error::Error,
    fmt, fs, io,
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
};
use ui::{choose_setup_path, write_completion_receipt, write_setup_heading};

const MAX_SECRET_BYTES: u64 = 64 * 1024;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_SELECTION_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
struct SetupCancelled;

impl fmt::Display for SetupCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("setup cancelled at end of input; no changes made")
    }
}

impl Error for SetupCancelled {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupOutcome {
    Unchanged,
    Committed { requires_new_conversation: bool },
}

impl SetupOutcome {
    pub(crate) fn requires_new_conversation(self) -> bool {
        matches!(
            self,
            Self::Committed {
                requires_new_conversation: true
            }
        )
    }

    pub(crate) fn starts_new_conversation(self, requested: bool) -> bool {
        requested && self.requires_new_conversation()
    }
}

pub(crate) fn args_for_request(request: &str) -> Result<SetupArgs> {
    let mut args = SetupArgs::default();
    match request.trim() {
        "" => {}
        "quick" => args.quick = true,
        "full" => args.full = true,
        "connection" => args.section = Some(crate::cli::SetupSectionChoice::Connection),
        "permissions-shell" => {
            args.section = Some(crate::cli::SetupSectionChoice::PermissionsShell);
        }
        "profiles-routes" => {
            args.section = Some(crate::cli::SetupSectionChoice::ProfilesRoutes);
        }
        "appearance" => args.section = Some(crate::cli::SetupSectionChoice::Appearance),
        value => bail!(
            "unknown setup section {value:?}; use quick, full, connection, permissions-shell, profiles-routes, or appearance"
        ),
    }
    Ok(args)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SetupCredential {
    None,
    Environment(String),
    Stored { id: String },
}

impl SetupCredential {
    fn reference(&self) -> Option<CredentialReference> {
        match self {
            Self::None => None,
            Self::Environment(variable) => Some(CredentialReference::Environment {
                variable: variable.clone(),
            }),
            Self::Stored { id } => Some(CredentialReference::Stored { id: id.clone() }),
        }
    }

    fn review(&self) -> String {
        match self {
            Self::None => "not required".to_owned(),
            Self::Environment(variable) => format!("environment variable {variable}"),
            Self::Stored { id } => format!("operating-system store entry {id}"),
        }
    }
}

#[derive(Debug)]
struct SetupDraft {
    kind: ProviderKind,
    connection: String,
    base_url: Option<String>,
    codex_program: Option<String>,
    codex_home: Option<PathBuf>,
    credential: SetupCredential,
    staged_secret: Option<SecretString>,
    model: String,
    reasoning_effort: Option<String>,
    permission_mode: PermissionMode,
}

impl SetupDraft {
    fn initial_config(&self) -> InitialConfig {
        let connection = if self.kind == ProviderKind::Codex {
            InitialConnection::Codex {
                name: self.connection.clone(),
                program: self
                    .codex_program
                    .clone()
                    .unwrap_or_else(|| "codex".to_owned()),
                home: self.codex_home.clone(),
            }
        } else {
            InitialConnection::Native {
                name: self.connection.clone(),
                kind: self.kind,
                base_url: self.base_url.clone(),
                credential: self.credential.reference(),
            }
        };
        InitialConfig {
            connection,
            model: self.model.clone(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: self.permission_mode,
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }
}

pub(crate) async fn run(
    args: &SetupArgs,
    paths: &XanaPaths,
    input_is_terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
    profile: ResolvedPresentation,
) -> Result<SetupOutcome> {
    if args.if_needed {
        bail!("--if-needed must be dispatched through Xana's readiness owner");
    }
    if args.non_interactive && input_is_terminal {
        // This is allowed, but the mode contract remains flag-driven.
    }
    if !args.non_interactive && !input_is_terminal {
        bail!(
            "interactive setup requires a terminal; use `xana setup --non-interactive --kind KIND --connection NAME --model MODEL --permission-mode MODE --yes`"
        );
    }
    if args.non_interactive && !args.dry_run && !args.yes {
        bail!("noninteractive setup requires --yes before changing durable state");
    }
    let Some(args) = choose_setup_path(args, input, output, profile)? else {
        writeln!(output, "Setup cancelled; no changes made.")?;
        return Ok(SetupOutcome::Unchanged);
    };
    let args = &args;
    if args
        .section
        .is_some_and(|section| section != crate::cli::SetupSectionChoice::Connection)
    {
        let outcome = custom::run_section(args, paths, input, output)?;
        if matches!(outcome, SetupOutcome::Committed { .. }) {
            write_completion_receipt(output, paths, profile)?;
        }
        return Ok(outcome);
    }

    write_setup_heading(output, profile, "Xana Quick Setup")?;
    writeln!(
        output,
        "{}",
        profile.paint(
            SemanticToken::Muted,
            "No provider is selected or recommended by Xana. Press Ctrl+C to cancel safely."
        )
    )?;

    let kind = choose_kind(args, input, output, profile)?;
    let connection = choose_connection(args, kind, input, output)?;
    let base_url = choose_base_url(args, kind, input, output)?;
    let (codex_program, codex_home) = choose_codex(args, kind, input, output)?;
    let (credential, staged_secret) = choose_credential(args, kind, &connection, input, output)?;

    writeln!(output)?;
    writeln!(output, "Establishing connection before model selection...")?;
    let provisional = SetupDraft {
        kind,
        connection: connection.clone(),
        base_url: base_url.clone(),
        codex_program: codex_program.clone(),
        codex_home: codex_home.clone(),
        credential: credential.clone(),
        staged_secret,
        model: "setup-probe".to_owned(),
        reasoning_effort: None,
        permission_mode: PermissionMode::Ask,
    };
    let models = establish(&provisional, paths).await?;
    if models.is_empty() {
        bail!("the established connection advertised no selectable models");
    }
    writeln!(
        output,
        "{}",
        profile.paint(
            SemanticToken::Success,
            &format!(
                "[OK] Connection established; {} model(s) available.",
                models.len()
            )
        )
    )?;

    let model = choose_model(args, &models, input, output, profile)?;
    let descriptor = models
        .iter()
        .find(|candidate| candidate.id == model)
        .expect("choose_model returns an advertised model");
    let reasoning_effort = choose_reasoning(args, kind, descriptor, input, output)?;
    let permission_mode = choose_permission(args, input, output)?;

    let draft = SetupDraft {
        model,
        reasoning_effort,
        permission_mode,
        ..provisional
    };
    let rendered = XanaConfig::render_initial(draft.initial_config())
        .context("could not render the validated setup configuration")?;
    let customization = custom::customize_quick(rendered, args, paths, input, output)?;
    let rendered = customization.config;

    writeln!(output)?;
    writeln!(output, "{}", profile.paint(SemanticToken::Accent, "Review"))?;
    writeln!(output, "  Connection:  {}", draft.connection)?;
    writeln!(output, "  Kind:        {}", draft.kind.as_str())?;
    if let Some(base_url) = &draft.base_url {
        writeln!(output, "  Endpoint:    {base_url}")?;
    }
    if draft.kind == ProviderKind::Codex {
        writeln!(
            output,
            "  Executable:  {}",
            draft.codex_program.as_deref().unwrap_or("codex")
        )?;
        if let Some(home) = &draft.codex_home {
            writeln!(output, "  Codex home:  {}", home.display())?;
        }
        writeln!(output, "  Account:     owned by Codex")?;
    } else {
        writeln!(output, "  Credential:  {}", draft.credential.review())?;
    }
    writeln!(output, "  Model:       {}", draft.model)?;
    if let Some(effort) = &draft.reasoning_effort {
        writeln!(output, "  Reasoning:   {effort}")?;
    }
    writeln!(output, "  Permissions: {}", draft.permission_mode.as_str())?;
    writeln!(output, "  Config:      {}", paths.config_file().display())?;
    for effect in &customization.effects {
        writeln!(output, "  Applies:     {effect}")?;
    }

    if args.dry_run {
        writeln!(output, "Validated preview only; no durable state changed.")?;
        return Ok(SetupOutcome::Unchanged);
    }
    if !args.yes && !confirm(input, output, "Install this configuration? [y/N]: ")? {
        writeln!(output, "No changes made.")?;
        return Ok(SetupOutcome::Unchanged);
    }

    let selection_path = paths.data_dir().join("selection.toml");
    install_with_preferences(
        paths.config_file(),
        &rendered,
        match (&draft.credential, draft.staged_secret.as_ref()) {
            (SetupCredential::Stored { id }, Some(secret)) => Some((id.as_str(), secret)),
            _ => None,
        },
        &OsSecretStore,
        customization
            .preferences
            .as_ref()
            .map(|rendered| (paths.presentation_file(), rendered.as_bytes())),
        Some(&selection_path),
    )?;
    write_completion_receipt(output, paths, profile)?;
    Ok(SetupOutcome::Committed {
        requires_new_conversation: true,
    })
}

fn choose_kind(
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    profile: ResolvedPresentation,
) -> Result<ProviderKind> {
    if let Some(kind) = args.kind {
        return Ok(kind.into());
    }
    if args.non_interactive {
        bail!("noninteractive setup requires --kind");
    }
    writeln!(output)?;
    writeln!(
        output,
        "{}",
        profile.paint(SemanticToken::Accent, "Choose a connection kind:")
    )?;
    for (number, label, detail) in [
        (1, "Ollama", "local Ollama endpoint; no API key"),
        (
            2,
            "OpenAI-compatible",
            "custom endpoint; key may be optional",
        ),
        (3, "OpenAI API", "OpenAI platform API key"),
        (4, "OpenRouter", "OpenRouter API key"),
        (5, "Anthropic", "Anthropic API key"),
        (6, "Codex", "local Codex CLI and vendor-owned account"),
    ] {
        writeln!(
            output,
            "  {} {}. {}  {}",
            profile.paint(
                SemanticToken::Muted,
                if profile.unicode { "○" } else { "o" }
            ),
            profile.paint(SemanticToken::Focus, &number.to_string()),
            label,
            profile.paint(SemanticToken::Muted, detail),
        )?;
    }
    match prompt_required(input, output, "Choice [1-6]: ")?.as_str() {
        "1" | "ollama" => Ok(ProviderKind::Ollama),
        "2" | "openai-compatible" | "openai_compat" => Ok(ProviderKind::OpenAiCompat),
        "3" | "openai" => Ok(ProviderKind::OpenAi),
        "4" | "openrouter" => Ok(ProviderKind::OpenRouter),
        "5" | "anthropic" => Ok(ProviderKind::Anthropic),
        "6" | "codex" => Ok(ProviderKind::Codex),
        _ => bail!("unknown connection choice; use 1 through 6"),
    }
}

fn choose_connection(
    args: &SetupArgs,
    kind: ProviderKind,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String> {
    if let Some(value) = &args.connection {
        return Ok(value.clone());
    }
    if args.non_interactive {
        bail!("noninteractive setup requires --connection");
    }
    prompt_default(input, output, "Connection name", kind.as_str())
}

fn choose_base_url(
    args: &SetupArgs,
    kind: ProviderKind,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Option<String>> {
    if kind == ProviderKind::Codex {
        if args.base_url.is_some() {
            bail!("managed Codex uses stdio and does not accept --base-url");
        }
        return Ok(None);
    }
    if let Some(value) = &args.base_url {
        return Ok(Some(value.clone()));
    }
    let default = match kind {
        ProviderKind::Ollama => Some("http://localhost:11434/v1"),
        ProviderKind::OpenAi => Some("https://api.openai.com/v1"),
        ProviderKind::OpenRouter => Some("https://openrouter.ai/api/v1"),
        ProviderKind::Anthropic => Some("https://api.anthropic.com"),
        ProviderKind::OpenAiCompat => None,
        ProviderKind::Codex => unreachable!(),
    };
    if let Some(default) = default {
        return Ok(Some(if args.non_interactive {
            default.to_owned()
        } else {
            prompt_default(input, output, "Endpoint", default)?
        }));
    }
    if args.non_interactive {
        bail!("openai-compatible setup requires --base-url");
    }
    Ok(Some(prompt_required(input, output, "Endpoint URL: ")?))
}

fn choose_codex(
    args: &SetupArgs,
    kind: ProviderKind,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(Option<String>, Option<PathBuf>)> {
    if kind != ProviderKind::Codex {
        if args.codex_program.is_some() || args.codex_home.is_some() {
            bail!("--codex-program and --codex-home require --kind codex");
        }
        return Ok((None, None));
    }
    let program = match &args.codex_program {
        Some(program) => program.clone(),
        None if args.non_interactive => "codex".to_owned(),
        None => prompt_default(input, output, "Codex executable", "codex")?,
    };
    Ok((Some(program), args.codex_home.clone()))
}

fn choose_credential(
    args: &SetupArgs,
    kind: ProviderKind,
    connection: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<(SetupCredential, Option<SecretString>)> {
    if matches!(kind, ProviderKind::Ollama | ProviderKind::Codex) {
        if args.credential_env.is_some() || args.key_from_stdin {
            bail!("the selected connection kind does not accept API-key setup options");
        }
        return Ok((SetupCredential::None, None));
    }
    if let Some(variable) = &args.credential_env {
        return Ok((SetupCredential::Environment(variable.clone()), None));
    }
    if args.key_from_stdin {
        let secret = read_bounded_secret(input)?;
        return Ok((
            SetupCredential::Stored {
                id: connection.to_owned(),
            },
            Some(secret),
        ));
    }
    if args.non_interactive {
        if kind == ProviderKind::OpenAiCompat {
            return Ok((SetupCredential::None, None));
        }
        bail!("API setup requires --credential-env VARIABLE or --key-from-stdin");
    }
    writeln!(output, "Credential source:")?;
    writeln!(output, "  1. environment variable")?;
    writeln!(
        output,
        "  2. operating-system credential store (hidden input)"
    )?;
    if kind == ProviderKind::OpenAiCompat {
        writeln!(
            output,
            "  3. none (endpoint does not require authentication)"
        )?;
    }
    match prompt_required(input, output, "Choice: ")?.as_str() {
        "1" => Ok((
            SetupCredential::Environment(prompt_required(input, output, "Environment variable: ")?),
            None,
        )),
        "2" => {
            let value = rpassword::prompt_password("API key (hidden): ")
                .context("could not read hidden API key")?;
            Ok((
                SetupCredential::Stored {
                    id: connection.to_owned(),
                },
                Some(SecretString::new(value)?),
            ))
        }
        "3" if kind == ProviderKind::OpenAiCompat => Ok((SetupCredential::None, None)),
        _ => bail!("unknown credential-source choice"),
    }
}

async fn establish(draft: &SetupDraft, paths: &XanaPaths) -> Result<Vec<ModelDescriptor>> {
    if draft.kind == ProviderKind::Codex {
        let mut server = CodexAppServer::spawn(&CodexLaunchConfig {
            program: draft
                .codex_program
                .clone()
                .unwrap_or_else(|| "codex".to_owned()),
            home: draft.codex_home.clone(),
        })
        .await
        .context("Codex executable or app-server is unavailable; update Codex CLI and retry")?;
        if matches!(server.account_status().await?, AccountStatus::LoggedOut) {
            bail!(
                "Codex is logged out; run `codex login` (or configure the selected CODEX_HOME), then rerun `xana setup`"
            );
        }
        return server
            .models()
            .await
            .context("Codex account is available but model discovery failed");
    }

    let rendered = XanaConfig::render_initial(draft.initial_config())?;
    let registry = XanaConfig::parse_registry(&rendered)?;
    let manager = ModelManager::new(
        registry,
        paths.cache_dir().join("setup-probe"),
        paths.cache_dir().join("setup-selection.toml"),
    );
    manager
        .probe_native(&draft.connection, draft.staged_secret.as_ref())
        .await
        .context("could not establish the provider endpoint and fetch its live model catalog")
}

fn choose_model(
    args: &SetupArgs,
    models: &[ModelDescriptor],
    input: &mut impl BufRead,
    output: &mut impl Write,
    profile: ResolvedPresentation,
) -> Result<String> {
    if let Some(requested) = &args.model {
        if models.iter().any(|model| model.id == *requested) {
            return Ok(requested.clone());
        }
        bail!(
            "the established connection does not advertise model {:?}; choose one returned by the live catalog",
            requested
        );
    }
    if args.non_interactive {
        bail!("noninteractive setup requires --model");
    }
    writeln!(
        output,
        "{}",
        profile.paint(SemanticToken::Accent, "Available models:")
    )?;
    for (index, model) in models.iter().enumerate() {
        writeln!(
            output,
            "  {} {}. {}",
            profile.paint(
                SemanticToken::Muted,
                if profile.unicode { "○" } else { "o" }
            ),
            profile.paint(SemanticToken::Focus, &(index + 1).to_string()),
            model.id
        )?;
    }
    let selection = prompt_required(input, output, "Model number or exact id: ")?;
    if let Ok(index) = selection.parse::<usize>()
        && let Some(model) = index.checked_sub(1).and_then(|index| models.get(index))
    {
        return Ok(model.id.clone());
    }
    if models.iter().any(|model| model.id == selection) {
        Ok(selection)
    } else {
        bail!("the selected model was not present in the live catalog")
    }
}

fn choose_reasoning(
    args: &SetupArgs,
    kind: ProviderKind,
    model: &ModelDescriptor,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Option<String>> {
    if kind != ProviderKind::Codex {
        if args.reasoning_effort.is_some() {
            bail!("reasoning effort setup is currently available only for managed Codex");
        }
        return Ok(None);
    }
    let requested = match &args.reasoning_effort {
        Some(value) => Some(value.clone()),
        None if args.non_interactive || model.reasoning_efforts.is_empty() => {
            model.default_reasoning_effort.clone()
        }
        None => {
            writeln!(output, "Reasoning efforts advertised by {}:", model.id)?;
            for effort in &model.reasoning_efforts {
                writeln!(output, "  {}", effort.id)?;
            }
            let default = model.default_reasoning_effort.as_deref().unwrap_or("auto");
            let selected = prompt_default(input, output, "Reasoning effort", default)?;
            (selected != "auto").then_some(selected)
        }
    };
    if let Some(requested) = &requested
        && !model.reasoning_efforts.is_empty()
        && !model
            .reasoning_efforts
            .iter()
            .any(|effort| effort.id == *requested)
    {
        bail!(
            "model {:?} does not advertise reasoning effort {:?}",
            model.id,
            requested
        );
    }
    Ok(requested)
}

fn choose_permission(
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<PermissionMode> {
    if let Some(value) = args.permission_mode {
        return Ok(value.into());
    }
    if args.non_interactive {
        bail!("noninteractive setup requires --permission-mode");
    }
    writeln!(output, "Permission modes: deny, ask, allow")?;
    match prompt_default(input, output, "Permission mode", "ask")?
        .to_ascii_lowercase()
        .as_str()
    {
        "deny" => Ok(PermissionMode::Deny),
        "ask" => Ok(PermissionMode::Ask),
        "allow" => Ok(PermissionMode::Allow),
        _ => bail!("permission mode must be deny, ask, or allow"),
    }
}

pub(super) fn prompt_required(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
) -> Result<String> {
    write!(output, "{label}")?;
    output.flush()?;
    let mut value = String::new();
    if input.read_line(&mut value)? == 0 {
        return Err(SetupCancelled.into());
    }
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("a value is required; no changes made");
    }
    Ok(value)
}

pub(super) fn prompt_default(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: &str,
) -> Result<String> {
    write!(output, "{label} [{default}]: ")?;
    output.flush()?;
    let mut value = String::new();
    if input.read_line(&mut value)? == 0 {
        return Err(SetupCancelled.into());
    }
    let value = value.trim();
    Ok(if value.is_empty() { default } else { value }.to_owned())
}

pub(super) fn confirm(
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
) -> Result<bool> {
    write!(output, "{label}")?;
    output.flush()?;
    let mut value = String::new();
    if input.read_line(&mut value)? == 0 {
        return Ok(false);
    }
    Ok(matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn read_bounded_secret(input: &mut impl BufRead) -> Result<SecretString> {
    let mut value = String::new();
    input
        .take(MAX_SECRET_BYTES + 1)
        .read_to_string(&mut value)
        .context("could not read API key from stdin")?;
    if value.len() as u64 > MAX_SECRET_BYTES {
        bail!("API key input exceeds the {MAX_SECRET_BYTES}-byte limit");
    }
    SecretString::new(value.trim_end_matches(['\r', '\n']).to_owned()).map_err(Into::into)
}

fn install(
    path: &Path,
    rendered: &str,
    credential: Option<(&str, &SecretString)>,
    store: &impl SecretStore,
) -> Result<()> {
    install_with_preferences(path, rendered, credential, store, None, None)
}

fn install_with_preferences(
    path: &Path,
    rendered: &str,
    credential: Option<(&str, &SecretString)>,
    store: &impl SecretStore,
    preference: Option<(PathBuf, &[u8])>,
    selection_path: Option<&Path>,
) -> Result<()> {
    XanaConfig::parse(rendered).context("refusing to install an invalid configuration")?;
    if let Some((_, bytes)) = &preference {
        let input = std::str::from_utf8(bytes).context("presentation preferences are not UTF-8")?;
        crate::presentation::PresentationPreferences::parse(input)
            .context("refusing to install invalid presentation preferences")?;
    }
    let previous_config = read_optional_bounded(path, MAX_CONFIG_BYTES)?;
    let backup = path.with_extension("toml.bak");
    let previous_backup = read_optional_bounded(&backup, MAX_CONFIG_BYTES)?;
    let previous_preference = match &preference {
        Some((path, _)) => read_optional_bounded(path, 32 * 1024)?,
        None => None,
    };
    let previous_selection = match selection_path {
        Some(path) => read_optional_bounded(path, MAX_SELECTION_BYTES)?,
        None => None,
    };
    let previous_secret = match credential {
        Some((id, _)) => Some((id, store.get(id)?)),
        None => None,
    };

    if let Some((id, secret)) = credential {
        store.set(id, secret.expose())?;
    }
    let result = (|| -> Result<()> {
        if let Some(path) = selection_path {
            restore_optional(path, None)?;
        }
        if let Some(previous) = &previous_config {
            atomic_write(&backup, previous)?;
        }
        atomic_write(path, rendered.as_bytes())?;
        if let Some((preference_path, bytes)) = &preference {
            atomic_write(preference_path, bytes)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        if let Some((id, previous)) = previous_secret {
            match previous {
                Some(value) => store.set(id, &value)?,
                None => {
                    store.delete(id)?;
                }
            }
        }
        restore_optional(&backup, previous_backup.as_deref())?;
        if let Some((preference_path, _)) = &preference {
            restore_optional(preference_path, previous_preference.as_deref())?;
        }
        restore_optional(path, previous_config.as_deref())?;
        if let Some(path) = selection_path {
            restore_optional(path, previous_selection.as_deref())?;
        }
        return Err(error).context("setup transaction rolled back");
    }
    Ok(())
}

fn read_optional_bounded(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => {
            bail!("{} exceeds the {limit}-byte setup limit", path.display())
        }
        Ok(_) => fs::read(path)
            .map(Some)
            .with_context(|| format!("could not read {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("configuration path has no usable parent")?;
    fs::create_dir_all(parent)?;
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .with_context(|| format!("could not stage {}", path.display()))?;
    file.write_all(bytes)?;
    file.commit()
        .with_context(|| format!("could not commit {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn restore_optional(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex};
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct FakeStore(Mutex<BTreeMap<String, String>>);

    impl SecretStore for FakeStore {
        fn get(&self, id: &str) -> Result<Option<String>, crate::credential::CredentialError> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }

        fn set(&self, id: &str, secret: &str) -> Result<(), crate::credential::CredentialError> {
            self.0
                .lock()
                .unwrap()
                .insert(id.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, id: &str) -> Result<bool, crate::credential::CredentialError> {
            Ok(self.0.lock().unwrap().remove(id).is_some())
        }
    }

    fn rendered(model: &str) -> String {
        XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Native {
                name: "ollama".into(),
                kind: ProviderKind::Ollama,
                base_url: Some("http://localhost:11434/v1".into()),
                credential: None,
            },
            model: model.into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap()
    }

    #[test]
    fn confirmed_install_preserves_exact_previous_config_as_backup() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = format!("# retained comment\n{}", rendered("old"));
        fs::write(&path, &original).unwrap();

        install(&path, &rendered("new"), None, &FakeStore::default()).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), rendered("new"));
        assert_eq!(
            fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn replacing_connection_reconciles_the_persisted_model_selection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).await.unwrap();
            assert!(
                String::from_utf8_lossy(&request[..read]).starts_with("GET /api/tags HTTP/1.1")
            );
            let body = r#"{"models":[{"name":"qwen3:1.7b"}]}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let directory = tempdir().unwrap();
        let root = directory.path().join("xana-home");
        let paths = XanaPaths::resolve(Some(root.into_os_string())).unwrap();
        fs::create_dir_all(paths.data_dir()).unwrap();
        let prior = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Codex {
                name: "codex".into(),
                program: "codex".into(),
                home: None,
            },
            model: "gpt-5.6-sol".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: Some("medium".into()),
        })
        .unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), prior).unwrap();
        fs::write(
            paths.data_dir().join("selection.toml"),
            "version = 2\nconnection = \"codex\"\nmodel = \"gpt-5.6-sol\"\nreasoning_effort = \"medium\"\nreasoning_summary = \"auto\"\n",
        )
        .unwrap();
        let args = SetupArgs {
            non_interactive: true,
            kind: Some(crate::cli::ConnectionKindChoice::Ollama),
            connection: Some("ollama".into()),
            base_url: Some(format!("http://{address}/v1")),
            model: Some("qwen3:1.7b".into()),
            permission_mode: Some(crate::cli::PermissionChoice::Ask),
            yes: true,
            ..SetupArgs::default()
        };
        let mut input = io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let outcome = run(
            &args,
            &paths,
            false,
            &mut input,
            &mut output,
            ResolvedPresentation::test_plain(),
        )
        .await
        .unwrap();
        assert!(outcome.requires_new_conversation());
        let manager = ModelManager::new(
            XanaConfig::load_registry_from(paths.config_file()).unwrap(),
            paths.cache_dir().to_owned(),
            paths.data_dir().join("selection.toml"),
        );
        assert_eq!(
            manager.selected().unwrap(),
            crate::model_catalog::ModelSelection {
                connection: "ollama".into(),
                model: "qwen3:1.7b".into(),
                reasoning_effort: None,
                reasoning_summary: None,
            }
        );
        server.await.unwrap();
    }

    #[test]
    fn review_labels_never_expose_a_seeded_secret() {
        let secret = SecretString::new("setup-secret-sentinel".into()).unwrap();
        let draft = SetupDraft {
            kind: ProviderKind::OpenAi,
            connection: "openai".into(),
            base_url: None,
            codex_program: None,
            codex_home: None,
            credential: SetupCredential::Stored {
                id: "openai".into(),
            },
            staged_secret: Some(secret),
            model: "model".into(),
            reasoning_effort: None,
            permission_mode: PermissionMode::Ask,
        };
        let rendered = XanaConfig::render_initial(draft.initial_config()).unwrap();
        assert!(!rendered.contains("setup-secret-sentinel"));
        assert!(!draft.credential.review().contains("setup-secret-sentinel"));
    }

    #[test]
    fn invalid_config_is_rejected_before_credential_mutation() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let store = FakeStore::default();
        store.set("openai", "previous").unwrap();
        let secret = SecretString::new("replacement".into()).unwrap();

        assert!(install(&path, "version = [", Some(("openai", &secret)), &store).is_err());
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("previous"));
        assert!(!path.exists());
    }

    #[test]
    fn provider_choice_has_no_implicit_default() {
        let args = SetupArgs {
            non_interactive: false,
            kind: None,
            connection: None,
            base_url: None,
            codex_program: None,
            codex_home: None,
            credential_env: None,
            key_from_stdin: false,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            yes: false,
            dry_run: false,
            ..SetupArgs::default()
        };
        let mut input = io::Cursor::new("\n");
        let mut output = Vec::new();
        assert!(
            choose_kind(
                &args,
                &mut input,
                &mut output,
                ResolvedPresentation::test_plain(),
            )
            .is_err()
        );
        let transcript = String::from_utf8(output).unwrap();
        assert!(!transcript.to_ascii_lowercase().contains("recommended"));
    }

    #[test]
    fn bare_interactive_setup_asks_for_a_path_and_defaults_to_quick() {
        let args = SetupArgs::default();
        let mut input = io::Cursor::new("\n");
        let mut output = Vec::new();

        let selected = choose_setup_path(
            &args,
            &mut input,
            &mut output,
            ResolvedPresentation::test_plain(),
        )
        .unwrap()
        .unwrap();

        assert!(selected.quick);
        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("Choose a guided path"));
        assert!(transcript.contains("Quick Setup"));
        assert!(transcript.contains("Appearance"));
    }

    #[test]
    fn completion_receipt_names_owned_paths_and_source_checkout_commands() {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
        let mut output = Vec::new();

        write_completion_receipt(&mut output, &paths, ResolvedPresentation::test_plain()).unwrap();

        let transcript = String::from_utf8(output).unwrap();
        assert!(transcript.contains("[OK] Setup Complete!"));
        assert!(transcript.contains(&paths.config_file().display().to_string()));
        assert!(transcript.contains(&paths.data_dir().display().to_string()));
        assert!(transcript.contains("operating-system credential store"));
        assert!(transcript.contains("cargo run -- doctor"));
        assert!(!transcript.contains(".env"));
    }

    #[test]
    fn start_new_requires_both_a_request_and_new_conversation_commit() {
        assert!(
            SetupOutcome::Committed {
                requires_new_conversation: true
            }
            .starts_new_conversation(true)
        );
        assert!(
            !SetupOutcome::Committed {
                requires_new_conversation: false
            }
            .starts_new_conversation(true)
        );
        assert!(
            !SetupOutcome::Committed {
                requires_new_conversation: true
            }
            .starts_new_conversation(false)
        );
        assert!(!SetupOutcome::Unchanged.starts_new_conversation(true));
    }
}
