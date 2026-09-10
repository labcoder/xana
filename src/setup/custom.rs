//! Advanced and sectional setup over the same validated transaction boundary.

mod appearance;

use super::{
    SetupBack, SetupOutcome, atomic_write, prompt_default,
    ui::{SelectOption, SetupUi},
};
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
    ui: SetupUi,
) -> Result<Customization> {
    let mut args = args.clone();
    let current_default = XanaConfig::load_registry_from(paths.config_file())
        .ok()
        .map(|registry| registry.default_profile)
        .unwrap_or_else(|| "default".into());
    if args.full && args.profile.is_none() && !args.non_interactive {
        args.profile = Some(loop {
            let name = super::ui::prompt_value(
                input,
                output,
                ui,
                "Profile name",
                &current_default,
                true,
                false,
            )?;
            match crate::config::profiles::validate_profile_name(&name) {
                Ok(()) => break name,
                Err(error) => writeln!(output, "{error}; choose a profile name again.")?,
            }
        });
    }
    let mut rendered = merge_setup_profile(paths, rendered, args.profile.as_deref())?;
    if let Some(name) = args.profile.as_deref() {
        let mut document = rendered.parse::<DocumentMut>()?;
        let already_default = document["default_profile"].as_str() == Some(name);
        let make_default = args.make_default
            || (!already_default
                && !args.non_interactive
                && super::ui::confirm_review(
                    input,
                    output,
                    ui,
                    "Default profile",
                    &[format!(
                        "Use {name:?} for new conversations? Existing conversations keep their profile."
                    )],
                )?);
        if make_default {
            document["default_profile"] = value(name);
        }
        rendered = validate_document(document)?;
    }
    let args = &args;
    if args.section == Some(SetupSectionChoice::Connection) {
        return Ok(Customization {
            config: rendered,
            preferences: None,
            effects: vec!["next turn in this conversation (compatible execution owner required)"],
        });
    }
    if !args.full {
        return Ok(Customization {
            config: rendered,
            preferences: None,
            effects: vec!["next turn in this conversation (compatible execution owner required)"],
        });
    }

    if !ui.rich {
        writeln!(output)?;
        writeln!(output, "Full Custom Setup")?;
    }
    let mut document = rendered.parse::<DocumentMut>()?;
    edit_permissions_shell(&mut document, args, input, output, true, true, ui)?;
    edit_profiles_routes(&mut document, args, input, output, true, ui)?;
    let config = validate_document(document)?;
    let preferences = appearance::edit(args, paths, input, output, true, ui)?;
    Ok(Customization {
        config,
        preferences: Some(preferences.render()?),
        effects: vec![
            "appearance immediately after commit",
            "managed model/reasoning on subsequent turns",
            "execution settings before the next new turn; existing history is retained",
        ],
    })
}

pub(super) fn run_section(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
) -> Result<SetupOutcome> {
    let section = args.section.context("missing setup section")?;
    if section == SetupSectionChoice::Appearance {
        if args.non_interactive && appearance::flags_empty(args) {
            bail!("noninteractive appearance setup requires at least one appearance option");
        }
        let preferences = appearance::edit(args, paths, input, output, !args.non_interactive, ui)?;
        let review_ui = super::ui::preview_preferences(ui, &preferences);
        let rendered = preferences.render()?;
        PresentationPreferences::parse(&rendered)?;
        let review = vec![
            "Section      appearance".to_owned(),
            format!("Theme        {:?}", preferences.theme),
            format!("Glyphs       {:?}", preferences.glyphs),
            format!("Motion       {:?}", preferences.motion),
            format!("Density      {:?}", preferences.density),
            format!("Composer     {:?}", preferences.composer),
            format!("Activity     {:?}", preferences.activity),
            "Applies      immediately; runtime policy is unchanged".to_owned(),
        ];
        if !ui.rich {
            writeln!(output, "Review")?;
            for line in &review {
                writeln!(output, "  {line}")?;
            }
        }
        if args.dry_run {
            writeln!(output, "Validated preview only; no durable state changed.")?;
            return Ok(SetupOutcome::Unchanged);
        }
        if !args.yes
            && !super::ui::confirm_review(input, output, review_ui, "Review appearance", &review)?
        {
            writeln!(output, "No changes made.")?;
            return Ok(SetupOutcome::Unchanged);
        }
        atomic_write(&paths.presentation_file(), rendered.as_bytes())?;
        writeln!(output, "Appearance preferences applied immediately.")?;
        return Ok(SetupOutcome::Committed {
            execution_changed: false,
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
                    && args.profile.is_none()
                    && !args.make_default
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
                ui,
            )?;
            "next new turn (shell or authority settings changed)"
        }
        SetupSectionChoice::ProfilesRoutes => {
            edit_profiles_routes(
                &mut document,
                args,
                input,
                output,
                !args.non_interactive,
                ui,
            )?;
            "next new turn (profile/route settings changed)"
        }
        SetupSectionChoice::Connection
        | SetupSectionChoice::Appearance
        | SetupSectionChoice::Storage => unreachable!(),
    };
    let rendered = validate_document(document)?;
    let mut review = vec![
        format!("Section      {}", section_name(section)),
        format!("Applies      {effect}"),
        "Conversation current one remains unchanged".to_owned(),
    ];
    if args.start_new {
        review.push("Next action  start a new conversation now".to_owned());
    }
    if !ui.rich {
        writeln!(output, "Review")?;
        for line in &review {
            writeln!(output, "  {line}")?;
        }
    }
    if args.dry_run {
        writeln!(output, "Validated preview only; no durable state changed.")?;
        return Ok(SetupOutcome::Unchanged);
    }
    if !args.yes && !super::ui::confirm_review(input, output, ui, "Review setup section", &review)?
    {
        writeln!(output, "No changes made.")?;
        return Ok(SetupOutcome::Unchanged);
    }
    super::install_with_preferences(
        paths.config_file(),
        &rendered,
        None,
        &OsSecretStore,
        None,
        None,
        Some(super::ConfigRevision(Some(blake3::hash(source.as_bytes())))),
    )?;
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
        execution_changed: true,
    })
}

#[cfg(test)]
fn merge_connection(existing: &str, replacement: &str) -> Result<String> {
    merge_named_connection(existing, replacement, None)
}

fn merge_named_connection(existing: &str, replacement: &str, name: Option<&str>) -> Result<String> {
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
    let target = name.unwrap_or(&current_default);
    crate::config::profiles::ensure_profile(&mut current, target)?;
    let profile = current["profiles"][target]
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

#[cfg(test)]
pub(super) fn merge_existing_connection_if_valid(
    paths: &XanaPaths,
    replacement: String,
) -> Result<String> {
    merge_setup_profile(paths, replacement, None)
}

pub(super) fn merge_setup_profile(
    paths: &XanaPaths,
    replacement: String,
    name: Option<&str>,
) -> Result<String> {
    if !paths.config_file().is_file() {
        return name.map_or(Ok(replacement.clone()), |name| {
            Ok(crate::config::profiles::name_initial_profile(
                &replacement,
                name,
            )?)
        });
    }
    let existing = fs::read_to_string(paths.config_file())?;
    if XanaConfig::parse(&existing).is_err() {
        return name.map_or(Ok(replacement.clone()), |name| {
            Ok(crate::config::profiles::name_initial_profile(
                &replacement,
                name,
            )?)
        });
    }
    merge_named_connection(&existing, &replacement, name)
}

fn edit_permissions_shell(
    document: &mut DocumentMut,
    args: &SetupArgs,
    input: &mut impl BufRead,
    output: &mut impl Write,
    full: bool,
    preserve_permission: bool,
    ui: SetupUi,
) -> Result<()> {
    let permission = match args.permission_mode {
        Some(value) => permission_name(value).to_owned(),
        None if preserve_permission || (args.non_interactive && !full) => {
            document["permission_mode"]
                .as_str()
                .context("permission_mode must be a string")?
                .to_owned()
        }
        None if ui.rich => {
            let options = [
                SelectOption::new("ask", "prompt before effectful tools; recommended default"),
                SelectOption::new(
                    "deny",
                    "block effectful tools unless a narrower rule allows them",
                ),
                SelectOption::new(
                    "allow",
                    "allow effectful tools under ordinary host permissions",
                ),
            ];
            let current = document["permission_mode"].as_str().unwrap_or("ask");
            let default = options
                .iter()
                .position(|option| option.label == current)
                .unwrap_or(0);
            super::ui::select(
                output,
                ui,
                "Choose the default permission mode",
                &options,
                default,
            )?
            .map(|index| options[index].label.clone())
            .ok_or(SetupBack)?
        }
        None => {
            writeln!(output)?;
            writeln!(output, "Permission modes:")?;
            writeln!(
                output,
                "  ask    prompt before effectful tools (recommended)"
            )?;
            writeln!(
                output,
                "  deny   block effectful tools unless a rule allows them"
            )?;
            writeln!(
                output,
                "  allow  allow tools under ordinary host permissions"
            )?;
            let current = document["permission_mode"].as_str().unwrap_or("ask");
            prompt_default(input, output, "Permission mode", current)?
        }
    };
    document["permission_mode"] = value(permission);

    let shell = match args.shell {
        Some(value) => shell_name(value).to_owned(),
        None if args.non_interactive => document["shell"]["kind"]
            .as_str()
            .context("shell.kind must be a string")?
            .to_owned(),
        None if ui.rich => {
            let options = [
                SelectOption::new("platform", "use the operating system's native default"),
                SelectOption::new("posix", "use a POSIX-compatible sh shell"),
                SelectOption::new("git_bash", "use Git for Windows Bash"),
                SelectOption::new("powershell", "use PowerShell / pwsh"),
                SelectOption::new("cmd", "use Windows Command Prompt"),
            ];
            let current = document["shell"]["kind"].as_str().unwrap_or("platform");
            let default = options
                .iter()
                .position(|option| option.label == current)
                .unwrap_or(0);
            super::ui::select(output, ui, "Choose the command shell", &options, default)?
                .map(|index| options[index].label.clone())
                .ok_or(SetupBack)?
        }
        None => {
            writeln!(output)?;
            writeln!(output, "Shells:")?;
            writeln!(output, "  platform    operating-system default")?;
            writeln!(output, "  posix       POSIX-compatible sh")?;
            writeln!(output, "  git_bash    Git for Windows Bash")?;
            writeln!(output, "  powershell  PowerShell / pwsh")?;
            writeln!(output, "  cmd         Windows Command Prompt")?;
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
        let specification = super::ui::prompt_value(
            input,
            output,
            ui,
            "Permission rule ID:DECISION:EFFECT[:WORKSPACE] (blank for none)",
            "",
            false,
            false,
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
    ui: SetupUi,
) -> Result<()> {
    let default_profile = document["default_profile"]
        .as_str()
        .context("missing default profile")?
        .to_owned();
    let profile_name = match &args.profile {
        Some(profile) => profile.clone(),
        None if full && !args.non_interactive => super::ui::prompt_value(
            input,
            output,
            ui,
            "Profile name",
            &default_profile,
            true,
            false,
        )?,
        None => default_profile.clone(),
    };
    crate::config::profiles::ensure_profile(document, &profile_name)?;
    let profiles = document["profiles"]
        .as_table_mut()
        .context("profiles must be a table")?;
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
        let selected = super::ui::prompt_value(
            input,
            output,
            ui,
            "Capabilities (comma separated; empty means defaults)",
            &current,
            false,
            false,
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
            ui,
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
            ui,
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
            ui,
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
            ui,
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
            ui,
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
            ui,
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
            ui,
        )?,
    );

    let default_route = if profile_name == default_profile {
        document
            .get("default_child_route")
            .and_then(Item::as_str)
            .unwrap_or(&profile_name)
            .to_owned()
    } else {
        profile_name.clone()
    };
    let route_name = match &args.route {
        Some(route) => route.clone(),
        None if full && !args.non_interactive => super::ui::prompt_value(
            input,
            output,
            ui,
            "Task route name",
            &default_route,
            true,
            false,
        )?,
        None => default_route,
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
    if args.make_default {
        document["default_profile"] = value(profile_name);
    }
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
    ui: SetupUi,
) -> Result<Option<usize>> {
    if selected.is_some() || !full || args.non_interactive {
        return Ok(selected);
    }
    super::ui::prompt_value(input, output, ui, label, &default.to_string(), true, false)?
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
    ui: SetupUi,
) -> Result<Option<u64>> {
    if selected.is_some() || !full || args.non_interactive {
        return Ok(selected);
    }
    super::ui::prompt_value(input, output, ui, label, &default.to_string(), true, false)?
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
        SetupSectionChoice::Storage => "storage",
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

    fn plain_ui() -> SetupUi {
        SetupUi {
            profile: crate::presentation::ResolvedPresentation::test_plain(),
            rich: false,
        }
    }

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
    fn full_setup_creates_one_complete_named_default_without_advanced_binding_flags() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        let args = SetupArgs {
            full: true,
            non_interactive: true,
            profile: Some("xana-dev".into()),
            ..SetupArgs::default()
        };
        let result = customize_quick(
            base(),
            &args,
            &paths,
            &mut std::io::Cursor::new([]),
            &mut Vec::new(),
            plain_ui(),
        )
        .unwrap();
        let registry = XanaConfig::parse_registry(&result.config).unwrap();
        assert_eq!(registry.default_profile, "xana-dev");
        assert_eq!(registry.profiles.len(), 1);
        assert_eq!(registry.profiles["xana-dev"].connection, "ollama");
        assert_eq!(registry.profiles["xana-dev"].model, "old");
        assert!(
            registry
                .routes
                .values()
                .all(|route| route.profile == "xana-dev")
        );
        assert!(!paths.config_file().exists());
    }

    #[test]
    fn additional_setup_profile_does_not_replace_the_default_or_its_binding() {
        let directory = tempdir().unwrap();
        let paths =
            XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
        fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        fs::write(paths.config_file(), base()).unwrap();
        let result =
            merge_setup_profile(&paths, base().replace("old", "new"), Some("work")).unwrap();
        let registry = XanaConfig::parse_registry(&result).unwrap();
        assert_eq!(registry.default_profile, "default");
        assert_eq!(registry.profiles["default"].model, "old");
        assert_eq!(registry.profiles["work"].model, "new");
        assert_eq!(registry.connections.len(), 1);
        assert_ne!(
            registry.profiles["default"].profile_id,
            registry.profiles["work"].profile_id
        );
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
    fn reconfiguration_normalizes_the_existing_legacy_profile_binding() {
        let existing = base().replace("connection = \"ollama\"", "provider = \"ollama\"");
        let replacement = base().replace("old", "new");
        let merged = merge_connection(&existing, &replacement).unwrap();
        let registry = XanaConfig::parse_registry(&merged).unwrap();
        assert_eq!(registry.profiles["default"].connection, "ollama");
        assert_eq!(registry.profiles["default"].model, "new");
        assert!(!merged.contains("provider = \"ollama\""));
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
            plain_ui(),
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
            plain_ui(),
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
            plain_ui(),
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
            plain_ui(),
        )
        .unwrap();
        assert_eq!(
            outcome,
            SetupOutcome::Committed {
                execution_changed: false
            }
        );
        assert_eq!(fs::read(paths.config_file()).unwrap(), original);
        let preferences = PresentationPreferences::load(&paths.presentation_file()).preferences;
        assert_eq!(preferences.theme, PresentationTheme::Monochrome);
    }
}
