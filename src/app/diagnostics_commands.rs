//! Explicit local inspection of metadata-only diagnostics.

use crate::{cli::LogsCommand, diagnostics, paths::XanaPaths};
use anyhow::Result;
use std::io::Write;

pub(super) fn run(command: LogsCommand, paths: &XanaPaths, output: &mut impl Write) -> Result<()> {
    match command {
        LogsCommand::Path => {
            let settings = diagnostics::load_settings(paths);
            let (logs, crashes) = diagnostics::resolve_directories(paths, &settings)?;
            writeln!(output, "Logs:    {}", logs.display())?;
            writeln!(output, "Crashes: {}", crashes.display())?;
            writeln!(output, "Retention: {} day(s)", settings.retention_days)?;
            writeln!(output, "Level: {:?}", settings.level)?;
        }
        LogsCommand::List => {
            let entries = diagnostics::list(paths)?;
            if entries.is_empty() {
                writeln!(output, "No Xana log or crash entries are present.")?;
            } else {
                for entry in entries {
                    writeln!(
                        output,
                        "{}\t{}\t{} bytes",
                        entry.kind, entry.name, entry.bytes
                    )?;
                }
            }
        }
        LogsCommand::Show {
            name,
            lines,
            follow,
        } => {
            for record in diagnostics::read_records(paths, &name, lines)? {
                writeln!(output, "{record}")?;
            }
            if follow {
                diagnostics::follow_records(paths, &name, output)?;
            }
        }
        LogsCommand::Export { output: path } => {
            diagnostics::export_support_bundle(paths, &path)?;
            writeln!(output, "Support bundle created: {}", path.display())?;
            writeln!(
                output,
                "Review it locally before sharing; Xana uploads nothing."
            )?;
        }
    }
    Ok(())
}
