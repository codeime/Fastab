//! Bounded, best-effort terminal context for an enabled Jev request.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use fastab_settings::history::History;
use fastab_settings::history::rusqlite::params;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub const MAX_BRANCH_BYTES: usize = 256;
pub const MAX_HISTORY_COMMANDS: usize = 10;
pub const MAX_COMMAND_BYTES: usize = 512;
pub const MAX_HISTORY_BYTES: usize = 2048;

const HISTORY_WINDOW: usize = 500;
const CONTEXT_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Clone, Default, Serialize)]
pub struct TerminalContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_branch: Option<String>,
    pub recent_commands: Vec<String>,
}

pub async fn collect(cwd: &str) -> TerminalContext {
    if cwd.is_empty() {
        return TerminalContext::default();
    }
    let branch = current_branch(cwd);
    let history_cwd = cwd.to_owned();
    let history = async move {
        tokio::time::timeout(
            CONTEXT_TIMEOUT,
            tokio::task::spawn_blocking(move || recent_commands(&History::new(), &history_cwd)),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
    };
    let (current_branch, recent_commands) = tokio::join!(branch, history);
    TerminalContext {
        current_branch,
        recent_commands,
    }
}

async fn current_branch(cwd: &str) -> Option<String> {
    tokio::time::timeout(CONTEXT_TIMEOUT, read_branch(cwd))
        .await
        .ok()
        .flatten()
}

async fn read_branch(cwd: &str) -> Option<String> {
    let mut repository_found = false;
    for ancestor in Path::new(cwd).ancestors() {
        if tokio::fs::symlink_metadata(ancestor.join(".git")).await.is_ok() {
            repository_found = true;
            break;
        }
    }
    if !repository_found {
        return None;
    }
    let mut child = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?.take((MAX_BRANCH_BYTES + 2) as u64);
    let mut bytes = Vec::with_capacity(MAX_BRANCH_BYTES + 2);
    stdout.read_to_end(&mut bytes).await.ok()?;
    if bytes.len() > MAX_BRANCH_BYTES + 1 {
        return None;
    }
    let status = child.wait().await.ok()?;
    if !status.success() {
        return None;
    }
    let branch = std::str::from_utf8(&bytes).ok()?.strip_suffix('\n')?;
    if branch.is_empty() || branch.len() > MAX_BRANCH_BYTES || branch.chars().any(char::is_control) {
        return None;
    }
    Some(branch.to_owned())
}

fn recent_commands(history: &History, cwd: &str) -> Vec<String> {
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
    let Ok(rows) = rows else {
        return Vec::new();
    };
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
    commands
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

    use super::*;

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

    #[test]
    fn history_is_scoped_by_directory_recent_ordered_and_bounded() {
        let history = History::mock();
        insert(&history, "/work", "git status");
        for number in 0..12 {
            insert(&history, "/work", &format!("git show {number}"));
        }
        insert(&history, "/else", "unrelated directory");
        assert_eq!(
            recent_commands(&history, "/work"),
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
        assert!(recent_commands(&history, "/work").is_empty());
        insert(&history, "/work", &"x".repeat(MAX_COMMAND_BYTES + 1));
        for number in 0..10 {
            insert(&history, "/work", &format!("echo {number} {}", "x".repeat(248)));
        }
        let commands = recent_commands(&history, "/work");
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
        assert!(recent_commands(&history, "/work").is_empty());
    }

    #[test]
    fn history_byte_boundary_counts_utf8_bytes() {
        let history = History::mock();
        let at_limit = "é".repeat(MAX_COMMAND_BYTES / 2);
        insert(&history, "/work", &at_limit);
        insert(&history, "/work", &format!("{at_limit}é"));
        assert_eq!(recent_commands(&history, "/work"), vec![at_limit]);
    }

    #[tokio::test]
    async fn branch_uses_current_repository_and_rejects_non_repository() {
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
            current_branch(repository.path().to_str().unwrap()).await.as_deref(),
            Some("feature/demo")
        );
        let other = tempfile::tempdir().unwrap();
        assert_eq!(current_branch(other.path().to_str().unwrap()).await, None);
    }
}
