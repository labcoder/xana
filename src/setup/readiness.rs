//! Installer-facing setup readiness without duplicating configuration policy.

use super::{SetupArgs, SetupCancelled, SetupOutcome};
use crate::{config::ConfigReadiness, paths::XanaPaths};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{error::Error, fmt, io::BufRead, io::Write, process::ExitCode};

const SETUP_PENDING_EXIT: u8 = 10;

#[derive(Debug)]
pub(crate) struct SetupPending;

impl SetupPending {
    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::from(SETUP_PENDING_EXIT)
    }
}

impl fmt::Display for SetupPending {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Xana setup remains pending")
    }
}

impl Error for SetupPending {}

#[derive(Debug, Serialize)]
struct ReadinessReceipt {
    version: u8,
    status: &'static str,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnose: Option<&'static str>,
}

pub(crate) async fn run(
    paths: &XanaPaths,
    input_is_terminal: bool,
    output_is_terminal: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let readiness = ConfigReadiness::inspect(paths.config_file());
    if readiness == ConfigReadiness::Healthy {
        writeln!(
            output,
            "Xana setup is ready; existing configuration was preserved."
        )?;
        return write_receipt(output, "ready", readiness, None);
    }
    if readiness == ConfigReadiness::Indeterminate {
        bail!(
            "could not determine Xana setup readiness; no state changed\nrun `xana doctor`, then `xana setup` after resolving the reported filesystem problem"
        );
    }

    let interactive = input_is_terminal && output_is_terminal;
    if !interactive {
        writeln!(
            output,
            "Xana setup is pending because configuration is {}.",
            readiness.as_str()
        )?;
        writeln!(output, "Next: xana setup")?;
        writeln!(output, "Diagnose: xana doctor")?;
        write_receipt(output, "pending", readiness, Some("xana setup"))?;
        return Err(SetupPending.into());
    }

    writeln!(
        output,
        "Xana configuration is {}; starting the canonical setup flow.",
        readiness.as_str()
    )?;
    if matches!(
        readiness,
        ConfigReadiness::Invalid | ConfigReadiness::Incompatible
    ) {
        writeln!(
            output,
            "The existing file will be preserved as a backup only after you confirm a replacement."
        )?;
    }

    let args = SetupArgs::default();
    match super::run(
        &args,
        paths,
        true,
        true,
        input,
        output,
        crate::presentation::ResolvedPresentation::plain(),
    )
    .await
    {
        Ok(SetupOutcome::Committed { .. }) => write_receipt(output, "configured", readiness, None),
        Ok(SetupOutcome::Unchanged) => {
            writeln!(output, "Xana setup remains pending; no changes were made.")?;
            write_receipt(output, "pending", readiness, Some("xana setup"))?;
            Err(SetupPending.into())
        }
        Err(error) if error.downcast_ref::<SetupCancelled>().is_some() => {
            writeln!(output, "Xana setup remains pending; no changes were made.")?;
            write_receipt(output, "pending", readiness, Some("xana setup"))?;
            Err(SetupPending.into())
        }
        Err(error) => Err(error),
    }
}

fn write_receipt(
    output: &mut impl Write,
    status: &'static str,
    readiness: ConfigReadiness,
    next: Option<&'static str>,
) -> Result<()> {
    let receipt = ReadinessReceipt {
        version: 1,
        status,
        reason: readiness.as_str(),
        next,
        diagnose: next.map(|_| "xana doctor"),
    };
    writeln!(
        output,
        "XANA_SETUP_RESULT {}",
        serde_json::to_string(&receipt)?
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::XanaPaths;
    use std::{fs, io::Cursor};
    use tempfile::tempdir;

    #[tokio::test]
    async fn noninteractive_missing_invalid_and_incompatible_state_remains_untouched() {
        for (input, reason) in [
            (None, "missing"),
            (Some("version = ["), "invalid"),
            (Some("version = 99"), "incompatible"),
        ] {
            let directory = tempdir().unwrap();
            let paths =
                XanaPaths::resolve(Some(directory.path().join("home").into_os_string())).unwrap();
            if let Some(input) = input {
                fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
                fs::write(paths.config_file(), input).unwrap();
            }
            let before = fs::read(paths.config_file()).ok();
            let mut output = Vec::new();
            let error = run(
                &paths,
                false,
                false,
                &mut Cursor::new(Vec::<u8>::new()),
                &mut output,
            )
            .await
            .unwrap_err();
            assert!(error.downcast_ref::<SetupPending>().is_some());
            assert_eq!(fs::read(paths.config_file()).ok(), before);
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains(&format!(r#""reason":"{reason}""#)));
            assert!(!paths.config_file().with_extension("toml.bak").exists());
        }
    }
}
