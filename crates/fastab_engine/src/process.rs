//! Timed subprocess helper for generators. Never run this on the UI thread.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

const MAX_STDOUT: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunResult {
    Output(String),
    TimedOut,
    Failed,
}

pub fn execute(command: &str, args: &[String], cwd: &str, timeout: Duration) -> String {
    output_or_empty(run(command, args, cwd, timeout, false, false))
}

/// Full executeCommand host result. Timeout and spawn failure are `Err` so the
/// JS host can throw the same way the old WebView wrapper did. A non-zero
/// exit status is still `Ok`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    TimedOut,
    Failed,
}

pub fn execute_full(
    command: &str,
    args: &[String],
    cwd: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    if command.is_empty() {
        return Err(CommandError::Failed);
    }
    #[cfg(test)]
    if let Some(result) = mock::intercept_full(command, args, timeout) {
        return result;
    }
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|_spawn| CommandError::Failed)?;
    wait_child_output(child, timeout)
}

#[cfg(unix)]
fn wait_child_output(mut child: Child, timeout: Duration) -> Result<CommandOutput, CommandError> {
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    let pid = child.id();
    let started = Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let result = (|| {
        let mut stdout = stdout.ok_or(CommandError::Failed)?;
        let mut stderr = stderr.ok_or(CommandError::Failed)?;
        let stdout_fd = stdout.as_raw_fd();
        let stderr_fd = stderr.as_raw_fd();
        for fd in [stdout_fd, stderr_fd] {
            // SAFETY: both descriptors are pipes owned for this entire scope.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
            if flags < 0 {
                return Err(CommandError::Failed);
            }
            // SAFETY: setting nonblocking mode preserves the descriptor's other flags.
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(CommandError::Failed);
            }
        }

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        loop {
            if !(stdout_eof && stderr_eof) && started.elapsed() >= timeout {
                return Err(CommandError::TimedOut);
            }
            if !stdout_eof {
                stdout_eof = drain_full_pipe(&mut stdout, &mut tmp, &mut stdout_buf, started, timeout)?;
            }
            if !stderr_eof {
                stderr_eof = drain_full_pipe(&mut stderr, &mut tmp, &mut stderr_buf, started, timeout)?;
            }
            // Do not reap the leader while a descendant can still hold a pipe.
            // Keeping its PID reserved makes timeout/error group cleanup safe.
            // Once both pipes close, a reaped status is returned immediately;
            // no later code signals that PID or process group.
            // Check status before timing out: the leader may have exited while
            // poll slept with both pipe descriptors disabled below.
            if stdout_eof && stderr_eof {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Ok(CommandOutput {
                            status: status.code().unwrap_or(-1),
                            stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
                            stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
                        });
                    },
                    Ok(None) => {},
                    Err(err) if err.kind() == ErrorKind::Interrupted => {},
                    Err(_) => return Err(CommandError::Failed),
                }
            }

            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(CommandError::TimedOut);
            }
            let mut pollfds = [
                libc::pollfd {
                    fd: if stdout_eof { -1 } else { stdout_fd },
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if stderr_eof { -1 } else { stderr_fd },
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                },
            ];
            // Negative fds disable closed pipes, including their persistent HUP.
            // With two EOFs this is a bounded sleep before the next exit check.
            let ms = remaining.as_millis().clamp(1, 20) as i32;
            // SAFETY: pollfds contains two valid, owned descriptors or -1.
            let ready = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, ms) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(CommandError::Failed);
            }
            if pollfds.iter().any(|fd| fd.revents & (libc::POLLNVAL | libc::POLLERR) != 0) {
                return Err(CommandError::Failed);
            }
        }
    })();

    // The closure has dropped both pipes before cleanup. No reader thread can
    // remain blocked on a descendant that escaped the original process group.
    if result.is_err() {
        kill_process_group(pid);
        // Also stop the leader if it moved out of the original process group.
        let _ = child.kill();
        // SIGKILL need not take effect immediately (for example during an
        // uninterruptible kernel wait). Never turn a bounded command failure
        // into an unbounded wait for reaping, including repeated EINTR.
        let cleanup_started = Instant::now();
        poll_exit_until(
            Duration::from_secs(1),
            || child.try_wait().map(|status| status.is_some()),
            || cleanup_started.elapsed(),
            std::thread::sleep,
        );
    }
    result
}

#[cfg(unix)]
fn poll_exit_until(
    timeout: Duration,
    mut try_wait: impl FnMut() -> std::io::Result<bool>,
    mut elapsed: impl FnMut() -> Duration,
    mut pause: impl FnMut(Duration),
) {
    loop {
        if elapsed() >= timeout {
            return;
        }
        match try_wait() {
            Ok(true) => return,
            Ok(false) => {},
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(_wait_error) => return,
        }
        let remaining = timeout.saturating_sub(elapsed());
        if remaining.is_zero() {
            return;
        }
        pause(remaining.min(Duration::from_millis(20)));
    }
}

#[cfg(unix)]
fn drain_full_pipe(
    pipe: &mut impl std::io::Read,
    tmp: &mut [u8; 4096],
    buf: &mut Vec<u8>,
    started: std::time::Instant,
    timeout: Duration,
) -> Result<bool, CommandError> {
    use std::io::ErrorKind;

    // At most 64 KiB per pipe per turn, even after its saved prefix is full.
    // Continuous stdout must not starve stderr or the deadline checks.
    for _ in 0..16 {
        if started.elapsed() >= timeout {
            return Err(CommandError::TimedOut);
        }
        match pipe.read(tmp) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                let keep = n.min(MAX_STDOUT - buf.len());
                buf.extend_from_slice(&tmp[..keep]);
            },
            Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::Interrupted => {
                return Ok(false);
            },
            Err(_) => return Err(CommandError::Failed),
        }
    }
    Ok(false)
}

#[cfg(not(unix))]
fn wait_child_output(child: Child, timeout: Duration) -> Result<CommandOutput, CommandError> {
    use std::sync::mpsc;
    use std::thread;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            let status = output.status.code().unwrap_or(-1);
            let mut stdout = output.stdout;
            stdout.truncate(MAX_STDOUT);
            let mut stderr = output.stderr;
            stderr.truncate(MAX_STDOUT);
            Ok(CommandOutput {
                status,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        },
        Ok(Err(_)) => Err(CommandError::Failed),
        Err(_) => {
            kill_process_group(pid);
            Err(CommandError::TimedOut)
        },
    }
}

/// [`execute`] that yields `None` on timeout or spawn failure so callers do not cache empties.
pub fn try_execute(command: &str, args: &[String], cwd: &str, timeout: Duration) -> Option<String> {
    match run(command, args, cwd, timeout, false, false) {
        RunResult::Output(stdout) => Some(stdout),
        RunResult::TimedOut | RunResult::Failed => None,
    }
}

/// Isolated session so the child cannot steal the TTY. `None` on timeout or spawn failure.
pub fn try_execute_isolated(command: &str, args: &[String], cwd: &str, timeout: Duration) -> Option<String> {
    match run(command, args, cwd, timeout, true, false) {
        RunResult::Output(stdout) => Some(stdout),
        RunResult::TimedOut | RunResult::Failed => None,
    }
}

/// Isolated execution that also treats a non-zero exit status as failure.
/// History commands use this contract so a broken custom source falls back
/// to the database even if it printed diagnostics on stdout first.
pub fn try_execute_isolated_success(command: &str, args: &[String], cwd: &str, timeout: Duration) -> Option<String> {
    match run(command, args, cwd, timeout, true, true) {
        RunResult::Output(stdout) => Some(stdout),
        RunResult::TimedOut | RunResult::Failed => None,
    }
}

fn output_or_empty(result: RunResult) -> String {
    match result {
        RunResult::Output(stdout) => stdout,
        RunResult::TimedOut | RunResult::Failed => String::new(),
    }
}

fn run(
    command: &str,
    args: &[String],
    cwd: &str,
    timeout: Duration,
    isolated: bool,
    require_success: bool,
) -> RunResult {
    if command.is_empty() {
        return RunResult::Failed;
    }
    #[cfg(test)]
    if let Some(result) = mock::intercept_run(command, args, timeout, require_success) {
        return result;
    }
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if !cwd.is_empty() {
        cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if isolated {
            // SAFETY: setsid() is called in the child after fork and before exec.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        } else {
            cmd.process_group(0);
        }
    }
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return RunResult::Failed,
    };
    wait_child(child, timeout, require_success)
}

fn wait_child(child: Child, timeout: Duration, require_success: bool) -> RunResult {
    #[cfg(unix)]
    {
        wait_child_unix(child, timeout, require_success)
    }
    #[cfg(not(unix))]
    {
        wait_child_threaded(child, timeout, require_success)
    }
}

#[cfg(unix)]
fn wait_child_unix(mut child: Child, timeout: Duration, require_success: bool) -> RunResult {
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd;

    let pid = child.id();
    let Some(mut stdout) = child.stdout.take() else {
        kill_and_reap(&mut child, pid);
        return RunResult::Failed;
    };
    let fd = stdout.as_raw_fd();
    // SAFETY: fd is the child's stdout pipe we still own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let deadline = std::time::Instant::now() + timeout;
    let mut stdout_eof = false;

    loop {
        if !stdout_eof {
            if drain_stdout(&mut stdout, &mut tmp, &mut buf) {
                stdout_eof = true;
            }
            if buf.len() >= MAX_STDOUT {
                kill_and_reap(&mut child, pid);
                buf.truncate(MAX_STDOUT);
                return RunResult::Output(String::from_utf8_lossy(&buf).into_owned());
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if !stdout_eof {
                    stdout_eof = drain_stdout(&mut stdout, &mut tmp, &mut buf);
                }
                // A background descendant can outlive the command leader and
                // keep its stdout pipe open. Do not leak that process group or
                // let a later reader wait forever for EOF.
                if !stdout_eof {
                    kill_process_group(pid);
                }
                if require_success && !status.success() {
                    return RunResult::Failed;
                }
                return RunResult::Output(String::from_utf8_lossy(&buf).into_owned());
            },
            Ok(None) => {},
            Err(_) => {
                kill_and_reap(&mut child, pid);
                return RunResult::Failed;
            },
        }

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            kill_and_reap(&mut child, pid);
            // Pipe already closed: stdout is complete even if the process is hung.
            if stdout_eof && !require_success {
                return RunResult::Output(String::from_utf8_lossy(&buf).into_owned());
            }
            return RunResult::TimedOut;
        }

        if stdout_eof {
            std::thread::sleep(remaining.min(Duration::from_millis(20)));
            continue;
        }

        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let ms = i32::try_from(remaining.as_millis().min(20)).unwrap_or(20);
        // SAFETY: pollfd.fd is the stdout pipe still owned by `stdout`.
        let n = unsafe { libc::poll(&mut pollfd, 1, ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            kill_and_reap(&mut child, pid);
            return RunResult::Failed;
        }
        if n == 0 {
            continue;
        }
        if pollfd.revents & libc::POLLNVAL != 0 {
            stdout_eof = true;
        }
    }
}

#[cfg(unix)]
fn drain_stdout(stdout: &mut impl std::io::Read, tmp: &mut [u8], buf: &mut Vec<u8>) -> bool {
    use std::io::ErrorKind;
    loop {
        if buf.len() >= MAX_STDOUT {
            return false;
        }
        match stdout.read(tmp) {
            Ok(0) => return true,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::Interrupted => {
                return false;
            },
            Err(_) => return true,
        }
    }
}

#[cfg(not(unix))]
fn wait_child_threaded(child: Child, timeout: Duration, require_success: bool) -> RunResult {
    use std::sync::mpsc;
    use std::thread;

    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            if require_success && !output.status.success() {
                return RunResult::Failed;
            }
            let mut stdout = output.stdout;
            stdout.truncate(MAX_STDOUT);
            RunResult::Output(String::from_utf8_lossy(&stdout).into_owned())
        },
        Ok(Err(_)) => RunResult::Failed,
        Err(_) => {
            kill_process_group(pid);
            RunResult::TimedOut
        },
    }
}

fn reap(child: &mut Child) {
    let _ = child.wait();
}

fn kill_and_reap(child: &mut Child, pid: u32) {
    kill_process_group(pid);
    reap(child);
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        let pgid = pid as i32;
        // SAFETY: the child was started with process_group(0) or setsid(), so its
        // pgid equals pid. Negative pgid sends the signal to the whole group.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::{CommandError, CommandOutput, RunResult};
    use std::cell::RefCell;
    use std::time::Duration;

    #[derive(Debug, Clone, Default)]
    pub struct ExecRule {
        pub command: Option<String>,
        pub args: Option<Vec<String>>,
        pub stdout: String,
        pub stderr: String,
        pub status: i32,
        pub delay_ms: Option<u64>,
    }

    thread_local! {
        static ACTIVE: RefCell<bool> = const { RefCell::new(false) };
        static RULES: RefCell<Vec<ExecRule>> = const { RefCell::new(Vec::new()) };
        static CALLS: RefCell<Vec<(String, Vec<String>)>> = const { RefCell::new(Vec::new()) };
    }

    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            clear();
        }
    }

    pub fn install(rules: Vec<ExecRule>) -> Guard {
        ACTIVE.with(|cell| *cell.borrow_mut() = true);
        RULES.with(|cell| *cell.borrow_mut() = rules);
        CALLS.with(|cell| cell.borrow_mut().clear());
        Guard
    }

    pub fn clear() {
        ACTIVE.with(|cell| *cell.borrow_mut() = false);
        RULES.with(|cell| cell.borrow_mut().clear());
        CALLS.with(|cell| cell.borrow_mut().clear());
    }

    pub fn calls() -> Vec<(String, Vec<String>)> {
        CALLS.with(|cell| cell.borrow().clone())
    }

    /// Swap the rule table without clearing the recorded call log. Engine
    /// golden cases use this between two `complete` calls so a TTL refetch
    /// can return different stdout while `expectSecondCalls` still counts.
    pub fn replace_rules(rules: Vec<ExecRule>) {
        RULES.with(|cell| *cell.borrow_mut() = rules);
    }

    fn is_active() -> bool {
        ACTIVE.with(|cell| *cell.borrow())
    }

    fn record(command: &str, args: &[String]) {
        CALLS.with(|cell| cell.borrow_mut().push((command.to_string(), args.to_vec())));
    }

    fn matching_rule(command: &str, args: &[String]) -> Option<ExecRule> {
        RULES.with(|cell| {
            cell.borrow()
                .iter()
                .find(|rule| {
                    rule.command.as_deref().is_none_or(|expected| expected == command)
                        && rule.args.as_ref().is_none_or(|expected| expected == args)
                })
                .cloned()
        })
    }

    fn timed_out(rule: &ExecRule, timeout: Duration) -> bool {
        rule.delay_ms
            .is_some_and(|delay| u128::from(delay) > timeout.as_millis())
    }

    pub fn intercept_run(
        command: &str,
        args: &[String],
        timeout: Duration,
        require_success: bool,
    ) -> Option<RunResult> {
        if !is_active() {
            return None;
        }
        record(command, args);
        match matching_rule(command, args) {
            Some(rule) if timed_out(&rule, timeout) => Some(RunResult::TimedOut),
            Some(rule) if require_success && rule.status != 0 => Some(RunResult::Failed),
            Some(rule) => Some(RunResult::Output(rule.stdout)),
            None => Some(RunResult::Failed),
        }
    }

    pub fn intercept_full(
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> Option<Result<CommandOutput, CommandError>> {
        if !is_active() {
            return None;
        }
        record(command, args);
        match matching_rule(command, args) {
            Some(rule) if timed_out(&rule, timeout) => Some(Err(CommandError::TimedOut)),
            Some(rule) => Some(Ok(CommandOutput {
                status: rule.status,
                stdout: rule.stdout,
                stderr: rule.stderr,
            })),
            None => Some(Err(CommandError::Failed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn full_shell(script: &str, timeout: Duration) -> Result<CommandOutput, CommandError> {
        execute_full("/bin/sh", &["-c".into(), script.into()], "/", &[], timeout)
    }

    #[cfg(unix)]
    #[test]
    fn full_output_drains_beyond_stdout_limit_until_command_finishes() {
        let output = full_shell(
            "printf stdout-prefix; head -c 1048576 /dev/zero; printf done >&2",
            Duration::from_secs(5),
        )
        .expect("large stdout should be drained rather than killing the command");
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout.len(), MAX_STDOUT);
        assert!(output.stdout.starts_with("stdout-prefix"));
        assert!(output.stdout.as_bytes()["stdout-prefix".len()..].iter().all(|byte| *byte == 0));
        assert_eq!(output.stderr, "done");
    }

    #[cfg(unix)]
    #[test]
    fn full_output_drains_concurrent_large_pipes_and_preserves_nonzero_status() {
        let output = full_shell(
            "(printf stdout-prefix; head -c 1048576 /dev/zero) & \
             (printf stderr-prefix; head -c 1048576 /dev/zero) >&2 & wait; exit 7",
            Duration::from_secs(5),
        )
        .expect("both full pipes must be drained without deadlocking");
        assert_eq!(output.status, 7);
        for (actual, prefix) in [(&output.stdout, "stdout-prefix"), (&output.stderr, "stderr-prefix")] {
            assert_eq!(actual.len(), MAX_STDOUT);
            assert!(actual.starts_with(prefix));
            assert!(actual.as_bytes()[prefix.len()..].iter().all(|byte| *byte == 0));
        }
    }

    #[cfg(unix)]
    #[test]
    fn full_timeout_kills_and_returns_with_bounded_cleanup() {
        let started = std::time::Instant::now();
        assert_eq!(
            full_shell("exec sleep 30", Duration::from_millis(50)),
            Err(CommandError::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(3), "cleanup took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn full_failed_pipe_setup_preserves_failure_after_cleanup() {
        use std::os::unix::process::CommandExt;

        let child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn real child without capture pipes");
        let started = std::time::Instant::now();
        assert_eq!(wait_child_output(child, Duration::from_secs(5)), Err(CommandError::Failed));
        assert!(started.elapsed() < Duration::from_secs(3), "cleanup took {:?}", started.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn full_signal_exit_remains_success_with_negative_status() {
        let output = full_shell("printf signalled; kill -KILL $$", Duration::from_secs(5))
            .expect("a signal exit is still a completed command");
        assert_eq!(output.status, -1);
        assert_eq!(output.stdout, "signalled");
        assert!(output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_deadline_bounds_pending_children_and_repeated_eintr() {
        use std::cell::Cell;

        // A real SIGKILL normally exits promptly, so emulate the two cases
        // that otherwise make cleanup unbounded without relying on OS timing
        // or installing process-wide signal handlers in parallel tests.
        for interrupted in [false, true] {
            let elapsed = Cell::new(Duration::ZERO);
            let polls = Cell::new(0);
            poll_exit_until(
                Duration::from_secs(1),
                || {
                    polls.set(polls.get() + 1);
                    if interrupted {
                        Err(std::io::Error::from_raw_os_error(libc::EINTR))
                    } else {
                        Ok(false)
                    }
                },
                || elapsed.get(),
                |delay| elapsed.set(elapsed.get() + delay),
            );
            assert_eq!(elapsed.get(), Duration::from_secs(1));
            assert_eq!(polls.get(), 50);
        }
    }

    #[test]
    fn times_out_and_returns_empty() {
        let out = execute("sleep", &["2".into()], "/", Duration::from_millis(50));
        assert!(out.is_empty());
        assert_eq!(
            try_execute("sleep", &["2".into()], "/", Duration::from_millis(50)),
            None
        );
    }

    #[test]
    fn captures_stdout_without_an_extra_thread() {
        let out = execute("printf", &["hello-engine".into()], "/", Duration::from_millis(500));
        assert_eq!(out, "hello-engine");
    }

    #[test]
    fn returns_stdout_after_pipe_closes_even_if_child_hangs() {
        let started = std::time::Instant::now();
        let out = execute(
            "sh",
            &["-c".into(), "printf hello-eof; exec 1>&-; sleep 2".into()],
            "/",
            Duration::from_millis(200),
        );
        assert_eq!(out, "hello-eof");
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn strict_isolated_execution_rejects_nonzero_status() {
        assert_eq!(
            try_execute_isolated_success(
                "sh",
                &["-c".into(), "printf misleading; exit 7".into()],
                "/",
                Duration::from_millis(500),
            ),
            None
        );
    }
}
