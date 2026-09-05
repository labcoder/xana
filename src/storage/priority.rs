//! Process-shared admission leases. Foreground work never waits for maintenance;
//! background owners observe it and cancel through their structured worker.
use super::ProtectedStore;
use anyhow::Result;
use std::{
    fs::{File, OpenOptions},
    time::Duration,
};

pub(crate) struct ForegroundLease(File);
pub(crate) struct BackgroundLease {
    _job: File,
    foreground: File,
}
/// A foreground helper shares the same provider-work lane as background jobs.
pub(crate) struct ForegroundJobLease {
    _foreground: ForegroundLease,
    job: File,
}
impl ProtectedStore {
    pub(crate) async fn foreground_job_lease(
        &self,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<ForegroundJobLease> {
        let foreground = self.foreground_lease()?;
        let job = self.priority_file("background.lease")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            anyhow::ensure!(
                !cancellation.is_cancelled(),
                "foreground helper admission cancelled"
            );
            match fs2::FileExt::try_lock_exclusive(&job) {
                Ok(()) => {
                    return Ok(ForegroundJobLease {
                        _foreground: foreground,
                        job,
                    });
                }
                Err(error) if contended(&error) => {}
                Err(error) => return Err(error.into()),
            }
            tokio::select! {
                biased;
                _=cancellation.cancelled()=>anyhow::bail!("foreground helper admission cancelled"),
                _=tokio::time::sleep_until(deadline)=>anyhow::bail!("foreground helper is waiting for previous model work to settle"),
                _=tokio::time::sleep(Duration::from_millis(20))=>{},
            }
        }
    }
    fn priority_file(&self, name: &str) -> Result<File> {
        self.with_database(|_| Ok(()))?;
        let path = self.inner.root.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        anyhow::ensure!(
            std::fs::symlink_metadata(&path)?.file_type().is_file(),
            "priority lease is not a regular file"
        );
        Ok(file)
    }
    pub(crate) fn foreground_lease(&self) -> Result<ForegroundLease> {
        let file = self.priority_file("foreground.lease")?;
        fs2::FileExt::lock_shared(&file)?;
        Ok(ForegroundLease(file))
    }
    pub(crate) fn background_lease(&self) -> Result<Option<BackgroundLease>> {
        let job = self.priority_file("background.lease")?;
        match fs2::FileExt::try_lock_exclusive(&job) {
            Ok(()) => {}
            Err(error) if contended(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let lease = BackgroundLease {
            _job: job,
            foreground: self.priority_file("foreground.lease")?,
        };
        if lease.foreground_active()? {
            return Ok(None);
        }
        Ok(Some(lease))
    }
}
impl BackgroundLease {
    pub(crate) fn foreground_active(&self) -> Result<bool> {
        match fs2::FileExt::try_lock_exclusive(&self.foreground) {
            Ok(()) => {
                fs2::FileExt::unlock(&self.foreground)?;
                Ok(false)
            }
            Err(error) if contended(&error) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) async fn preempted(&self) -> Result<()> {
        loop {
            if self.foreground_active()? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for ForegroundLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

// fs2 exposes each platform's lock-conflict error; Windows ERROR_LOCK_VIOLATION
// is not necessarily classified as WouldBlock by std::io.
fn contended(error: &std::io::Error) -> bool {
    error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
        || error.kind() == std::io::ErrorKind::WouldBlock
}
impl Drop for BackgroundLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self._job);
    }
}
impl Drop for ForegroundJobLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn foreground_does_not_wait_and_background_lanes_are_exclusive() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let background = store.background_lease().unwrap().unwrap();
        assert!(store.background_lease().unwrap().is_none());
        let foreground = store.foreground_lease().unwrap();
        tokio::time::timeout(Duration::from_secs(1), background.preempted())
            .await
            .unwrap()
            .unwrap();
        drop(background);
        assert!(store.background_lease().unwrap().is_none());
        drop(foreground);
        assert!(store.background_lease().unwrap().is_some());
    }

    #[tokio::test]
    async fn foreground_helper_signals_preemption_but_waits_for_the_shared_lane() {
        let home = tempfile::tempdir().unwrap();
        let store = ProtectedStore::initialize(
            home.path(),
            &crate::storage::RecoveryIdentity::generate(),
            &crate::storage::TestCustody::default(),
        )
        .unwrap();
        let background = store.background_lease().unwrap().unwrap();
        let waiting = tokio::spawn({
            let store = store.clone();
            async move {
                store
                    .foreground_job_lease(&tokio_util::sync::CancellationToken::new())
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), background.preempted())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !waiting.is_finished(),
            "preemption must settle before helper dispatch"
        );
        drop(background);
        let foreground = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(store.background_lease().unwrap().is_none());
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        assert!(store.foreground_job_lease(&cancelled).await.is_err());
        drop(foreground);
        assert!(store.background_lease().unwrap().is_some());
    }
}
