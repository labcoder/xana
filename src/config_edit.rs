//! Validated, race-aware manual configuration editing.

use crate::{
    bounded_file,
    config::{ConfigTransactionLock, XanaConfig},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::Command,
};

const MAX_EDIT_BYTES: usize = 1024 * 1024;

pub(crate) fn edit(path: &Path, editor: &Path) -> Result<()> {
    edit_with(path, &editor.display().to_string(), |draft| {
        Command::new(editor).arg(draft).status()
    })
}

fn edit_with(
    path: &Path,
    editor_label: &str,
    run_editor: impl FnOnce(&Path) -> io::Result<std::process::ExitStatus>,
) -> Result<()> {
    let draft = ConfigDraft::stage(path)?;
    let status = run_editor(&draft.path).with_context(|| {
        format!(
            "could not launch editor {}; draft retained at {}",
            editor_label,
            draft.path.display()
        )
    })?;
    if !status.success() {
        bail!(
            "editor {} exited with {status}; live configuration is unchanged and the draft is retained at {}",
            editor_label,
            draft.path.display()
        );
    }
    draft.commit()
}

struct ConfigDraft {
    live_path: PathBuf,
    path: PathBuf,
    original: Vec<u8>,
}

impl ConfigDraft {
    fn stage(live_path: &Path) -> Result<Self> {
        let original = read_bounded(live_path).with_context(|| {
            format!(
                "could not stage configuration from {}; run xana setup if it is missing",
                live_path.display()
            )
        })?;
        let parent = live_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .context("configuration path has no parent directory")?;
        let path = parent.join(format!(
            ".config.edit.{}.{}.toml",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!("could not create configuration draft at {}", path.display())
            })?;
        protect_open_file(&file)?;
        file.write_all(&original).with_context(|| {
            format!("could not write configuration draft at {}", path.display())
        })?;
        file.sync_all()
            .with_context(|| format!("could not sync configuration draft at {}", path.display()))?;
        Ok(Self {
            live_path: live_path.to_owned(),
            path,
            original,
        })
    }

    fn commit(self) -> Result<()> {
        let edited = read_bounded(&self.path).with_context(|| {
            format!(
                "edited configuration is unreadable or oversized; live configuration is unchanged and the draft is retained at {}",
                self.path.display()
            )
        })?;
        let edited_text = std::str::from_utf8(&edited).with_context(|| {
            format!(
                "edited configuration is not UTF-8; live configuration is unchanged and the draft is retained at {}",
                self.path.display()
            )
        })?;
        XanaConfig::parse(edited_text).with_context(|| {
            format!(
                "edited configuration is invalid; live configuration is unchanged and the draft is retained at {}",
                self.path.display()
            )
        })?;
        let _lock = ConfigTransactionLock::acquire(&self.live_path)
            .context("could not lock configuration for the final edit transaction")?;
        let current = read_bounded(&self.live_path).with_context(|| {
            format!(
                "live configuration changed or became unreadable; draft retained at {}",
                self.path.display()
            )
        })?;
        if current != self.original {
            bail!(
                "live configuration changed while the editor was open; refusing to overwrite it and retaining the draft at {}",
                self.path.display()
            );
        }

        let backup = self.live_path.with_extension("toml.bak");
        let previous_backup = read_optional_bounded(&backup)?;
        atomic_write(&backup, &current).with_context(|| {
            format!(
                "could not update configuration backup; live configuration is unchanged and the draft is retained at {}",
                self.path.display()
            )
        })?;
        if let Err(error) = atomic_write(&self.live_path, &edited) {
            restore_optional(&backup, previous_backup.as_deref())?;
            return Err(error).with_context(|| {
                format!(
                    "could not install edited configuration; live configuration is unchanged and the draft is retained at {}",
                    self.path.display()
                )
            });
        }
        let _ = fs::remove_file(&self.path);
        Ok(())
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    bounded_file::read(path, MAX_EDIT_BYTES).map_err(anyhow::Error::new)
}

fn read_optional_bounded(path: &Path) -> Result<Option<Vec<u8>>> {
    match bounded_file::read(path, MAX_EDIT_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(bounded_file::BoundedReadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(anyhow::Error::new(error)),
    }
}

fn restore_optional(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("could not restore {}", path.display()))
            }
        },
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .with_context(|| format!("could not create atomic write for {}", path.display()))?;
    protect_open_file(file.as_file())?;
    file.write_all(bytes)
        .with_context(|| format!("could not write {}", path.display()))?;
    file.commit()
        .with_context(|| format!("could not commit {}", path.display()))
}

#[cfg(unix)]
fn protect_open_file(file: &fs::File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn protect_open_file(_file: &fs::File) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InitialConfig, InitialConnection, PermissionMode};
    use tempfile::tempdir;

    fn valid_config(model: &str) -> String {
        XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
            },
            model: model.into(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
            shell: crate::shell::ShellConfig::default(),
            max_tool_rounds: 8,
        })
        .unwrap()
    }

    #[test]
    fn valid_edit_installs_atomically_with_exact_backup() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = valid_config("before");
        fs::write(&path, &original).unwrap();
        let draft = ConfigDraft::stage(&path).unwrap();
        fs::write(&draft.path, valid_config("after")).unwrap();

        draft.commit().unwrap();

        assert!(fs::read_to_string(&path).unwrap().contains("after"));
        assert_eq!(
            fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            original
        );
    }

    fn exit_status(success: bool) -> std::process::ExitStatus {
        #[cfg(windows)]
        let status = Command::new("cmd.exe")
            .args(["/C", if success { "exit 0" } else { "exit 1" }])
            .status();
        #[cfg(not(windows))]
        let status = Command::new("sh")
            .args(["-c", if success { "exit 0" } else { "exit 1" }])
            .status();
        status.unwrap()
    }

    #[test]
    fn editor_success_and_cancellation_have_transactional_outcomes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, valid_config("before")).unwrap();
        edit_with(&path, "fake-editor", |draft| {
            fs::write(draft, valid_config("after"))?;
            Ok(exit_status(true))
        })
        .unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("after"));

        let current = fs::read(&path).unwrap();
        let error = edit_with(&path, "fake-editor", |_draft| Ok(exit_status(false)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("draft is retained"));
        assert_eq!(fs::read(&path).unwrap(), current);
        assert!(fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".config.edit.")
        }));
    }

    #[test]
    fn invalid_edit_retains_draft_and_preserves_live_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = valid_config("before");
        fs::write(&path, &original).unwrap();
        let draft = ConfigDraft::stage(&path).unwrap();
        let draft_path = draft.path.clone();
        fs::write(&draft_path, "api_key = \"seeded-secret\"\n").unwrap();

        let error = draft.commit().unwrap_err().to_string();

        assert!(error.contains("draft is retained"));
        assert!(!error.contains("seeded-secret"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(draft_path.is_file());
    }

    #[test]
    fn concurrent_live_change_is_never_overwritten() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, valid_config("before")).unwrap();
        let draft = ConfigDraft::stage(&path).unwrap();
        let concurrent = valid_config("concurrent");
        fs::write(&path, &concurrent).unwrap();
        fs::write(&draft.path, valid_config("edited")).unwrap();

        assert!(
            draft
                .commit()
                .unwrap_err()
                .to_string()
                .contains("changed while")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), concurrent);
    }

    #[test]
    fn oversized_edit_is_rejected_without_replacing_live_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = valid_config("before");
        fs::write(&path, &original).unwrap();
        let draft = ConfigDraft::stage(&path).unwrap();
        fs::write(&draft.path, vec![b'x'; MAX_EDIT_BYTES + 1]).unwrap();

        assert!(draft.commit().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
}
