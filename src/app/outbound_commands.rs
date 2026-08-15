//! CLI ownership for saved outbound-data decisions.

use crate::{
    cli::OutboundCommand, outbound::OutboundGuard, paths::XanaPaths,
    private_state::SavedOutboundDecision,
};
use anyhow::Result;
use std::io::Write;

pub(super) fn run(
    command: OutboundCommand,
    paths: &XanaPaths,
    output: &mut dyn Write,
) -> Result<()> {
    let guard = OutboundGuard::open(paths)?;
    match command {
        OutboundCommand::List => {
            let decisions = guard.list_decisions()?;
            if decisions.is_empty() {
                writeln!(output, "No saved outbound-data decisions.")?;
                return Ok(());
            }
            writeln!(
                output,
                "DECISION  CLASS                   UPDATED_MS     RECIPIENT IDENTITY DIGEST"
            )?;
            for record in decisions {
                let decision = match record.decision {
                    SavedOutboundDecision::Allow => "allow",
                    SavedOutboundDecision::Deny => "deny",
                };
                writeln!(
                    output,
                    "{decision:<9} {:<23} {:<14} {}",
                    record.data_class.as_str(),
                    record.updated_unix_ms,
                    record.recipient_identity_digest
                )?;
            }
            Ok(())
        }
        OutboundCommand::Revoke {
            identity_digest,
            class,
            yes,
        } => {
            if !yes {
                anyhow::bail!(
                    "refusing to revoke an outbound-data decision without --yes; review `xana outbound list` first"
                );
            }
            let class = class.into();
            if guard.revoke_digest(&identity_digest, class)? {
                writeln!(
                    output,
                    "Revoked {} for recipient identity {}.",
                    class.as_str(),
                    identity_digest
                )?;
            } else {
                writeln!(
                    output,
                    "No saved {} decision exists for recipient identity {}.",
                    class.as_str(),
                    identity_digest
                )?;
            }
            Ok(())
        }
    }
}
