//! Cross-platform shell selection and pure process planning.
//!
//! A shell turns one user-visible command string into an exact program and
//! argument vector. Resolution is configuration validation; it never spawns a
//! process or inspects ambient shell variables.

use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShellKind {
    #[default]
    Platform,
    Posix,
    GitBash,
    #[serde(rename = "powershell")]
    PowerShell,
    Cmd,
}

impl ShellKind {
    pub(crate) fn config_name(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Posix => "posix",
            Self::GitBash => "git_bash",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellConfig {
    #[serde(default)]
    pub(crate) kind: ShellKind,
    #[serde(default)]
    pub(crate) program: Option<PathBuf>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            kind: ShellKind::Platform,
            program: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shell {
    kind: ResolvedShellKind,
    program: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedShellKind {
    #[cfg(unix)]
    Posix,
    #[cfg(windows)]
    GitBash,
    #[cfg(windows)]
    PowerShell,
    #[cfg(windows)]
    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessPlan {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ShellError {
    UnsupportedOnPlatform { kind: ShellKind },
    InvalidProgram { reason: &'static str },
}

impl Shell {
    pub(crate) fn resolve(config: ShellConfig) -> Result<Self, ShellError> {
        if config.program.as_ref().is_some_and(|program| {
            program.as_os_str().is_empty()
                || program
                    .to_str()
                    .is_some_and(|program| program.trim().is_empty())
        }) {
            return Err(ShellError::InvalidProgram {
                reason: "program must not be empty",
            });
        }

        let kind = resolve_kind(config.kind)?;
        let program = config.program.unwrap_or_else(|| default_program(kind));

        Ok(Self { kind, program })
    }

    pub(crate) fn plan(&self, command: &str) -> ProcessPlan {
        let args = match self.kind {
            #[cfg(unix)]
            ResolvedShellKind::Posix => {
                vec!["-lc".to_owned(), command.to_owned()]
            }
            #[cfg(windows)]
            ResolvedShellKind::GitBash => vec!["-lc".to_owned(), command.to_owned()],
            #[cfg(windows)]
            ResolvedShellKind::PowerShell => vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                command.to_owned(),
            ],
            #[cfg(windows)]
            ResolvedShellKind::Cmd => vec![
                "/D".to_owned(),
                "/S".to_owned(),
                "/C".to_owned(),
                command.to_owned(),
            ],
        };

        ProcessPlan {
            program: self.program.clone(),
            args,
        }
    }

    pub(crate) fn display_name(&self) -> &'static str {
        match self.kind {
            #[cfg(unix)]
            ResolvedShellKind::Posix => "POSIX shell",
            #[cfg(windows)]
            ResolvedShellKind::GitBash => "Git Bash",
            #[cfg(windows)]
            ResolvedShellKind::PowerShell => "PowerShell",
            #[cfg(windows)]
            ResolvedShellKind::Cmd => "Command Prompt",
        }
    }

    pub(crate) fn prompt_description(&self) -> String {
        format!(
            "{} (program: {})",
            self.display_name(),
            self.program.display()
        )
    }
}

/// Formats argv for review only; the result is intentionally not a command
/// string and is not parsed back for execution.
pub(crate) fn display_argv(plan: &ProcessPlan) -> String {
    std::iter::once(display_os_str(&plan.program))
        .chain(plan.args.iter().map(|argument| format!("{argument:?}")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_os_str(path: &Path) -> String {
    format!("{:?}", path.as_os_str())
}

#[cfg(unix)]
fn resolve_kind(kind: ShellKind) -> Result<ResolvedShellKind, ShellError> {
    match kind {
        ShellKind::Platform | ShellKind::Posix => Ok(ResolvedShellKind::Posix),
        ShellKind::GitBash | ShellKind::PowerShell | ShellKind::Cmd => {
            Err(ShellError::UnsupportedOnPlatform { kind })
        }
    }
}

#[cfg(windows)]
fn resolve_kind(kind: ShellKind) -> Result<ResolvedShellKind, ShellError> {
    match kind {
        ShellKind::Platform | ShellKind::PowerShell => Ok(ResolvedShellKind::PowerShell),
        ShellKind::GitBash => Ok(ResolvedShellKind::GitBash),
        ShellKind::Cmd => Ok(ResolvedShellKind::Cmd),
        ShellKind::Posix => Err(ShellError::UnsupportedOnPlatform { kind }),
    }
}

fn default_program(kind: ResolvedShellKind) -> PathBuf {
    match kind {
        #[cfg(unix)]
        ResolvedShellKind::Posix => PathBuf::from("sh"),
        #[cfg(windows)]
        ResolvedShellKind::GitBash => PathBuf::from("bash.exe"),
        #[cfg(windows)]
        ResolvedShellKind::PowerShell => PathBuf::from("powershell.exe"),
        #[cfg(windows)]
        ResolvedShellKind::Cmd => PathBuf::from("cmd.exe"),
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOnPlatform { kind } => write!(
                f,
                "shell kind {:?} is not supported on this platform",
                kind.config_name()
            ),
            Self::InvalidProgram { reason } => write!(f, "invalid shell program: {reason}"),
        }
    }
}

impl Error for ShellError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn platform_shell_plans_posix_argv() {
        let shell = Shell::resolve(ShellConfig::default()).expect("platform shell");

        assert_eq!(
            shell.plan("printf 'hello world'"),
            ProcessPlan {
                program: PathBuf::from("sh"),
                args: vec!["-lc".to_owned(), "printf 'hello world'".to_owned()],
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn platform_shell_plans_powershell_without_profiles() {
        let shell = Shell::resolve(ShellConfig::default()).expect("platform shell");

        assert_eq!(
            shell.plan("Write-Output 'hello world'"),
            ProcessPlan {
                program: PathBuf::from("powershell.exe"),
                args: vec![
                    "-NoLogo".to_owned(),
                    "-NoProfile".to_owned(),
                    "-NonInteractive".to_owned(),
                    "-Command".to_owned(),
                    "Write-Output 'hello world'".to_owned(),
                ],
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_kinds_preserve_command_as_one_argument() {
        let command = "Write-Output 'a b'; exit 7";
        let cases = [
            (
                ShellKind::GitBash,
                vec!["-lc".to_owned(), command.to_owned()],
            ),
            (
                ShellKind::PowerShell,
                vec![
                    "-NoLogo".to_owned(),
                    "-NoProfile".to_owned(),
                    "-NonInteractive".to_owned(),
                    "-Command".to_owned(),
                    command.to_owned(),
                ],
            ),
            (
                ShellKind::Cmd,
                vec![
                    "/D".to_owned(),
                    "/S".to_owned(),
                    "/C".to_owned(),
                    command.to_owned(),
                ],
            ),
        ];

        for (kind, expected_args) in cases {
            let shell = Shell::resolve(ShellConfig {
                kind,
                program: None,
            })
            .expect("supported Windows shell");

            assert_eq!(shell.plan(command).args, expected_args);
        }
    }

    #[test]
    fn blank_custom_program_is_rejected() {
        let result = Shell::resolve(ShellConfig {
            kind: ShellKind::Platform,
            program: Some(PathBuf::from(std::ffi::OsStr::new(""))),
        });

        assert_eq!(
            result,
            Err(ShellError::InvalidProgram {
                reason: "program must not be empty"
            })
        );

        let whitespace = Shell::resolve(ShellConfig {
            kind: ShellKind::Platform,
            program: Some(PathBuf::from("   ")),
        });
        assert_eq!(
            whitespace,
            Err(ShellError::InvalidProgram {
                reason: "program must not be empty"
            })
        );
    }

    #[test]
    fn powershell_uses_its_public_configuration_spelling() {
        assert_eq!(
            serde_json::to_value(ShellKind::PowerShell).expect("serialize shell kind"),
            "powershell"
        );
    }

    #[test]
    fn incompatible_explicit_shell_is_rejected_on_this_platform() {
        #[cfg(unix)]
        let kind = ShellKind::PowerShell;
        #[cfg(windows)]
        let kind = ShellKind::Posix;

        let result = Shell::resolve(ShellConfig {
            kind,
            program: None,
        });

        assert_eq!(result, Err(ShellError::UnsupportedOnPlatform { kind }));
    }

    #[test]
    fn display_quotes_program_and_arguments_without_round_trip_claims() {
        let plan = ProcessPlan {
            program: PathBuf::from("shell with spaces"),
            args: vec![
                "plain".to_owned(),
                "a b".to_owned(),
                "say \"hi\"".to_owned(),
            ],
        };

        assert_eq!(
            display_argv(&plan),
            r#""shell with spaces" "plain" "a b" "say \"hi\"""#
        );
    }

    #[cfg(unix)]
    #[test]
    fn display_handles_non_utf8_program_paths_losslessly_for_review() {
        use std::os::unix::ffi::OsStringExt;

        let plan = ProcessPlan {
            program: PathBuf::from(std::ffi::OsString::from_vec(vec![b's', b'h', 0xff])),
            args: Vec::new(),
        };

        assert!(display_argv(&plan).contains("\\xFF"));
    }
}
