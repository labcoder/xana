//! Advanced and sectional setup over the same validated transaction boundary.

mod appearance;

use super::{SetupOutcome, atomic_write, confirm, prompt_default};
use crate::{
    cli::{SetupArgs, SetupSectionChoice},
    config::XanaConfig,
    credential::OsSecretStore,
    paths::XanaPaths,
    presentation::PresentationPreferences,
};
use anyhow::{Context, Result, bail};
use std::{fs, io::BufRead, io::Write};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

pub(super) struct Customization {
    pub(super) config: String,
    pub(super) preferences: Option<String>,
    pub(super) effects: Vec<&'static str>,
}

pub(super) fn customize_quick(
    rendered: String,
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<Customization> {
    let rendered = merge_existing_connection_if_valid(paths, rendered)?;
    if args.section == Some(SetupSectionChoice::Connection) {
        return Ok(Customization {
            config: rendered,
            preferences: None,
            effects: vec!["new conversation (connection/model owner changed)"],
        });
    }
    if !args.full {
        return Ok(Customization {
            config: rendered,
            preferences: None,
            effects: vec!["new conversation (selected connection/model changed)"],
        });
    }

    writeln!(output)?;
    writeln!(output, "Full Custom Setup")?;
    let mut document = rendered.parse::<DocumentMut>()?;
    edit_permissions_shell(&mut document, args, input, output, true, true)?;
    edit_profiles_routes(&mut document, args, input, output, true)?;
    let preferences = appearance::edit(args, paths, input, output, true)?;
    let config = validate_document(document)?;
    Ok(Customization {
        config,
        preferences: Some(preferences.render()?),
        effects: vec![
            "appearance immediately after commit",
            "managed model/reasoning on subsequent turns",
            "connection, shell, authority, profile, and route changes in a new conversation",
        ],
    })
}

pub(super) fn run_section(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<SetupOutcome> {
    let section = args.section.context("missing setup section")?;
    if section == SetupSectionChoice::Appearance {
        if args.non_interactive && appearance::flags_empty(args) {
            bail!("noninteractive appearance setup requires at least one appearance option");
        }
        let preferences = appearance::edit(args, paths, input, output, !args.non_interactive)?;
        let rendered = preferences.render()?;
        PresentationPreferences::parse(&rendered)?;
        writeln!(output, "Review")?;
        writeln!(output, "  Section: appearance")?;
        writeln!(
            output,
            "  Applies: immediately; runtime policy is unchanged"
        )?;
        if args.dry_run {
            writeln!(output, "Validated preview only; no durable state changed.")?;
            return Ok(SetupOutcome::Unchanged);
        }
        if !args.yes && !confirm(input, output, "Apply appearance changes? [y/N]: ")? {
            writeln!(output, "No changes made.")?;
            return Ok(SetupOutcome::Unchanged);
        }
        atomic_write(&paths.presentation_file(), rendered.as_bytes())?;
        writeln!(output, "Appearance preferences applied immediately.")?;
        return Ok(SetupOutcome::Committed {
            requires_new_conversation: false,
        });
    }

    let source = fs::read_to_string(paths.config_file()).with_context(|| {
        format!(
            "sectional setup requires an existing config at {}",
            paths.config_file().display()
        )
    })?;
    if args.non_interactive {
        match section {
            SetupSectionChoice::PermissionsShell
                if args.permission_mode.is_none()
                    && args.shell.is_none()
                    && args.permission_rule.is_empty() =>
            {
                bail!(
                    "noninteractive permissions-shell setup requires --permission-mode, --shell, or --permission-rule"
                )
            }
            SetupSectionChoice::ProfilesRoutes
                if args.capabilities.is_none()
                    && args.profile_connection.is_none()
                    && args.profile_model.is_none()
                    && args.route_profile.is_none()
                    && args.max_fan_out.is_none()
                    && args.max_descendants.is_none()
                    && args.max_concurrency.is_none()
                    && args.deadline_seconds.is_none()
                    && args.max_context_tokens.is_none()
                    && args.max_report_bytes.is_none()
                    && args.max_artifact_bytes.is_none() =>
            {
                bail!(
                    "noninteractive profiles-routes setup requires a profile, route, capability, or orchestration change"
                )
            }
            _ => {}
        }
    }
    let mut document = source.parse::<DocumentMut>()?;
    let effect = match section {
        SetupSectionChoice::PermissionsShell => {
            edit_permissions_shell(
                &mut document,
                args,
                input,
                output,
                !args.non_interactive,
                false,
            )?;
            "new conversation (shell or authority snapshot changed)"
        }
        SetupSectionChoice::ProfilesRoutes => {
            edit_profiles_routes(&mut document, args, input, output, !args.non_interactive)?;
            "new conversation (profile/route snapshot changed)"
        }
        SetupSectionChoice::Connection | SetupSectionChoice::Appearance => unreachable!(),
    };
    let rendered = validate_document(document)?;
    writeln!(output, "Review")?;
    writeln!(output, "  Section: {}", section_name(section))?;
    writeln!(output, "  Applies: {effect}")?;
    writeln!(output, "  Existing conversation: unchanged")?;
    if args.start_new {
        writeln!(output, "  Next action: start a new conversation now")?;
    }
    if args.dry_run {
        writeln!(output, "Validated preview only; no durable state changed.")?;
        return Ok(SetupOutcome::Unchanged);
    }
    if !args.yes && !confirm(input, output, "Apply this section? [y/N]: ")? {
        writeln!(output, "No changes made.")?;
        return Ok(SetupOutcome::Unchanged);
    }
    super::install(paths.config_file(), &rendered, None, &OsSecretStore)?;
    writeln!(
        output,
        "Section committed atomically; current conversation was not mutated."
    )?;
    if args.start_new {
        writeln!(
            output,
            "Starting a new conversation; no active thread was discarded."
        )?;
    }
    Ok(SetupOutcome::Committed {
        requires_new_conversation: true,
    })
}

fn merge_connection(existing: &str, replacement: &str) -> Result<String> {
    let mut current = existing.parse::<DocumentMut>()?;
    let replacement = replacement.parse::<DocumentMut>()?;
    let new_provider = replacement["providers"]
        .as_table()
        .and_then(|table| table.iter().next())
        .map(|(name, item)| (name.to_owned(), item.clone()))
        .context("replacement setup has no provider")?;
    let providers = current["providers"]
        .as_table_mut()
        .context("providers must be a table")?;
    providers.insert(&new_provider.0, new_provider.1);

    let replacement_default = replacement["default_profile"]
        .as_str()
        .context("replacement setup has no default profile name")?;
    let current_default = current["default_profile"]
        .as_str()
        .context("existing config has no default profile name")?
        .to_owned();
    let new_profile = replacement["profiles"][replacement_default]
        .as_table()
        .context("replacement setup has no default profile")?;
    let profile = current["profiles"][&current_default]
        .as_table_mut()
        .context("existing config has no default profile")?;
    for key in [
        "connection",
        "model",
        "reasoning_effort",
        "reasoning_summary",
    ] {
        if let Some(item) = new_profile.get(key) {
            profile.insert(key, item.clone());
        } else {
            profile.remove(key);
        }
    }
    validate_document(current)
}

fn merge_existing_connection_if_valid(paths: &XanaPaths, replacement: String) -> Result<String> {
    if !paths.config_file().is_file() {
        return Ok(replacement);
    }
    let existing = fs::read_to_string(paths.config_file())?;
    if XanaConfig::parse(&existing).is_err() {
        return Ok(replacement);
    }
    merge_connection(&existing, &replacement)
}

fn edit_permissions_shell(
    document: &mut DocumentMut,
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    full: bool,
    preserve_permission: bool,
) -> Result<()> {
    let permission = match args.permission_mode {
        Some(value) => permission_name(value).to_owned(),
        None if preserve_permission || (args.non_interactive && !full) => {
            document["permission_mode"]
                .as_str()
                .context("permission_mode must be a string")?
                .to_owned()
        }
        None => {
            let current = document["permission_mode"].as_str().unwrap_or("ask");
            prompt_default(input, output, "Permission mode", current)?
        }
    };
    document["permission_mode"] = value(permission);

    let shell = match args.shell {
        Some(value) => shell_name(value).to_owned(),
        None if args.non_interactive && !full => document["shell"]["kind"]
            .as_str()
            .context("shell.kind must be a string")?
            .to_owned(),
        None => {
            let current = document["shell"]["kind"].as_str().unwrap_or("platform");
            prompt_default(input, output, "Shell", current)?
        }
    };
    document["shell"]["kind"] = value(shell);
    if let Some(program) = &args.shell_program {
        document["shell"]["program"] = value(program.to_string_lossy().into_owned());
    }

    if !args.permission_rule.is_empty() {
        let rules = permission_rules_mut(document)?;
        for specification in &args.permission_rule {
            rules.push(parse_permission_rule(specification)?);
        }
    } else if full && !args.non_interactive {
        let specification = prompt_default(
            input,
            output,
            "Permission rule ID:DECISION:EFFECT[:WORKSPACE] (blank for none)",
            "",
        )?;
        if !specification.is_empty() {
            permission_rules_mut(document)?.push(parse_permission_rule(&specification)?);
        }
    }
    Ok(())
}

fn permission_rules_mut(document: &mut DocumentMut) -> Result<&mut ArrayOfTables> {
    let item = document
        .entry("permission_rules")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    if item.as_array().is_some_and(Array::is_empty) {
        *item = Item::ArrayOfTables(ArrayOfTables::new());
    }
    item.as_array_of_tables_mut()
        .context("permission_rules must be an array of tables")
}

fn parse_permission_rule(specification: &str) -> Result<Table> {
    let parts = specification.splitn(4, ':').collect::<Vec<_>>();
    if !(3..=4).contains(&parts.len()) {
        bail!("permission rule must be ID:DECISION:EFFECT[:WORKSPACE]");
    }
    if !matches!(parts[1], "deny" | "ask" | "allow") {
        bail!("permission rule decision must be deny, ask, or allow");
    }
    if !matches!(
        parts[2],
        "read" | "write" | "execute" | "network" | "external"
    ) {
        bail!("permission rule effect is not supported");
    }
    let mut table = Table::new();
    table["id"] = value(parts[0]);
    table["decision"] = value(parts[1]);
    table["effect"] = value(parts[2]);
    if let Some(workspace) = parts.get(3) {
        table["workspace"] = value(*workspace);
    }
    Ok(table)
}

fn edit_profiles_routes(
    document: &mut DocumentMut,
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    full: bool,
) -> Result<()> {
    let profile_name = match &args.profile {
        Some(profile) => profile.clone(),
        None if full && !args.non_interactive => {
            prompt_default(input, output, "Profile name", "default")?
        }
        None => "default".to_owned(),
    };
    let profiles = document["profiles"]
        .as_table_mut()
        .context("profiles must be a table")?;
    if !profiles.contains_key(&profile_name) {
        profiles.insert(&profile_name, Item::Table(Table::new()));
    }
    let profile = profiles[&profile_name]
        .as_table_mut()
        .context("profile must be a table")?;
    if let Some(connection) = &args.profile_connection {
        profile["connection"] = value(connection.clone());
    }
    if let Some(model) = &args.profile_model {
        profile["model"] = value(model.clone());
    }
    if let Some(capabilities) = &args.capabilities {
        let mut values = Array::new();
        for capability in capabilities
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            values.push(capability);
        }
        profile["capabilities"] = Item::Value(values.into());
    } else if full && !args.non_interactive {
        let current = profile
            .get("capabilities")
            .and_then(Item::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let selected = prompt_default(
            input,
            output,
            "Capabilities (comma separated; empty means defaults)",
            &current,
        )?;
        if !selected.is_empty() {
            let mut values = Array::new();
            for capability in selected
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                values.push(capability);
            }
            profile["capabilities"] = Item::Value(values.into());
        }
    }

    let orchestration = profile
        .entry("orchestration")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .context("profile orchestration must be a table")?;
    let current_max_fan_out = table_usize(orchestration, "max_fan_out", 4);
    let current_max_descendants = table_usize(orchestration, "max_descendants", 8);
    let current_max_concurrency = table_usize(orchestration, "max_concurrency", 2);
    let current_deadline_seconds = table_u64(orchestration, "deadline_seconds", 300);
    let current_max_context_tokens = table_usize(orchestration, "max_context_tokens", 8_192);
    let current_max_report_bytes = table_usize(orchestration, "max_report_bytes", 32 * 1024);
    let current_max_artifact_bytes =
        table_usize(orchestration, "max_artifact_bytes", 8 * 1024 * 1024);
    set_usize(
        orchestration,
        "max_fan_out",
        full_usize(
            args.max_fan_out,
            full,
            args,
            input,
            output,
            "Max fan-out",
            current_max_fan_out,
        )?,
    );
    set_usize(
        orchestration,
        "max_descendants",
        full_usize(
            args.max_descendants,
            full,
            args,
            input,
            output,
            "Max descendants",
            current_max_descendants,
        )?,
    );
    set_usize(
        orchestration,
        "max_concurrency",
        full_usize(
            args.max_concurrency,
            full,
            args,
            input,
            output,
            "Max child concurrency",
            current_max_concurrency,
        )?,
    );
    set_u64(
        orchestration,
        "deadline_seconds",
        full_u64(
            args.deadline_seconds,
            full,
            args,
            input,
            output,
            "Child deadline seconds",
            current_deadline_seconds,
        )?,
    );
    set_usize(
        orchestration,
        "max_context_tokens",
        full_usize(
            args.max_context_tokens,
            full,
            args,
            input,
            output,
            "Child context tokens",
            current_max_context_tokens,
        )?,
    );
    set_usize(
        orchestration,
        "max_report_bytes",
        full_usize(
            args.max_report_bytes,
            full,
            args,
            input,
            output,
            "Child report bytes",
            current_max_report_bytes,
        )?,
    );
    set_usize(
        orchestration,
        "max_artifact_bytes",
        full_usize(
            args.max_artifact_bytes,
            full,
            args,
            input,
            output,
            "Child artifact bytes",
            current_max_artifact_bytes,
        )?,
    );

    let route_name = match &args.route {
        Some(route) => route.clone(),
        None if full && !args.non_interactive => {
            prompt_default(input, output, "Task route name", "default")?
        }
        None => "default".to_owned(),
    };
    let route_profile = args
        .route_profile
        .clone()
        .unwrap_or_else(|| profile_name.clone());
    let routes = document["routes"]
        .as_table_mut()
        .context("routes must be a table")?;
    if !routes.contains_key(&route_name) {
        routes.insert(&route_name, Item::Table(Table::new()));
    }
    routes[&route_name]["profile"] = value(route_profile);
    Ok(())
}

fn validate_document(document: DocumentMut) -> Result<String> {
    let rendered = document.to_string();
    XanaConfig::parse(&rendered).context("advanced setup produced an invalid configuration")?;
    Ok(rendered)
}

fn set_usize(table: &mut Table, key: &str, selected: Option<usize>) {
    if let Some(selected) = selected {
        table[key] = value(i64::try_from(selected).unwrap_or(i64::MAX));
    }
}

fn set_u64(table: &mut Table, key: &str, selected: Option<u64>) {
    if let Some(selected) = selected {
        table[key] = value(i64::try_from(selected).unwrap_or(i64::MAX));
    }
}

fn table_usize(table: &Table, key: &str, default: usize) -> usize {
    table
        .get(key)
        .and_then(Item::as_integer)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn table_u64(table: &Table, key: &str, default: u64) -> u64 {
    table
        .get(key)
        .and_then(Item::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(default)
}

#[allow(clippy::too_many_arguments)]
fn full_usize(
    selected: Option<usize>,
    full: bool,
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: usize,
) -> Result<Option<usize>> {
    if selected.is_some() || !full || args.non_interactive {
        return Ok(selected);
    }
    prompt_default(input, output, label, &default.to_string())?
        .parse()
        .map(Some)
        .with_context(|| format!("{label} must be a nonnegative whole number"))
}

#[allow(clippy::too_many_arguments)]
fn full_u64(
    selected: Option<u64>,
    full: bool,
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    label: &str,
    default: u64,
) -> Result<Option<u64>> {
    if selected.is_some() || !full || args.non_interactive {
        return Ok(selected);
    }
    prompt_default(input, output, label, &default.to_string())?
        .parse()
        .map(Some)
        .with_context(|| format!("{label} must be a nonnegative whole number"))
}

fn permission_name(value: crate::cli::PermissionChoice) -> &'static str {
    match value {
        crate::cli::PermissionChoice::Deny => "deny",
        crate::cli::PermissionChoice::Ask => "ask",
        crate::cli::PermissionChoice::Allow => "allow",
    }
}

fn shell_name(value: crate::cli::ShellChoice) -> &'static str {
    match value {
        crate::cli::ShellChoice::Platform => "platform",
        crate::cli::ShellChoice::Posix => "posix",
        crate::cli::ShellChoice::GitBash => "git_bash",
        crate::cli::ShellChoice::PowerShell => "powershell",
        crate::cli::ShellChoice::Cmd => "cmd",
    }
}

fn section_name(section: SetupSectionChoice) -> &'static str {
    match section {
        SetupSectionChoice::Connection => "connection/model",
        SetupSectionChoice::PermissionsShell => "permissions/shell",
        SetupSectionChoice::ProfilesRoutes => "profiles/routes",
        SetupSectionChoice::Appearance => "appearance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ThemeChoice;
    use crate::config::{InitialConfig, InitialConnection, PermissionMode, ProviderKind};
    use crate::presentation::ThemeChoice as PresentationTheme;
    use crate::shell::ShellConfig;
    use tempfile::tempdir;

    fn base() -> String {
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Native {
                name: "ollama".into(),
                kind: ProviderKind::Ollama,
                base_url: Some("http://localhost:11434/v1".into()),
                credential: None,
            },
            model: "old".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        format!("# keep me\n{rendered}")
    }

    #[test]
    fn connection_section_preserves_comments_and_unrelated_policy() {
        let mut existing = base().parse::<DocumentMut>().unwrap();
        existing["permission_mode"] = value("deny");
        let existing = format!("# retained\n{existing}");
        let replacement = base().replace("old", "new");
        let merged = merge_connection(&existing, &replacement).unwrap();
        assert!(merged.starts_with("# retained"));
        assert!(merged.contains("permission_mode = \"deny\""));
        assert!(merged.contains("model = \"new\""));
    }

    #[test]
    fn connection_merge_updates_the_existing_named_default_profile() {
        let existing = base()
            .replace(
                "default_profile = \"default\"",
                "default_profile = \"main\"",
            )
            .replace("[profiles.default", "[profiles.main")
            .replace("profile = \"default\"", "profile = \"main\"");
        let replacement = base().replace("old", "new");

        let merged = merge_connection(&existing, &replacement).unwrap();

        assert!(merged.contains("default_profile = \"main\""));
        assert!(merged.contains("[profiles.main]"));
        assert!(merged.contains("model = \"new\""));
    }

    #[test]
    fn quick_reconfiguration_preserves_existing_connections() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), base()).unwrap();
        let replacement = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Native {
                name: "remote".into(),
                kind: crate::config::ProviderKind::OpenAiCompat,
                base_url: Some("http://localhost:1234/v1".into()),
                credential: None,
            },
            model: "remote-model".into(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();

        let customization = customize_quick(
            replacement,
            &SetupArgs::default(),
            &paths,
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            &mut Vec::new(),
        )
        .unwrap();

        assert!(customization.config.contains("[providers.ollama]"));
        assert!(customization.config.contains("[providers.remote]"));
        assert!(customization.config.contains("connection = \"remote\""));
    }

    #[test]
    fn reusable_setup_base_preserves_valid_connections_but_can_repair_invalid_config() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        let replacement = base().replace("old", "new");

        fs::write(paths.config_file(), base()).unwrap();
        let merged = merge_existing_connection_if_valid(&paths, replacement.clone()).unwrap();
        assert!(merged.contains("# keep me"));
        assert!(merged.contains("model = \"new\""));

        fs::write(paths.config_file(), "not valid TOML = [").unwrap();
        assert_eq!(
            merge_existing_connection_if_valid(&paths, replacement.clone()).unwrap(),
            replacement
        );
    }

    #[test]
    fn permission_rule_parser_is_bounded_and_typed_by_real_validation() {
        let rule = parse_permission_rule("reads:allow:read:.").unwrap();
        assert_eq!(rule["decision"].as_str(), Some("allow"));
        assert!(parse_permission_rule("bad:maybe:read").is_err());
        assert!(parse_permission_rule("bad:allow:unknown").is_err());
    }

    #[test]
    fn permission_section_preserves_comments_and_unrelated_profile_fields() {
        let mut document = base().parse::<DocumentMut>().unwrap();
        let args = SetupArgs {
            non_interactive: true,
            permission_mode: Some(crate::cli::PermissionChoice::Deny),
            shell: Some(crate::cli::ShellChoice::Platform),
            permission_rule: vec!["reads:allow:read:.".into()],
            ..SetupArgs::default()
        };
        edit_permissions_shell(
            &mut document,
            &args,
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            &mut Vec::new(),
            false,
            false,
        )
        .unwrap();
        let rendered = validate_document(document).unwrap();
        assert!(rendered.starts_with("# keep me"));
        assert!(rendered.contains("model = \"old\""));
        assert!(rendered.contains("permission_mode = \"deny\""));
        assert!(rendered.contains("id = \"reads\""));
    }

    #[test]
    fn invalid_profile_authority_fails_the_real_config_validator_before_commit() {
        let mut document = base().parse::<DocumentMut>().unwrap();
        let args = SetupArgs {
            non_interactive: true,
            profile: Some("child".into()),
            profile_connection: Some("missing".into()),
            profile_model: Some("model".into()),
            route: Some("child".into()),
            route_profile: Some("child".into()),
            ..SetupArgs::default()
        };
        edit_profiles_routes(
            &mut document,
            &args,
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            &mut Vec::new(),
            false,
        )
        .unwrap();
        assert!(validate_document(document).is_err());
    }

    #[test]
    fn appearance_section_changes_only_machine_local_presentation() {
        let directory = tempdir().unwrap();
        let paths = XanaPaths::resolve(Some(directory.path().as_os_str().to_owned())).unwrap();
        fs::write(paths.config_file(), base()).unwrap();
        let original = fs::read(paths.config_file()).unwrap();
        let args = SetupArgs {
            non_interactive: true,
            section: Some(SetupSectionChoice::Appearance),
            theme: Some(ThemeChoice::Monochrome),
            yes: true,
            ..SetupArgs::default()
        };
        let outcome = run_section(
            &args,
            &paths,
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            outcome,
            SetupOutcome::Committed {
                requires_new_conversation: false
            }
        );
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        let preferences = PresentationPreferences::load(&paths.presentation_file()).preferences;
        assert_eq!(preferences.theme, PresentationTheme::Monochrome);
    }
}
