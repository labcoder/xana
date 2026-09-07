//! Guided protection lifecycle; model setup and cryptographic mechanisms stay separate.

use super::{
    SetupArgs, SetupBack, SetupOutcome,
    ui::{self, SelectOption, SetupUi},
};
use crate::{
    paths::XanaPaths,
    storage::{KeyCustody, OsCustody, ProtectedStore, StorageStatus, migration, recovery},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{BufRead, Write},
    path::PathBuf,
};

const LOSS_WARNING: &str = "Keys are managed automatically. Until you save a separate recovery backup, losing the OS key can make your data unrecoverable.";

pub(super) enum FreshPlan {
    Keep,
    Protect { export: Option<PathBuf> },
}

impl FreshPlan {
    pub(super) fn automatic(paths: &XanaPaths) -> Result<Self> {
        Ok(if is_empty(paths)? {
            Self::Protect { export: None }
        } else {
            Self::Keep
        })
    }
    pub(super) fn review(&self) -> String {
        match self {
            Self::Keep => {
                "Storage      unchanged; use setup --section storage to review protection and recovery".into()
            }
            Self::Protect { export: Some(path) } => {
                format!("Storage      encrypted, OS-managed unlock; private recovery backup: {}", path.display())
            }
            Self::Protect { export: None } => LOSS_WARNING.into(),
        }
    }

    pub(super) fn apply(&self, paths: &XanaPaths, custody: &dyn KeyCustody) -> Result<()> {
        if let Self::Protect { export } = self {
            if let Some(destination) = export {
                recovery::validate_destination(paths.data_dir(), destination)?;
            }
            let _lock = crate::config::ConfigTransactionLock::acquire(paths.config_file())?;
            migration::fence_config(paths)?;
            let store = ProtectedStore::initialize_managed(paths.data_dir(), custody)?;
            if let Some(destination) = export {
                store.export_recovery(destination).context("protection is enabled, but recovery export failed; use setup --section storage to retry")?;
            }
        }
        Ok(())
    }

    pub(super) fn record_receipt(&self, receipts: &mut Vec<String>) {
        if let Self::Protect { export: Some(path) } = self {
            receipts.push(export_receipt(path));
        }
    }
}

fn export_receipt(path: &std::path::Path) -> String {
    format!(
        "Recovery backup saved at {}. Keep it private and back it up separately.",
        path.display()
    )
}

pub(super) fn fresh_plan(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
) -> Result<FreshPlan> {
    if args.legacy_storage || !is_empty(paths)? {
        ensure!(
            args.recovery_output.is_none(),
            "use setup --section storage to export recovery for an existing home"
        );
        return Ok(FreshPlan::Keep);
    }
    Ok(FreshPlan::Protect {
        export: choose_backup(args, paths, input, output, ui)?,
    })
}

fn is_empty(paths: &XanaPaths) -> Result<bool> {
    // An incomplete initialization/transition is never a fresh home.
    if !matches!(
        ProtectedStore::status(paths.data_dir())?,
        StorageStatus::Legacy
    ) {
        return Ok(false);
    }
    Ok(!paths.data_dir().exists() || fs::read_dir(paths.data_dir())?.next().is_none())
}

fn choose_backup(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
) -> Result<Option<PathBuf>> {
    if let Some(path) = &args.recovery_output {
        recovery::validate_destination(paths.data_dir(), path)?;
        return Ok(Some(path.clone()));
    }
    if args.non_interactive || args.dry_run {
        return Ok(None);
    }
    let save = if ui.rich {
        let options = [
            SelectOption::new(
                "Save recovery backup now",
                "Xana creates a recovery-key file; choose its full filename, not an existing key or folder",
            ),
            SelectOption::new("Later", LOSS_WARNING),
        ];
        ui::select(
            output,
            ui,
            "Protect your memories and conversations",
            &options,
            0,
        )?
        .ok_or(SetupBack)?
            == 0
    } else {
        writeln!(
            output,
            "{LOSS_WARNING}\n1. Save recovery backup now\n2. Later"
        )?;
        match super::prompt_default(input, output, "Recovery backup", "1")?.as_str() {
            "1" | "now" => true,
            "2" | "later" => false,
            _ => anyhow::bail!("choose 1 (save now) or 2 (later)"),
        }
    };
    if !save {
        return Ok(None);
    }
    let destination_hint = if cfg!(windows) {
        "Xana creates the key. New file, e.g. C:\\Users\\you\\Documents\\xana-recovery.txt"
    } else {
        "Xana creates the key. New file, e.g. /home/you/Documents/xana-recovery.txt"
    };
    let path = ui::prompt_value(input, output, ui, destination_hint, "", true, false)?;
    let path = PathBuf::from(path);
    recovery::validate_destination(paths.data_dir(), &path).context(
        "choose a new recovery-key filename in an existing folder outside Xana data; do not enter a folder or an existing key",
    )?;
    Ok(Some(path))
}

pub(super) fn run(
    args: &SetupArgs,
    paths: &XanaPaths,
    input: &mut impl BufRead,
    output: &mut impl Write,
    ui: SetupUi,
    receipts: &mut Vec<String>,
) -> Result<SetupOutcome> {
    ensure!(
        !args.legacy_storage,
        "storage setup cannot disable existing protection"
    );
    if migration::journal_path(paths.data_dir()).exists() {
        if let Some(path) = &args.recovery_output {
            recovery::validate_destination(paths.data_dir(), path)?;
        }
        let review = vec!["Resume the interrupted reviewed migration using the existing OS-held key. No new key or model call.".into(), LOSS_WARNING.into()];
        if args.dry_run
            || (!args.yes
                && !ui::confirm_review(input, output, ui, "Resume storage migration", &review)?)
        {
            return Ok(SetupOutcome::Unchanged);
        }
        let retained = migration::resume_managed(paths, &OsCustody)?;
        receipts.push(format!(
            "Migration resumed. Prior plaintext generation retained at {}. Remove only after checking recovery; no secure erasure is promised.",
            retained.display()
        ));
        if let Some(path) = &args.recovery_output {
            ProtectedStore::configured(paths.data_dir())?
                .context("migration resumed but protected storage could not be reopened")?
                .export_recovery(path)?;
            receipts.push(export_receipt(path));
        }
        return Ok(SetupOutcome::Committed {
            requires_new_conversation: true,
        });
    }
    let status = ProtectedStore::status(paths.data_dir())?;
    if matches!(status, StorageStatus::Protected { locked: true, .. }) {
        anyhow::bail!(
            "storage is explicitly locked; run xana storage unlock before exporting recovery"
        );
    }
    if matches!(status, StorageStatus::Protected { .. })
        && recovery::status(paths.data_dir())? == recovery::RecoveryStatus::UserManaged
    {
        receipts.push("Protection is already enabled. Keep your original user-supplied recovery key; no replacement was generated.".into());
        return Ok(SetupOutcome::Unchanged);
    }
    let empty = is_empty(paths)?;
    let plan = if status == StorageStatus::Legacy && !empty {
        Some(migration::preview(paths)?)
    } else {
        None
    };
    let export = choose_backup(args, paths, input, output, ui)?;
    let mut review = vec![
        LOSS_WARNING.into(),
        format!("Unlock       {}", recovery::custody_label()),
    ];
    if let Some(plan) = &plan {
        review.push(format!(
            "Migrate      {} files / {} bytes; preserve prior plaintext generation",
            plan.files, plan.bytes
        ));
        review.push(format!(
            "Free space   at least {} bytes required; no effect replay",
            plan.required_free_bytes
        ));
    } else if empty {
        review.push("Initialize   fresh encrypted home".into());
    } else {
        review.push(format!(
            "Recovery     {}",
            recovery::status(paths.data_dir())?.description()
        ));
    }
    review.push(export.as_ref().map_or_else(
        || "Recovery backup: Later".into(),
        |path| {
            format!(
                "Recovery backup: {} (new private file; back up separately)",
                path.display()
            )
        },
    ));
    if !ui.rich {
        for line in &review {
            writeln!(output, "{line}")?;
        }
    }
    if args.dry_run
        || (!args.yes
            && !ui::confirm_review(input, output, ui, "Review storage protection", &review)?)
    {
        return Ok(SetupOutcome::Unchanged);
    }
    let changed = plan.is_some() || empty;
    if let Some(plan) = plan {
        let retained = migration::apply_managed(paths, &OsCustody, &plan.review)?;
        receipts.push(format!(
            "Migration verified. Prior plaintext generation retained at {}. Remove only after checking recovery; no secure erasure is promised.",
            retained.display()
        ));
    } else if empty {
        FreshPlan::Protect { export: None }.apply(paths, &OsCustody)?;
    }
    if let Some(path) = export {
        ProtectedStore::configured(paths.data_dir())?
            .ok_or_else(|| anyhow::anyhow!("protected storage unavailable"))?
            .export_recovery(&path)?;
        receipts.push(export_receipt(&path));
    }
    Ok(SetupOutcome::Committed {
        requires_new_conversation: changed,
    })
}

#[cfg(test)]
mod tests;
