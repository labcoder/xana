//! Qualified native launch owns an exact suspended process and its descendants.
//! No profile attachment, handle inheritance, browser download or shell command.

use super::BrowserError;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(target_os = "windows")]
mod windows;

pub(super) struct OwnedBrowser {
    #[cfg(target_os = "windows")]
    inner: windows::BrowserProcess,
    profile: PathBuf,
    identity: same_file::Handle,
}

impl OwnedBrowser {
    #[cfg(test)]
    pub(super) fn invalidate_cleanup_identity_for_fixture(&mut self, other: &Path) {
        self.identity = same_file::Handle::from_path(other).expect("fixture identity");
    }
    #[cfg(all(test, windows))]
    pub(super) fn metrics(&self) -> Result<serde_json::Value, BrowserError> {
        self.inner.metrics()
    }
    pub(super) fn discover() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        for candidate in [
            "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
            "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
        ] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }

    pub(super) fn launch(
        executable: &Path,
        profile: PathBuf,
        proxy: std::net::SocketAddr,
        headless: bool,
    ) -> Result<Self, BrowserError> {
        Self::launch_options(executable, profile, proxy, headless, &[])
    }

    #[cfg(test)]
    pub(super) fn launch_fixture(
        executable: &Path,
        profile: PathBuf,
        proxy: std::net::SocketAddr,
        headless: bool,
        spki: &str,
    ) -> Result<Self, BrowserError> {
        if spki.len() != 44
            || !spki
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"+/=".contains(&v))
        {
            return Err(BrowserError::InvalidInput);
        }
        Self::launch_options(
            executable,
            profile,
            proxy,
            headless,
            &[format!("--ignore-certificate-errors-spki-list={spki}")],
        )
    }

    fn launch_options(
        executable: &Path,
        profile: PathBuf,
        proxy: std::net::SocketAddr,
        headless: bool,
        fixture_arguments: &[String],
    ) -> Result<Self, BrowserError> {
        #[cfg(target_os = "windows")]
        {
            let profile = profile.canonicalize().map_err(|_| BrowserError::Process)?;
            let identity =
                same_file::Handle::from_path(&profile).map_err(|_| BrowserError::Process)?;
            let mut arguments = vec![
                format!("--user-data-dir={}", profile.display()),
                format!("--proxy-server=http://{proxy}"),
                "--proxy-bypass-list=<-loopback>".into(),
                "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".into(),
                "--disable-quic".into(),
                "--webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
                "--remote-debugging-port=0".into(),
                "--no-first-run".into(),
                "--no-default-browser-check".into(),
                "--disable-background-networking".into(),
                "--disable-component-update".into(),
                "--disable-sync".into(),
                "--disable-extensions".into(),
                "--disable-popup-blocking".into(),
                "about:blank".into(),
            ];
            if headless {
                arguments.push("--headless=new".into());
            }
            arguments.extend_from_slice(fixture_arguments);
            let inner = windows::BrowserProcess::launch(executable, &arguments, &profile)?;
            Ok(Self {
                inner,
                profile,
                identity,
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (executable, profile, proxy, headless, fixture_arguments);
            Err(BrowserError::Unavailable)
        }
    }

    pub(super) async fn endpoint(&self) -> Result<String, BrowserError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let file = self.profile.join("DevToolsActivePort");
                #[cfg(target_os = "windows")]
                if self.inner.exited() {
                    return Err(BrowserError::Process);
                }
                if let Ok(bytes) = crate::bounded_file::read(&file, 4096) {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        let mut lines = text.lines();
                        if let (Some(port), Some(path)) = (
                            lines.next().and_then(|v| v.parse::<u16>().ok()),
                            lines.next(),
                        ) {
                            if port != 0
                                && path.starts_with("/devtools/browser/")
                                && path.len() < 256
                                && path
                                    .bytes()
                                    .all(|c| c.is_ascii_alphanumeric() || b"/-".contains(&c))
                            {
                                return Ok(format!("ws://127.0.0.1:{port}{path}"));
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| BrowserError::TimedOut)?
    }

    pub(super) async fn close(mut self) -> Result<(), BrowserError> {
        #[cfg(target_os = "windows")]
        self.inner.terminate();
        #[cfg(not(target_os = "windows"))]
        let _ = &mut self;
        tokio::task::spawn_blocking(move || {
            #[cfg(target_os = "windows")]
            self.inner.wait_empty()?;
            let path = &self.profile;
            // Only remove the exact fresh directory allocated by this owner.
            // Never follow a replacement symlink or recurse through a broad root.
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| BrowserError::Process)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(BrowserError::Process);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(BrowserError::Process);
                }
            }
            if path
                .file_name()
                .and_then(|v| v.to_str())
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
                .is_none()
                || path.canonicalize().map_err(|_| BrowserError::Process)? != *path
                || same_file::Handle::from_path(path).map_err(|_| BrowserError::Process)?
                    != self.identity
            {
                return Err(BrowserError::Process);
            }
            for _ in 0..20 {
                if std::fs::remove_dir_all(path).is_ok() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(BrowserError::Process)
        })
        .await
        .map_err(|_| BrowserError::Process)?
    }
}
