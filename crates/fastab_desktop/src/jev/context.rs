//! Bounded, best-effort terminal context for an enabled Jev request.

use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use fastab_settings::history::History;
use fastab_settings::history::rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::Instant;

pub const MAX_BRANCH_BYTES: usize = 256;
pub const MAX_HISTORY_COMMANDS: usize = 10;
pub const MAX_COMMAND_BYTES: usize = 512;
pub const MAX_HISTORY_BYTES: usize = 2048;

const HISTORY_WINDOW: usize = 500;
const CONTEXT_TIMEOUT: Duration = Duration::from_millis(200);
const MAX_STATUS_BYTES: usize = 128 * 1024;
const MAX_STATUS_ENTRIES: usize = 4096;
static HISTORY_QUERIES: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
static REPOSITORY_QUERIES: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// Git can spawn helpers for submodules. Keep the group leader unreaped until
/// stdout closes, then kill the entire group if a timeout or output bound drops
/// this future. Tokio's child kill_on_drop alone covers only the leader.
struct GitChild {
    child: Option<tokio::process::Child>,
    group_id: i32,
    finished: bool,
}

impl GitChild {
    fn spawn(command: &mut Command) -> Result<Self, ()> {
        let child = command.spawn().map_err(|_error| ())?;
        let group_id = child.id().and_then(|pid| i32::try_from(pid).ok()).ok_or(())?;
        Ok(Self {
            child: Some(child),
            group_id,
            finished: false,
        })
    }

    fn stdout(&mut self) -> Result<tokio::process::ChildStdout, ()> {
        self.child.as_mut().and_then(|child| child.stdout.take()).ok_or(())
    }

    async fn wait(&mut self) -> Result<std::process::ExitStatus, ()> {
        let status = self.child.as_mut().ok_or(())?.wait().await.map_err(|_error| ())?;
        self.finished = true;
        Ok(status)
    }
}

impl Drop for GitChild {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // SAFETY: process_group(0) gave this still-owned child a group ID
        // equal to its PID. We do not reap the leader before this point, so
        // that identifier cannot have been recycled for an unrelated group.
        unsafe {
            libc::kill(-self.group_id, libc::SIGKILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    // Cleanup must not lengthen the caller's 200 ms deadline.
                    let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
                });
            }
            // Outside a runtime, Tokio's kill_on_drop still retires the leader.
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GitStatus {
    pub has_staged: bool,
    pub has_unstaged: bool,
    pub has_conflicts: bool,
    pub has_untracked: bool,
}

#[derive(Clone, Default, Serialize)]
pub struct TerminalContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
    pub recent_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_status: Option<GitStatus>,
}

pub struct CollectedContext {
    pub terminal: TerminalContext,
    pub fingerprint: [u8; 32],
    pub complete: bool,
}

enum GitSnapshot {
    NoRepository,
    Repository {
        branch: Option<String>,
        status: GitStatus,
        porcelain_digest: [u8; 32],
    },
}

pub async fn collect_for_request(cwd: &str, share_git_status: bool) -> CollectedContext {
    collect_for_request_with_history(cwd, share_git_status, collect_history(cwd)).await
}

async fn collect_for_request_with_history(
    cwd: &str,
    share_git_status: bool,
    history_query: impl Future<Output = Result<Vec<String>, ()>>,
) -> CollectedContext {
    if cwd.is_empty() {
        return CollectedContext {
            terminal: TerminalContext::default(),
            fingerprint: fingerprint(None, None),
            complete: false,
        };
    }
    let deadline = Instant::now() + CONTEXT_TIMEOUT;
    let (git, history) = tokio::join!(
        tokio::time::timeout_at(deadline, collect_git(cwd)),
        tokio::time::timeout_at(deadline, history_query),
    );
    let git = git.ok().and_then(Result::ok);
    let history = history.ok().and_then(Result::ok);
    let complete = git.is_some() && history.is_some();
    let context_fingerprint = fingerprint(git.as_ref(), history.as_deref());
    let current_branch = match &git {
        Some(GitSnapshot::Repository { branch, .. }) => branch.clone(),
        _ => None,
    };
    let git_status = if share_git_status {
        match &git {
            Some(GitSnapshot::Repository { status, .. }) => Some(*status),
            _ => None,
        }
    } else {
        None
    };
    CollectedContext {
        terminal: TerminalContext {
            current_branch,
            recent_commands: history.unwrap_or_default(),
            git_status,
        },
        fingerprint: context_fingerprint,
        complete,
    }
}

async fn collect_history(cwd: &str) -> Result<Vec<String>, ()> {
    let cwd = cwd.to_owned();
    bounded_history_query(HISTORY_QUERIES.clone(), move || recent_commands(&History::new(), &cwd)).await
}

async fn bounded_history_query(
    semaphore: Arc<Semaphore>,
    query: impl FnOnce() -> Result<Vec<String>, ()> + Send + 'static,
) -> Result<Vec<String>, ()> {
    // Dropping the timeout's JoinHandle does not stop spawn_blocking. Move the
    // permit into the closure so a slow SQLite read keeps the slot until done.
    let permit = semaphore.try_acquire_owned().map_err(|_error| ())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        query()
    })
    .await
    .map_err(|_error| ())?
}

async fn repository_present(cwd: &str) -> Result<bool, ()> {
    let cwd = cwd.to_owned();
    bounded_repository_query(REPOSITORY_QUERIES.clone(), move || repository_present_local(&cwd)).await
}

async fn bounded_repository_query(
    semaphore: Arc<Semaphore>,
    query: impl FnOnce() -> Result<bool, ()> + Send + 'static,
) -> Result<bool, ()> {
    // A context timeout cannot stop filesystem I/O on an unavailable mount.
    // Reserve before spawning, and keep the slot inside the real blocking work
    // so cancelled requests cannot fill Tokio's blocking pool or its queue.
    let permit = semaphore.try_acquire_owned().map_err(|_error| ())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        query()
    })
    .await
    .map_err(|_error| ())?
}

fn repository_present_local(cwd: &str) -> Result<bool, ()> {
    let resolved = std::fs::canonicalize(cwd).map_err(|_error| ())?;
    if !std::fs::metadata(&resolved).map_err(|_error| ())?.is_dir() {
        return Err(());
    }
    for ancestor in resolved.ancestors() {
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err(()),
        }
    }
    Ok(false)
}

async fn collect_git(cwd: &str) -> Result<GitSnapshot, ()> {
    if !repository_present(cwd).await? {
        return Ok(GitSnapshot::NoRepository);
    }
    let (branch, status) = tokio::join!(read_branch(cwd), read_status(cwd));
    let (status, porcelain_digest) = status?;
    Ok(GitSnapshot::Repository {
        branch: branch?,
        status,
        porcelain_digest,
    })
}

fn git_command(cwd: &str) -> Command {
    let mut command = Command::new("git");
    // Git's inherited environment can redirect an otherwise local query into
    // a different repository, config file, or executable helper.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.process_group(0);
    command
}

async fn read_branch(cwd: &str) -> Result<Option<String>, ()> {
    let mut command = git_command(cwd);
    command.args(["branch", "--show-current"]);
    let mut child = GitChild::spawn(&mut command)?;
    let mut stdout = child.stdout()?.take((MAX_BRANCH_BYTES + 2) as u64);
    let mut bytes = Vec::with_capacity(MAX_BRANCH_BYTES + 2);
    stdout.read_to_end(&mut bytes).await.map_err(|_error| ())?;
    if bytes.len() > MAX_BRANCH_BYTES + 1 {
        return Err(());
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(());
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let branch = std::str::from_utf8(&bytes)
        .map_err(|_error| ())?
        .strip_suffix('\n')
        .ok_or(())?;
    if branch.is_empty() || branch.len() > MAX_BRANCH_BYTES || branch.chars().any(char::is_control) {
        return Err(());
    }
    Ok(Some(branch.to_owned()))
}

async fn read_status(cwd: &str) -> Result<(GitStatus, [u8; 32]), ()> {
    let mut command = git_command(cwd);
    command.args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignore-submodules=none",
    ]);
    let mut child = GitChild::spawn(&mut command)?;
    let mut stdout = child.stdout()?.take((MAX_STATUS_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).await.map_err(|_error| ())?;
    if bytes.len() > MAX_STATUS_BYTES {
        // Do not wait on a child whose full stdout we intentionally stopped
        // reading: it may be blocked on a full pipe. kill_on_drop retires it.
        return Err(());
    }
    if !child.wait().await?.success() {
        return Err(());
    }
    let status = parse_status(&bytes)?;
    let mut digest = [0; 32];
    digest.copy_from_slice(&Sha256::digest(&bytes));
    Ok((status, digest))
}

fn parse_status(bytes: &[u8]) -> Result<GitStatus, ()> {
    let mut status = GitStatus::default();
    let mut offset = 0;
    let mut entries = 0;
    while offset < bytes.len() {
        let end = offset + bytes[offset..].iter().position(|byte| *byte == 0).ok_or(())?;
        let record = &bytes[offset..end];
        offset = end + 1;
        if record.len() < 4 || record[2] != b' ' {
            return Err(());
        }
        let (x, y) = (record[0], record[1]);
        let conflict = matches!(
            (x, y),
            (b'D' | b'U', b'D') | (b'A' | b'D' | b'U', b'U') | (b'U' | b'A', b'A')
        );
        let ordinary = b" MADRCT".contains(&x) && b" MDTm?".contains(&y) && (x, y) != (b' ', b' ');
        if !conflict && !ordinary && !matches!((x, y), (b'?', b'?') | (b'!', b'!')) {
            return Err(());
        }
        entries += 1;
        if entries > MAX_STATUS_ENTRIES {
            return Err(());
        }
        if conflict {
            status.has_conflicts = true;
        } else if (x, y) == (b'?', b'?') {
            status.has_untracked = true;
        } else if (x, y) != (b'!', b'!') {
            status.has_staged |= x != b' ';
            status.has_unstaged |= y != b' ';
        }
        if matches!(x, b'R' | b'C') {
            let original_end = offset + bytes[offset..].iter().position(|byte| *byte == 0).ok_or(())?;
            if original_end == offset {
                return Err(());
            }
            offset = original_end + 1;
        }
    }
    Ok(status)
}

fn hash_field(hasher: &mut Sha256, label: u8, value: &[u8]) {
    hasher.update([label]);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn fingerprint(git: Option<&GitSnapshot>, history: Option<&[String]>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"jev-context-v2");
    match git {
        Some(GitSnapshot::NoRepository) => hash_field(&mut hasher, 1, b"no-repository"),
        Some(GitSnapshot::Repository {
            branch,
            porcelain_digest,
            ..
        }) => {
            hash_field(&mut hasher, 1, b"repository");
            match branch {
                Some(branch) => {
                    hash_field(&mut hasher, 2, b"branch");
                    hash_field(&mut hasher, 3, branch.as_bytes());
                },
                None => hash_field(&mut hasher, 2, b"detached"),
            }
            hash_field(&mut hasher, 6, porcelain_digest);
        },
        None => hash_field(&mut hasher, 1, b"incomplete"),
    }
    match history {
        Some(commands) => {
            hash_field(&mut hasher, 4, &(commands.len() as u64).to_le_bytes());
            for command in commands {
                hash_field(&mut hasher, 5, command.as_bytes());
            }
        },
        None => hash_field(&mut hasher, 4, b"incomplete"),
    }
    let mut digest = [0; 32];
    digest.copy_from_slice(&hasher.finalize());
    digest
}

fn recent_commands(history: &History, cwd: &str) -> Result<Vec<String>, ()> {
    // Limit the search before filtering. This avoids scanning or exposing the
    // user's complete history, even when this directory has few matching rows.
    let rows = history.query(
        "SELECT command FROM (
            SELECT id, command, cwd FROM history ORDER BY id DESC LIMIT ?1
         ) WHERE cwd = ?2
           AND command IS NOT NULL AND length(CAST(command AS BLOB)) <= ?3
         ORDER BY id DESC",
        params![HISTORY_WINDOW, cwd, MAX_COMMAND_BYTES],
    );
    let rows = rows.map_err(|_error| ())?;
    let mut commands = Vec::with_capacity(MAX_HISTORY_COMMANDS);
    let mut total_bytes = 0;
    for row in rows {
        let Some(command) = row.get("command").and_then(|value| value.as_str()) else {
            continue;
        };
        if command.trim().is_empty() || command.chars().any(char::is_control) || looks_sensitive(command) {
            continue;
        }
        if total_bytes + command.len() > MAX_HISTORY_BYTES {
            continue;
        }
        total_bytes += command.len();
        commands.push(command.to_owned());
        if commands.len() == MAX_HISTORY_COMMANDS {
            break;
        }
    }
    commands.reverse();
    Ok(commands)
}

// Conservative pattern screen, not a guarantee that arbitrary secrets are detected.
fn looks_sensitive(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if [
        "token",
        "password",
        "passwd",
        "secret",
        "api-key",
        "api_key",
        "apikey",
        "authorization",
        "bearer ",
        "private_key",
        "private-key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    if lower.split_whitespace().any(|part| {
        let part = part.trim_start_matches(['\'', '"']);
        part == "--user"
            || part.starts_with("--user=")
            || part.split_once('=').is_some_and(|(name, _)| {
                name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
    }) {
        return true;
    }
    // Short password flags are tool-specific: git's -u and docker run's -p
    // are useful history, unlike curl -u or docker login -p.
    let has_tool = |tool| {
        lower
            .split_whitespace()
            .any(|part| part.trim_matches(['\'', '"']).rsplit('/').next() == Some(tool))
    };
    let has_flag = |flag| lower.split_whitespace().any(|part| part.starts_with(flag));
    let password_flag = has_tool("mysql")
        || has_tool("mariadb")
        || has_tool("sshpass")
        || ((has_tool("docker") || has_tool("podman")) && lower.split_whitespace().any(|part| part == "login"));
    if (has_tool("curl") && has_flag("-u"))
        || (password_flag && has_flag("-p"))
        || (has_tool("redis-cli") && (has_flag("-a") || has_flag("--pass")))
    {
        return true;
    }
    // URL userinfo is confined to the authority, before its first /, ?, or #.
    lower.match_indices("://").any(|(index, _)| {
        lower[index + 3..]
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    })
}

#[cfg(test)]
mod tests {
    use fastab_settings::history::CommandInfo;
    use std::path::Path;

    use super::*;

    // These integration cases use the process-wide, fail-fast repository gate.
    // Keep independent test runtimes from contending with each other's probes.
    static REPOSITORY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn insert(history: &History, cwd: &str, command: &str) {
        history
            .insert_command_history(
                &CommandInfo {
                    command: Some(command.into()),
                    cwd: Some(cwd.into()),
                    ..CommandInfo::default()
                },
                false,
            )
            .unwrap();
    }

    fn git_at(cwd: &Path, args: &[&str]) {
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let status = command
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "temporary Git command failed: {args:?}");
    }

    #[test]
    fn history_is_scoped_by_directory_recent_ordered_and_bounded() {
        let history = History::mock();
        insert(&history, "/work", "git status");
        for number in 0..12 {
            insert(&history, "/work", &format!("git show {number}"));
        }
        insert(&history, "/else", "unrelated directory");
        assert_eq!(
            recent_commands(&history, "/work").unwrap(),
            (2..12).map(|number| format!("git show {number}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn history_filters_credentials_and_honors_byte_limits() {
        let history = History::mock();
        for command in [
            "TOKEN=x command",
            "export AWS_ACCESS_KEY_ID=x",
            "NAME=value command",
            "tool --password=x",
            "tool --api-key x",
            "curl -H 'Authorization: Bearer x' url",
            "curl https://user:pass@example.com/",
            "curl -u user:pass example.com",
            "curl --user=user:pass example.com",
            "mysql -pabc123",
            "redis-cli -a abc123",
            "sshpass -p abc123 ssh example.com",
            "docker login -p abc123 registry.example.com",
        ] {
            insert(&history, "/work", command);
        }
        assert!(recent_commands(&history, "/work").unwrap().is_empty());
        insert(&history, "/work", &"x".repeat(MAX_COMMAND_BYTES + 1));
        for number in 0..10 {
            insert(&history, "/work", &format!("echo {number} {}", "x".repeat(248)));
        }
        let commands = recent_commands(&history, "/work").unwrap();
        assert_eq!(commands.len(), 8);
        assert!(commands.iter().map(String::len).sum::<usize>() <= MAX_HISTORY_BYTES);
        assert!(commands.first().is_some_and(|command| command.starts_with("echo 2 ")));
        assert!(commands.last().is_some_and(|command| command.starts_with("echo 9 ")));
        assert!(!looks_sensitive("git push -u origin feature/demo"));
        assert!(!looks_sensitive("docker run -p 8080:80 example"));
    }

    #[test]
    fn history_search_stays_within_latest_global_window() {
        let history = History::mock();
        insert(&history, "/work", "older command");
        for number in 0..HISTORY_WINDOW {
            insert(&history, "/else", &format!("echo {number}"));
        }
        assert!(recent_commands(&history, "/work").unwrap().is_empty());
    }

    #[test]
    fn history_byte_boundary_counts_utf8_bytes() {
        let history = History::mock();
        let at_limit = "é".repeat(MAX_COMMAND_BYTES / 2);
        insert(&history, "/work", &at_limit);
        insert(&history, "/work", &format!("{at_limit}é"));
        assert_eq!(recent_commands(&history, "/work").unwrap(), vec![at_limit]);
    }

    #[tokio::test]
    async fn branch_uses_current_repository_and_rejects_non_repository() {
        let _guard = REPOSITORY_TEST_LOCK.lock().await;
        let repository = tempfile::tempdir().unwrap();
        let git_dir = repository.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::create_dir(git_dir.join("objects")).unwrap();
        std::fs::create_dir(git_dir.join("refs")).unwrap();
        std::fs::write(
            git_dir.join("config"),
            "[core]\n repositoryformatversion = 0\n bare = false\n",
        )
        .unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/demo\n").unwrap();
        assert_eq!(
            read_branch(repository.path().to_str().unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("feature/demo")
        );
        let other = tempfile::tempdir().unwrap();
        assert!(!repository_present(other.path().to_str().unwrap()).await.unwrap());
    }

    #[test]
    fn porcelain_status_uses_only_status_bytes_and_handles_renames() {
        let status = parse_status(b" M modified\0A  staged\0R  new\0old\0UU conflict\0?? \xff\0").unwrap();
        assert_eq!(
            status,
            GitStatus {
                has_staged: true,
                has_unstaged: true,
                has_conflicts: true,
                has_untracked: true,
            }
        );
        assert_eq!(parse_status(b"").unwrap(), GitStatus::default());
        for pair in [b"DD", b"AU", b"UD", b"UA", b"DU", b"AA", b"UU"] {
            let record = [pair[0], pair[1], b' ', b'x', 0];
            assert_eq!(
                parse_status(&record).unwrap(),
                GitStatus {
                    has_conflicts: true,
                    ..GitStatus::default()
                }
            );
        }
        assert!(parse_status(b" M unterminated").is_err());
        assert!(parse_status(b"R  new\0").is_err());
        assert!(parse_status(b"   path\0").is_err());
        assert!(parse_status(b"?M path\0").is_err());
        assert!(parse_status(b"M! path\0").is_err());
        assert!(parse_status(b" M path\0".repeat(MAX_STATUS_ENTRIES + 1).as_slice()).is_err());
    }

    #[test]
    fn fingerprint_changes_with_local_status_branch_and_effective_history() {
        let first = GitSnapshot::Repository {
            branch: Some("main".into()),
            status: GitStatus::default(),
            porcelain_digest: [1; 32],
        };
        let changed = GitSnapshot::Repository {
            branch: Some("main".into()),
            status: GitStatus::default(),
            porcelain_digest: [2; 32],
        };
        let first_history = vec!["git status".to_owned()];
        let second_history = vec!["git diff".to_owned()];
        assert_eq!(
            fingerprint(Some(&first), Some(&first_history)),
            fingerprint(Some(&first), Some(&first_history))
        );
        assert_ne!(
            fingerprint(Some(&first), Some(&first_history)),
            fingerprint(Some(&changed), Some(&first_history))
        );
        assert_ne!(
            fingerprint(Some(&first), Some(&first_history)),
            fingerprint(Some(&first), Some(&second_history))
        );
        assert_ne!(
            fingerprint(Some(&first), Some(&[])),
            fingerprint(Some(&GitSnapshot::NoRepository), Some(&[]))
        );
        assert_ne!(fingerprint(Some(&first), Some(&[])), fingerprint(Some(&first), None));
        let detached = GitSnapshot::Repository {
            branch: None,
            status: GitStatus::default(),
            porcelain_digest: [1; 32],
        };
        let named_detached = GitSnapshot::Repository {
            branch: Some("detached".into()),
            status: GitStatus::default(),
            porcelain_digest: [1; 32],
        };
        assert_ne!(
            fingerprint(Some(&detached), Some(&[])),
            fingerprint(Some(&named_detached), Some(&[]))
        );
    }

    #[tokio::test]
    async fn real_git_status_and_opt_in_serialization_are_bounded_to_boolean_fields() {
        let _guard = REPOSITORY_TEST_LOCK.lock().await;
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path();
        let cwd = root.to_str().unwrap();
        git_at(root, &["init", "-q"]);
        std::fs::write(root.join("tracked"), b"base\n").unwrap();
        git_at(root, &["add", "tracked"]);
        git_at(
            root,
            &[
                "-c",
                "user.name=Fastab Test",
                "-c",
                "user.email=fastab-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );

        let (clean, _) = read_status(cwd).await.unwrap();
        assert_eq!(clean, GitStatus::default());
        std::fs::write(root.join("tracked"), b"changed\n").unwrap();
        let (unstaged, _) = read_status(cwd).await.unwrap();
        assert!(unstaged.has_unstaged);
        assert!(!unstaged.has_staged);
        git_at(root, &["add", "tracked"]);
        let (staged, _) = read_status(cwd).await.unwrap();
        assert!(staged.has_staged);
        assert!(!staged.has_unstaged);
        git_at(
            root,
            &[
                "-c",
                "user.name=Fastab Test",
                "-c",
                "user.email=fastab-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "update",
            ],
        );
        std::fs::rename(root.join("tracked"), root.join("renamed-private-file")).unwrap();
        git_at(root, &["add", "-A"]);
        std::fs::write(root.join("untracked-private-file"), b"private\n").unwrap();
        let (renamed_and_untracked, _) = read_status(cwd).await.unwrap();
        assert!(renamed_and_untracked.has_staged);
        assert!(renamed_and_untracked.has_untracked);
        assert!(!renamed_and_untracked.has_conflicts);

        let off = collect_for_request_with_history(cwd, false, async { Ok(Vec::new()) }).await;
        let on = collect_for_request_with_history(cwd, true, async { Ok(Vec::new()) }).await;
        assert!(off.complete && on.complete);
        assert_eq!(off.fingerprint, on.fingerprint);
        assert!(off.terminal.git_status.is_none());
        assert_eq!(on.terminal.git_status, Some(renamed_and_untracked));
        let off_json = serde_json::to_value(&off.terminal).unwrap();
        let on_json = serde_json::to_value(&on.terminal).unwrap();
        assert!(off_json.get("git_status").is_none());
        let fields = on_json["git_status"].as_object().unwrap();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields["has_staged"].as_bool(), Some(true));
        assert_eq!(fields["has_untracked"].as_bool(), Some(true));
        let serialized = serde_json::to_string(&on.terminal).unwrap();
        assert!(!serialized.contains("renamed-private-file"));
        assert!(!serialized.contains("untracked-private-file"));
    }

    #[tokio::test]
    async fn repository_detection_follows_an_external_symlink() {
        let _guard = REPOSITORY_TEST_LOCK.lock().await;
        let repository = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        git_at(repository.path(), &["init", "-q"]);
        let subdir = repository.path().join("nested");
        std::fs::create_dir(&subdir).unwrap();
        let link = outside.path().join("linked-cwd");
        std::os::unix::fs::symlink(&subdir, &link).unwrap();
        let cwd = link.to_str().unwrap();
        assert!(repository_present(cwd).await.unwrap());
        let first = collect_for_request_with_history(cwd, false, async { Ok(Vec::new()) }).await;
        assert!(first.complete);
        std::fs::write(subdir.join("new-file"), b"a").unwrap();
        let second = collect_for_request_with_history(cwd, false, async { Ok(Vec::new()) }).await;
        assert!(second.complete);
        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[tokio::test]
    async fn cancelled_git_query_kills_its_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let finished = temp.path().join("descendant-finished");
        let ready_arg = ready.to_str().unwrap().to_owned();
        let finished_arg = finished.to_str().unwrap().to_owned();
        let task = tokio::spawn(async move {
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "(sleep 0.5; printf done > \"$2\") & printf ready > \"$1\"; wait",
                    "git-child-test",
                    &ready_arg,
                    &finished_arg,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .process_group(0);
            let mut child = GitChild::spawn(&mut command).unwrap();
            let mut stdout = child.stdout().unwrap();
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await.unwrap();
            child.wait().await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert!(!finished.exists(), "a descendant survived cancelled Git work");
    }

    async fn assert_repository_query_cancellation_keeps_its_permit(abort: bool) {
        let semaphore = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut query = Box::pin(bounded_repository_query(semaphore.clone(), move || {
            let _ = started_tx.send(());
            // Dropping release_tx also unblocks the worker if an assertion fails.
            let _ = release_rx.recv();
            Ok(true)
        }));
        tokio::select! {
            result = &mut query => panic!("blocked repository query finished early: {result:?}"),
            started = tokio::time::timeout(Duration::from_secs(5), started_rx) => {
                started.unwrap().unwrap();
            },
        }

        if abort {
            let task = tokio::spawn(query);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            // Own the pinned future: the timeout must drop the query itself,
            // not merely a borrowed future or a detached JoinHandle.
            assert!(tokio::time::timeout(Duration::ZERO, query).await.is_err());
        }
        assert_eq!(semaphore.available_permits(), 0);
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..4 {
            let entered = entered.clone();
            assert!(
                bounded_repository_query(semaphore.clone(), move || {
                    entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(false)
                })
                .await
                .is_err()
            );
        }
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 0);
        release_tx.send(()).unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(5), semaphore.clone().acquire_owned())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(bounded_repository_query(semaphore, || Ok(false)).await, Ok(false));
    }

    #[tokio::test]
    async fn timed_out_repository_query_keeps_its_permit_until_blocking_work_exits() {
        assert_repository_query_cancellation_keeps_its_permit(false).await;
    }

    #[tokio::test]
    async fn aborted_repository_query_keeps_its_permit_until_blocking_work_exits() {
        assert_repository_query_cancellation_keeps_its_permit(true).await;
    }

    #[tokio::test]
    async fn cancelled_history_query_keeps_its_permit_until_blocking_work_exits() {
        let semaphore = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let query_semaphore = semaphore.clone();
        let task = tokio::spawn(async move {
            bounded_history_query(query_semaphore, move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Ok(Vec::new())
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(semaphore.available_permits(), 0);
        assert!(
            bounded_history_query(semaphore.clone(), || Ok(Vec::new()))
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while semaphore.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
