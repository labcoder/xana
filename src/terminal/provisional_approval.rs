//! Terminal adapter for the temporary per-command approval contract.

use crate::approval::{ApprovalError, ProvisionalApprover, RequestedAction};
use std::io::{self, BufRead, Write};

pub(crate) fn terminal_approver() -> Box<dyn ProvisionalApprover> {
    Box::new(TerminalApprover)
}

struct TerminalApprover;

impl ProvisionalApprover for TerminalApprover {
    fn confirm(&self, action: &RequestedAction) -> Result<bool, ApprovalError> {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let stdout = anstream::stdout();
        let mut output = stdout.lock();

        confirm_with_io(action, &mut input, &mut output).map_err(ApprovalError::io)
    }
}

fn confirm_with_io<R: BufRead, W: Write>(
    action: &RequestedAction,
    input: &mut R,
    output: &mut W,
) -> io::Result<bool> {
    writeln!(output, "{} requests local host execution", action.tool_name)?;
    writeln!(output, "shell: {}", action.shell)?;
    writeln!(output, "cwd: {}", action.cwd.display())?;
    writeln!(output, "command: {}", action.command)?;
    writeln!(output, "argv: {}", action.argv)?;
    writeln!(
        output,
        "This process will use Xana's ordinary host permissions; it is not contained."
    )?;
    write!(output, "approve once? [y/N] ")?;
    output.flush()?;

    let mut answer = String::new();
    if input.read_line(&mut answer)? == 0 {
        return Ok(false);
    }

    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, path::PathBuf};

    fn action() -> RequestedAction {
        RequestedAction {
            tool_name: "run_command",
            shell: "PowerShell",
            command: "cargo test".to_owned(),
            argv: r#""powershell.exe" "-Command" "cargo test""#.to_owned(),
            cwd: PathBuf::from(r"C:\dev\project"),
        }
    }

    #[test]
    fn only_an_explicit_yes_approves() {
        for answer in ["y\n", "YES\r\n", " yes \n"] {
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();

            assert!(confirm_with_io(&action(), &mut input, &mut output).expect("approval prompt"));
        }

        for answer in ["", "\n", "n\n", "approve\n"] {
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();

            assert!(!confirm_with_io(&action(), &mut input, &mut output).expect("denial prompt"));
        }
    }

    #[test]
    fn prompt_discloses_the_exact_action_and_host_boundary() {
        let mut input = Cursor::new(b"n\n");
        let mut output = Vec::new();

        confirm_with_io(&action(), &mut input, &mut output).expect("approval prompt");
        let transcript = String::from_utf8(output).expect("UTF-8 transcript");

        assert!(transcript.contains("run_command requests local host execution"));
        assert!(transcript.contains("shell: PowerShell"));
        assert!(transcript.contains(r"cwd: C:\dev\project"));
        assert!(transcript.contains("command: cargo test"));
        assert!(transcript.contains("ordinary host permissions"));
        assert!(transcript.contains("not contained"));
    }
}
