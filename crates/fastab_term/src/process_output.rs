//! Bounded RunProcess output collection. The caller owns the task, timeout,
//! generation checks and command's kill-on-drop policy.

use std::io;
use std::process::Stdio;

use fastab_proto::fig::RunProcessResponse;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const MAX_PIPE_BYTES: usize = 256 * 1024;

async fn read_prefix(mut pipe: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut prefix = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = match pipe.read(&mut chunk).await {
            Ok(0) => return Ok(prefix),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let keep = count.min(MAX_PIPE_BYTES - prefix.len());
        prefix.extend_from_slice(&chunk[..keep]);
        // Keep draining after the prefix fills so neither pipe blocks exit.
    }
}

/// Spawn and concurrently drain both pipes, retaining their raw-byte prefixes.
/// Dropping this future drops the Child, preserving its kill-on-drop setting.
pub async fn run(command: &mut Command) -> io::Result<RunProcessResponse> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("missing stdout pipe"))?;
    let stderr = child.stderr.take().ok_or_else(|| io::Error::other("missing stderr pipe"))?;
    let (status, stdout, stderr) = tokio::try_join!(child.wait(), read_prefix(stdout), read_prefix(stderr))?;
    Ok(RunProcessResponse {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        // Keep RunProcess's existing signal-exit convention (unlike the
        // completion engine's execute_full, which deliberately returns -1).
        exit_code: status.code().unwrap_or(0),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use super::*;

    async fn shell(script: &str) -> RunProcessResponse {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]).kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(10), run(&mut command))
            .await
            .expect("bounded collector must finish")
            .expect("child output")
    }

    #[tokio::test]
    async fn run_process_drains_large_pipes_and_keeps_both_prefixes() {
        let output = shell(
            "(printf stdout-prefix; dd if=/dev/zero bs=65536 count=8 2>/dev/null) & \
             (printf stderr-prefix; dd if=/dev/zero bs=65536 count=8 2>/dev/null) >&2; wait; exit 23",
        )
        .await;
        assert_eq!(output.stdout.len(), MAX_PIPE_BYTES);
        assert_eq!(output.stderr.len(), MAX_PIPE_BYTES);
        assert!(output.stdout.starts_with("stdout-prefix"));
        assert!(output.stderr.starts_with("stderr-prefix"));
        assert!(output.stdout[13..].bytes().all(|byte| byte == 0));
        assert!(output.stderr[13..].bytes().all(|byte| byte == 0));
        assert_eq!(output.exit_code, 23);
    }

    #[tokio::test]
    async fn run_process_truncates_raw_bytes_before_lossy_decoding() {
        let output = shell(
            "dd if=/dev/zero bs=262143 count=1 2>/dev/null; printf '\\342\\202\\254'; exit 0",
        )
        .await;
        assert_eq!(output.stdout.len(), MAX_PIPE_BYTES + 2);
        assert!(output.stdout.ends_with('\u{fffd}'));
        assert!(output.stderr.is_empty());
        assert_eq!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn run_process_preserves_signal_exit_code() {
        let output = shell("printf before-signal; kill -KILL $$").await;
        assert_eq!(output.stdout, "before-signal");
        assert!(output.stderr.is_empty());
        assert_eq!(output.exit_code, 0);
    }
}
