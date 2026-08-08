//! Constant-memory child-process output capture.
//!
//! Both pipes are drained concurrently so a noisy child cannot deadlock on a
//! full pipe. Bytes beyond each independent limit are discarded, not buffered.

use std::{io, process::ExitStatus, process::Stdio};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

pub(crate) struct BoundedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
}

pub(crate) async fn run(
    command: &mut Command,
    per_stream_limit: usize,
) -> io::Result<BoundedOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr was not piped"))?;

    let (stdout, stderr, status) = tokio::try_join!(
        collect(stdout, per_stream_limit),
        collect(stderr, per_stream_limit),
        child.wait()
    )?;
    Ok(BoundedOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

struct CollectedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn collect(mut stream: impl AsyncRead + Unpin, limit: usize) -> io::Result<CollectedStream> {
    let mut bytes = Vec::with_capacity(limit);
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let retained = read.min(limit.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
    }
    Ok(CollectedStream { bytes, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn collector_drains_after_reaching_its_memory_limit() {
        let (mut writer, reader) = tokio::io::duplex(32);
        let producer = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; 128 * 1024])
                .await
                .expect("write fixture");
        });
        let collected = collect(reader, 1024).await.expect("collect fixture");
        producer.await.expect("producer task");

        assert_eq!(collected.bytes.len(), 1024);
        assert!(collected.truncated);
    }
}
