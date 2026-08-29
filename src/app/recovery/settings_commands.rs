//! Scriptable adapter over the shared settings interface.

use crate::{
    paths::XanaPaths,
    settings::{
        SettingChange, SettingEntry, SettingKind, SettingValue, SettingsManager, SettingsReceipt,
        SettingsSection, SettingsSnapshot,
    },
};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;

const SETTINGS_DOCUMENT_VERSION: u16 = 1;

#[derive(Serialize)]
struct SettingsListDocument<'a> {
    version: u16,
    revision: &'a str,
    warnings: &'a [String],
    settings: Vec<&'a SettingEntry>,
}

pub(super) fn list<W: Write>(
    paths: &XanaPaths,
    section: Option<&str>,
    search: Option<&str>,
    json: bool,
    output: &mut W,
) -> Result<()> {
    let snapshot = SettingsManager::new(paths).snapshot()?;
    let section = section
        .map(|section| {
            SettingsSection::parse(section)
                .ok_or_else(|| crate::settings::SettingsError::UnknownSection(section.to_owned()))
        })
        .transpose()?;
    let search = search.map(str::trim).filter(|query| !query.is_empty());
    let entries = matching_entries(&snapshot, section, search);
    if json {
        serde_json::to_writer_pretty(
            &mut *output,
            &SettingsListDocument {
                version: SETTINGS_DOCUMENT_VERSION,
                revision: &snapshot.revision,
                warnings: &snapshot.warnings,
                settings: entries,
            },
        )?;
        writeln!(output)?;
        return Ok(());
    }
    write_list_text(output, &snapshot, &entries, section, search)
}

pub(super) fn get<W: Write>(
    paths: &XanaPaths,
    key: &str,
    json: bool,
    output: &mut W,
) -> Result<()> {
    let snapshot = SettingsManager::new(paths).snapshot()?;
    let entry = require_entry(&snapshot, key)?;
    if json {
        serde_json::to_writer_pretty(&mut *output, entry)?;
        writeln!(output)?;
    } else {
        writeln!(output, "{}", machine_or_display_value(&entry.value))?;
    }
    Ok(())
}

pub(super) fn explain<W: Write>(
    paths: &XanaPaths,
    key: &str,
    json: bool,
    output: &mut W,
) -> Result<()> {
    let snapshot = SettingsManager::new(paths).snapshot()?;
    let entry = require_entry(&snapshot, key)?;
    if json {
        serde_json::to_writer_pretty(&mut *output, entry)?;
        writeln!(output)?;
        return Ok(());
    }

    writeln!(output, "{}", entry.label)?;
    writeln!(output, "  Key:     {}", entry.key)?;
    writeln!(output, "  Current: {}", entry.value.display)?;
    writeln!(
        output,
        "  Default: {}",
        entry
            .default
            .as_ref()
            .map_or("Not applicable", |value| value.display.as_str())
    )?;
    writeln!(output, "  Source:  {}", entry.source.label())?;
    writeln!(output, "  Scope:   {}", entry.target.label())?;
    writeln!(output, "  Effect:  {}", entry.effect.label())?;
    writeln!(output, "  Type:    {}", setting_kind_label(entry.kind))?;
    if !entry.choices.is_empty() {
        writeln!(output, "  Choices: {}", entry.choices.join(", "))?;
    }
    writeln!(output)?;
    writeln!(output, "{}", entry.description)?;
    if let Some(action) = &entry.action {
        writeln!(output)?;
        writeln!(output, "Open its focused manager with `{action}`.")?;
    }
    Ok(())
}

pub(super) fn set<W: Write>(
    paths: &XanaPaths,
    key: &str,
    value: &str,
    dry_run: bool,
    json: bool,
    output: &mut W,
) -> Result<()> {
    let manager = SettingsManager::new(paths);
    let mut draft = manager.begin()?;
    draft.set(key, value)?;
    let receipt = manager.commit(draft, dry_run)?;
    write_receipt(output, &receipt, json)
}

pub(super) fn reset<W: Write>(
    paths: &XanaPaths,
    key: &str,
    dry_run: bool,
    json: bool,
    output: &mut W,
) -> Result<()> {
    let manager = SettingsManager::new(paths);
    let mut draft = manager.begin()?;
    draft.reset(key)?;
    let receipt = manager.commit(draft, dry_run)?;
    write_receipt(output, &receipt, json)
}

fn matching_entries<'a>(
    snapshot: &'a SettingsSnapshot,
    section: Option<SettingsSection>,
    search: Option<&str>,
) -> Vec<&'a SettingEntry> {
    let folded = search.map(str::to_ascii_lowercase);
    let entries = section.map_or_else(
        || snapshot.entries.iter().collect::<Vec<_>>(),
        |section| snapshot.entries_in(section),
    );
    entries
        .into_iter()
        .filter(|entry| {
            folded.as_ref().is_none_or(|query| {
                entry.key.to_ascii_lowercase().contains(query)
                    || entry.label.to_ascii_lowercase().contains(query)
                    || entry.description.to_ascii_lowercase().contains(query)
                    || entry.value.display.to_ascii_lowercase().contains(query)
            })
        })
        .collect()
}

fn require_entry<'a>(
    snapshot: &'a SettingsSnapshot,
    key: &str,
) -> Result<&'a SettingEntry, crate::settings::SettingsError> {
    snapshot
        .entry(key.trim())
        .ok_or_else(|| crate::settings::SettingsError::UnknownKey(key.trim().to_owned()))
}

fn write_list_text<W: Write>(
    output: &mut W,
    snapshot: &SettingsSnapshot,
    entries: &[&SettingEntry],
    section: Option<SettingsSection>,
    search: Option<&str>,
) -> Result<()> {
    writeln!(output, "Xana settings")?;
    writeln!(output, "  Revision: {}", snapshot.revision)?;
    if let Some(section) = section {
        writeln!(output, "  Section:  {}", section.title())?;
    }
    if let Some(search) = search {
        writeln!(output, "  Search:   {search:?}")?;
    }
    for warning in &snapshot.warnings {
        writeln!(output, "  Warning:  {warning}")?;
    }
    if entries.is_empty() {
        writeln!(output)?;
        writeln!(output, "No settings matched.")?;
        return Ok(());
    }

    for section in SettingsSection::all() {
        let rows = entries
            .iter()
            .copied()
            .filter(|entry| entry.section == section)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        writeln!(output)?;
        writeln!(output, "{}", section.title())?;
        for entry in rows {
            writeln!(output, "  {:<34} {}", entry.key, entry.value.display)?;
            writeln!(
                output,
                "    {} · {} · {}",
                entry.source.label(),
                entry.target.label(),
                entry.effect.label()
            )?;
        }
    }
    writeln!(output)?;
    writeln!(
        output,
        "Use `xana config explain KEY` for details or `xana settings` for the interactive browser."
    )?;
    Ok(())
}

fn write_receipt<W: Write>(output: &mut W, receipt: &SettingsReceipt, json: bool) -> Result<()> {
    if json {
        serde_json::to_writer_pretty(&mut *output, receipt)?;
        writeln!(output)?;
        return Ok(());
    }

    writeln!(
        output,
        "{}",
        if receipt.dry_run {
            "Settings preview (no files changed)"
        } else if receipt.changes.is_empty() {
            "Settings already matched; no files changed"
        } else {
            "Settings applied"
        }
    )?;
    for change in &receipt.changes {
        write_change(output, change)?;
    }
    writeln!(
        output,
        "  Revision: {} -> {}",
        receipt.revision_before, receipt.revision_after
    )?;
    if receipt.requires_new_conversation() {
        writeln!(
            output,
            "  Active conversations are unchanged; this applies to new conversations."
        )?;
    }
    Ok(())
}

fn write_change<W: Write>(output: &mut W, change: &SettingChange) -> Result<()> {
    writeln!(output)?;
    writeln!(output, "  {} ({})", change.label, change.key)?;
    writeln!(
        output,
        "    {} -> {}",
        change.before.display, change.after.display
    )?;
    writeln!(output, "    Scope:  {}", change.target.label())?;
    writeln!(output, "    Effect: {}", change.effect.label())?;
    Ok(())
}

fn machine_or_display_value(value: &SettingValue) -> &str {
    value.raw.as_deref().unwrap_or(&value.display)
}

fn setting_kind_label(kind: SettingKind) -> &'static str {
    match kind {
        SettingKind::Boolean => "Boolean",
        SettingKind::Choice => "Choice",
        SettingKind::Integer => "Integer",
        SettingKind::Bytes => "Byte size",
        SettingKind::DurationDays => "Duration in days",
        SettingKind::OptionalPath => "Optional path",
        SettingKind::ReadOnly => "Read-only summary",
    }
}

#[cfg(test)]
mod tests;
