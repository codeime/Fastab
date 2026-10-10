//! Timed subprocess helper for generators. Never run this on the UI thread.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

const MAX_STDOUT: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunResult {
    Output(String),
    TimedOut,
    Cancelled,
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
    Cancelled,
    Failed,
}

pub fn execute_full(
    command: &str,
    args: &[String],
    cwd: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<CommandOutput, CommandError> {
    crate::cancellation::check().map_err(|_cancelled| CommandError::Cancelled)?;
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
    crate::cancellation::check().map_err(|_cancelled| CommandError::Cancelled)?;
    let child = cmd.spawn().map_err(|_spawn| CommandError::Failed)?;
    let result = wait_child_output(child, timeout);
    crate::cancellation::check().map_err(|_cancelled| CommandError::Cancelled)?;
    result
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
            crate::cancellation::check().map_err(|_cancelled| CommandError::Cancelled)?;
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
            if pollfds
                .iter()
                .any(|fd| fd.revents & (libc::POLLNVAL | libc::POLLERR) != 0)
            {
                return Err(CommandError::Failed);
            }
        }
    })();

    // The closure has dropped both pipes before cleanup. No reader thread can
    // remain blocked on a descendant that escaped the original process group.
    if result.is_err() {
        kill_and_reap(&mut child, pid);
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
        crate::cancellation::check().map_err(|_cancelled| CommandError::Cancelled)?;
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
    let started = std::time::Instant::now();
    let received = loop {
        if crate::cancellation::is_cancelled() {
            kill_process_group(pid);
            return Err(CommandError::Cancelled);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        match rx.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Err(mpsc::RecvTimeoutError::Timeout) if !remaining.is_zero() => continue,
            result => break result,
        }
    };
    match received {
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
        RunResult::TimedOut | RunResult::Cancelled | RunResult::Failed => None,
    }
}

/// Isolated session so the child cannot steal the TTY. `None` on timeout or spawn failure.
pub fn try_execute_isolated(command: &str, args: &[String], cwd: &str, timeout: Duration) -> Option<String> {
    match run(command, args, cwd, timeout, true, false) {
        RunResult::Output(stdout) => Some(stdout),
        RunResult::TimedOut | RunResult::Cancelled | RunResult::Failed => None,
    }
}

/// Isolated execution that also treats a non-zero exit status as failure.
/// History commands use this contract so a broken custom source falls back
/// to the database even if it printed diagnostics on stdout first.
pub fn try_execute_isolated_success(command: &str, args: &[String], cwd: &str, timeout: Duration) -> Option<String> {
    match run(command, args, cwd, timeout, true, true) {
        RunResult::Output(stdout) => Some(stdout),
        RunResult::TimedOut | RunResult::Cancelled | RunResult::Failed => None,
    }
}

fn output_or_empty(result: RunResult) -> String {
    match result {
        RunResult::Output(stdout) => stdout,
        RunResult::TimedOut | RunResult::Cancelled | RunResult::Failed => String::new(),
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
    if crate::cancellation::is_cancelled() {
        return RunResult::Cancelled;
    }
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
    if crate::cancellation::is_cancelled() {
        return RunResult::Cancelled;
    }
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return RunResult::Failed,
    };
    let result = wait_child(child, timeout, require_success);
    if crate::cancellation::is_cancelled() {
        RunResult::Cancelled
    } else {
        result
    }
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
        if crate::cancellation::is_cancelled() {
            kill_and_reap(&mut child, pid);
            return RunResult::Cancelled;
        }
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

        match child_has_exited(pid) {
            Ok(true) => {
                if !stdout_eof {
                    stdout_eof = drain_stdout(&mut stdout, &mut tmp, &mut buf);
                }
                // A background descendant can outlive the command leader and
                // keep its stdout pipe open. Do not leak that process group or
                // let a later reader wait forever for EOF.
                if !stdout_eof {
                    kill_process_group(pid);
                }
                // waitid left the leader unreaped, reserving its PID until
                // the final group signal. Never signal this group after wait.
                let status = match child.wait() {
                    Ok(status) => status,
                    Err(_) => return RunResult::Failed,
                };
                if require_success && !status.success() {
                    return RunResult::Failed;
                }
                return RunResult::Output(String::from_utf8_lossy(&buf).into_owned());
            },
            Ok(false) => {},
            Err(error) if error.kind() == ErrorKind::Interrupted => {},
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

/// Observe an exited leader without releasing its PID for reuse. Both macOS
/// and Linux support WNOWAIT; only the following Child::wait consumes status.
#[cfg(unix)]
fn child_has_exited(pid: u32) -> std::io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: info is writable, pid names our unreaped direct child, and the
    // zeroed si_pid distinguishes WNOHANG's no-status result on older systems.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: waitid initialized info, including the child identity field.
    Ok(unsafe { info.assume_init().si_pid() } != 0)
}

#[cfg(unix)]
fn drain_stdout(stdout: &mut impl std::io::Read, tmp: &mut [u8], buf: &mut Vec<u8>) -> bool {
    use std::io::ErrorKind;
    loop {
        if crate::cancellation::is_cancelled() || buf.len() >= MAX_STDOUT {
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
    let started = std::time::Instant::now();
    let received = loop {
        if crate::cancellation::is_cancelled() {
            kill_process_group(pid);
            return RunResult::Cancelled;
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        match rx.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Err(mpsc::RecvTimeoutError::Timeout) if !remaining.is_zero() => continue,
            result => break result,
        }
    };
    match received {
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

#[cfg(not(unix))]
fn reap(child: &mut Child) {
    let _ = child.wait();
}

#[cfg(unix)]
fn reap_pid_in_background(pid: u32) {
    // SIGKILL need not take effect immediately (for example during an
    // uninterruptible kernel wait). Never turn a bounded command failure into
    // an unbounded wait on the calling thread; collect the zombie off-thread.
    let _ = std::thread::Builder::new().name("fastab-reap".into()).spawn(move || {
        let mut status = 0;
        loop {
            // SAFETY: `pid` is a direct child this process spawned and failed
            // to reap within the cleanup budget. Waiting here prevents a
            // permanent zombie without blocking the generator thread.
            let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
            if result == pid as libc::pid_t {
                return;
            }
            if result < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                // ECHILD: already reaped elsewhere.
                return;
            }
        }
    });
}

fn kill_and_reap(child: &mut Child, pid: u32) {
    kill_process_group(pid);
    let _ = child.kill();
    #[cfg(unix)]
    {
        use std::cell::Cell;
        use std::time::Instant;

        // Bound the foreground wait. A hung kill must not stall generators.
        // `try_wait` consumes the exit status, so track reaping explicitly —
        // a second try_wait after success returns Ok(None).
        let reaped = Cell::new(false);
        let cleanup_started = Instant::now();
        poll_exit_until(
            Duration::from_secs(1),
            || match child.try_wait() {
                Ok(Some(_)) => {
                    reaped.set(true);
                    Ok(true)
                },
                Ok(None) => Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Ok(false),
                Err(_) => Ok(true),
            },
            || cleanup_started.elapsed(),
            std::thread::sleep,
        );
        if !reaped.get() {
            match child.try_wait() {
                Ok(Some(_)) => {},
                Ok(None) | Err(_) => reap_pid_in_background(pid),
            }
        }
    }
    #[cfg(not(unix))]
    {
        reap(child);
    }
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        #[cfg(test)]
        GROUP_SIGNAL_OBSERVATIONS.with(|observations| {
            if let Some(observations) = observations.borrow_mut().as_mut() {
                observations.push(child_has_exited(pid).map_err(|error| error.raw_os_error()));
            }
        });
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

#[cfg(all(test, unix))]
std::thread_local! {
    static GROUP_SIGNAL_OBSERVATIONS: std::cell::RefCell<Option<Vec<Result<bool, Option<i32>>>>> = const { std::cell::RefCell::new(None) };
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
    #[test]
    fn plain_exited_leader_keeps_pid_reserved_until_descendant_group_cleanup() {
        GROUP_SIGNAL_OBSERVATIONS.with(|observations| *observations.borrow_mut() = Some(Vec::new()));
        let started = std::time::Instant::now();
        let result = run(
            "/bin/sh",
            &["-c".into(), "sleep 5 & printf leader-done".into()],
            "/",
            Duration::from_secs(8),
            false,
            true,
        );
        let observations = GROUP_SIGNAL_OBSERVATIONS.with(|observations| observations.borrow_mut().take().unwrap());
        assert_eq!(result, RunResult::Output("leader-done".into()));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "must not wait for the descendant's pipe EOF"
        );
        // WNOWAIT can still observe the exited leader at the exact group-kill
        // boundary. A consuming try_wait before that signal yields ECHILD.
        assert_eq!(observations, vec![Ok(true)]);
    }

    #[cfg(unix)]
    #[test]
    fn plain_escaped_descendant_does_not_delay_return_or_release_leader_before_signal() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::CommandExt;

        // Keep the escaped child alive through an owned pipe, then release it
        // by closing that pipe even if an assertion fails. No Python, global
        // signal handler, PID churn, or delayed orphan cleanup is needed.
        let (release, hold) = UnixStream::pair().unwrap();
        let (ready_read, ready_write) = UnixStream::pair().unwrap();
        let release_fd = release.as_raw_fd();
        let hold_fd = hold.as_raw_fd();
        let ready_read_fd = ready_read.as_raw_fd();
        let ready_write_fd = ready_write.as_raw_fd();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf leader-done"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: the post-fork closure only invokes async-signal-safe libc
        // functions and constructs an OS error. The escaped child never
        // returns into Rust or runs the copied test harness's destructors.
        unsafe {
            command.pre_exec(move || {
                libc::close(release_fd);
                let child = libc::fork();
                if child < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if child == 0 {
                    libc::close(ready_read_fd);
                    let status = u8::from(libc::setsid() >= 0);
                    loop {
                        let written = libc::write(ready_write_fd, (&status as *const u8).cast(), 1);
                        if written >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                            break;
                        }
                    }
                    libc::close(ready_write_fd);
                    libc::dup2(hold_fd, libc::STDIN_FILENO);
                    libc::close(hold_fd);
                    // Exec also closes std::process's CLOEXEC spawn-error
                    // pipe. Holding it open here would deadlock spawn itself.
                    let argv = [c"cat".as_ptr(), std::ptr::null()];
                    libc::execv(c"/bin/cat".as_ptr(), argv.as_ptr());
                    libc::_exit(127);
                }
                libc::close(hold_fd);
                libc::close(ready_write_fd);
                let mut ready = libc::pollfd {
                    fd: ready_read_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if libc::poll(&mut ready, 1, 3000) <= 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
                }
                let mut status = 0_u8;
                let read = loop {
                    let read = libc::read(ready_read_fd, (&mut status as *mut u8).cast(), 1);
                    if read >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                        break read;
                    }
                };
                libc::close(ready_read_fd);
                if read != 1 || status != 1 {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop((hold, ready_read, ready_write));
        GROUP_SIGNAL_OBSERVATIONS.with(|observations| *observations.borrow_mut() = Some(Vec::new()));
        let started = std::time::Instant::now();
        let result = wait_child(child, Duration::from_secs(5), true);
        drop(release);
        let observations = GROUP_SIGNAL_OBSERVATIONS.with(|observations| observations.borrow_mut().take().unwrap());
        assert_eq!(result, RunResult::Output("leader-done".into()));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "must not await EOF from an escaped descendant"
        );
        assert_eq!(observations, vec![Ok(true)]);
    }

    #[cfg(unix)]
    #[test]
    fn observing_exit_does_not_consume_the_real_child_status() {
        let mut child = Command::new("/bin/sh").args(["-c", "exit 7"]).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !child_has_exited(child.id()).unwrap() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(child_has_exited(child.id()).unwrap());
        assert_eq!(child.wait().unwrap().code(), Some(7));
        assert_eq!(
            child_has_exited(child.id()).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_after_child_handshake_kills_reaps_and_discards_partial_output() {
        for full_output in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let handshake = directory.path().join("started");
            let path = handshake.to_string_lossy().into_owned();
            let token = crate::cancellation::CancellationToken::new();
            let attempt_token = token.clone();
            let thread = std::thread::spawn(move || {
                let _scope = crate::cancellation::enter(attempt_token);
                let args = vec![
                    "-c".into(),
                    "printf partial; printf '%s' \"$$\" > \"$1\"; while :; do sleep 1; done".into(),
                    "cancel-test".into(),
                    path,
                ];
                if full_output {
                    assert_eq!(
                        execute_full("/bin/sh", &args, "/", &[], Duration::from_secs(5)),
                        Err(CommandError::Cancelled)
                    );
                } else {
                    assert_eq!(
                        run("/bin/sh", &args, "/", Duration::from_secs(5), true, false),
                        RunResult::Cancelled
                    );
                }
            });
            let started = std::time::Instant::now();
            let pid = loop {
                if let Some(pid) = std::fs::read_to_string(&handshake)
                    .ok()
                    .and_then(|text| text.parse::<i32>().ok())
                {
                    break Some(pid);
                }
                if started.elapsed() > Duration::from_secs(3) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let cancelled_at = std::time::Instant::now();
            assert!(token.cancel());
            thread.join().unwrap();
            let pid = pid.expect("the real child must have started before cancellation");
            assert!(
                cancelled_at.elapsed() < Duration::from_secs(2),
                "must not wait for the 5s command deadline"
            );
            let mut status = 0;
            // SAFETY: query only the direct child whose PID the handshake returned.
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
        }
    }

    #[test]
    fn cancelled_attempt_never_starts_another_command() {
        let _mock = mock::install(vec![mock::ExecRule::default()]);
        let token = crate::cancellation::CancellationToken::new();
        let _scope = crate::cancellation::enter(token.clone());
        token.cancel();
        assert_eq!(
            execute_full("unused", &[], "", &[], Duration::from_secs(1)),
            Err(CommandError::Cancelled)
        );
        assert_eq!(
            run("unused", &[], "", Duration::from_secs(1), false, false),
            RunResult::Cancelled
        );
        assert!(mock::calls().is_empty());
    }

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
        assert!(
            output.stdout.as_bytes()["stdout-prefix".len()..]
                .iter()
                .all(|byte| *byte == 0)
        );
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
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cleanup took {:?}",
            started.elapsed()
        );
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
        assert_eq!(
            wait_child_output(child, Duration::from_secs(5)),
            Err(CommandError::Failed)
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cleanup took {:?}",
            started.elapsed()
        );
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
        let started = std::time::Instant::now();
        let out = execute("sleep", &["2".into()], "/", Duration::from_millis(50));
        assert!(out.is_empty());
        assert_eq!(
            try_execute("sleep", &["2".into()], "/", Duration::from_millis(50)),
            None
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "ordinary execute cleanup must stay bounded, took {:?}",
            started.elapsed()
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
