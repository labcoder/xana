//! Per-user login entries. No launchctl/systemctl call starts work during setup.
use super::Registration;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub(super) fn change(registration: &Registration, enable: bool) -> Result<()> {
    let base = directories::BaseDirs::new().context("per-user startup directory unavailable")?;
    #[cfg(target_os = "macos")]
    let (directory, extension, bytes) = (
        base.home_dir().join("Library/LaunchAgents"),
        "plist",
        plist(registration)?,
    );
    #[cfg(target_os = "linux")]
    let (directory, extension, bytes) = (
        base.config_dir().join("autostart"),
        "desktop",
        desktop_entry(registration)?,
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (registration, enable, base);
        anyhow::bail!("per-user startup is unsupported on this Unix platform");
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        write_entry(
            &directory,
            &format!("{}.{}", registration.name, extension),
            &bytes,
            enable,
        )
    }
}

fn write_entry(directory: &Path, name: &str, bytes: &[u8], enable: bool) -> Result<()> {
    ensure!(bytes.len() <= 32768, "startup entry exceeds its bound");
    if enable {
        fs::create_dir_all(directory)?;
    }
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "startup entry is not a regular file"
            );
            let identity = same_file::Handle::from_path(&path)?;
            ensure!(
                crate::bounded_file::read(&path, 32768)? == bytes,
                "startup entry differs; it was not overwritten or removed"
            );
            ensure!(
                same_file::Handle::from_path(&path)? == identity,
                "startup entry changed during inspection"
            );
            if !enable {
                fs::remove_file(&path)?;
            }
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !enable {
                return Ok(());
            }
        }
        Err(error) => return Err(error.into()),
    }
    let mut file = crate::storage::create_private_file(&path)?;
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        ensure!(
            fs::symlink_metadata(&path)?.file_type().is_file()
                && same_file::Handle::from_path(&path)? == identity,
            "startup entry was replaced during writing"
        );
        Ok(())
    })();
    if result.is_err()
        && same_file::Handle::from_path(&path).is_ok_and(|current| current == identity)
    {
        let _ = fs::remove_file(&path);
    }
    result
}

#[cfg(any(target_os = "linux", test))]
fn desktop_entry(registration: &Registration) -> Result<Vec<u8>> {
    ensure!(
        !registration.arguments[0].contains('='),
        "desktop startup executable contains an unsupported '='"
    );
    let arguments = registration
        .arguments
        .iter()
        .map(|argument| {
            ensure!(
                !argument.contains('%'),
                "desktop startup paths containing '%' are unsupported"
            );
            let mut value = String::from("\"");
            for ch in argument.chars() {
                match ch {
                    '\\' => value.push_str("\\\\\\\\"),
                    '"' | '$' | '`' => {
                        value.push_str("\\\\");
                        value.push(ch);
                    }
                    _ => value.push(ch),
                }
            }
            value.push('"');
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!("[Desktop Entry]\nType=Application\nName=Xana background host\nTerminal=false\nExec={arguments}\n").into_bytes())
}

#[cfg(any(target_os = "macos", test))]
fn plist(registration: &Registration) -> Result<Vec<u8>> {
    fn xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
    let arguments = registration
        .arguments
        .iter()
        .map(|argument| format!("<string>{}</string>", xml(argument)))
        .collect::<String>();
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{arguments}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>\n",xml(&registration.name)).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_registration_is_create_only_and_only_removes_its_exact_entry() {
        let root = tempfile::tempdir().unwrap();
        write_entry(root.path(), "fixture.desktop", b"owned", true).unwrap();
        assert!(write_entry(root.path(), "fixture.desktop", b"different", true).is_err());
        assert!(write_entry(root.path(), "fixture.desktop", b"different", false).is_err());
        write_entry(root.path(), "fixture.desktop", b"owned", false).unwrap();
        assert!(!root.path().join("fixture.desktop").exists());
    }
}
