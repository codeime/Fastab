use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use ec_engine::{CompleteRequest, CompleteResult, EngineClient, HookBackend, complete_result_diffs, default_specs_dir};
use eyre::{Context, Result, bail};
use serde::Serialize;

/// Run the headless completion engine
#[derive(Debug, PartialEq, Args)]
pub struct EngineArgs {
    #[command(subcommand)]
    command: EngineCommand,
}

#[derive(Debug, PartialEq, clap::Subcommand)]
enum EngineCommand {
    /// Print suggestions for a command buffer
    Complete {
        /// Shell buffer to complete, e.g. "git ch"
        #[arg(long)]
        buffer: Option<String>,
        /// Working directory used to resolve local specs
        #[arg(long)]
        cwd: Option<String>,
        /// Override the bundled specs directory
        #[arg(long)]
        specs_dir: Option<PathBuf>,
        /// Run Native and Js, print a JSON diff, exit 1 when they differ.
        /// Test-only dual-path (T3.3). Does not change the product default.
        #[arg(long)]
        compare: bool,
        /// JSONL of `{buffer, cwd}` session rows. Implies `--compare`.
        /// `cwd` may be a repo key (`git` / `npm` / `docker` / `kubectl` / `cargo`)
        /// resolved under `--repos-dir`.
        #[arg(long)]
        session: Option<PathBuf>,
        /// Root of T3.3 fixture repos (`tests/dual-path/repos`)
        #[arg(long)]
        repos_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
struct CompareReport {
    equal: bool,
    buffer: String,
    cwd: String,
    diffs: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native: Option<CompleteResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    js: Option<CompleteResult>,
}

#[derive(Debug, serde::Deserialize)]
struct SessionRow {
    buffer: String,
    cwd: String,
}

struct CompareEngines {
    native: EngineClient,
    js: EngineClient,
}

impl EngineArgs {
    pub async fn execute(self) -> Result<ExitCode> {
        match self.command {
            EngineCommand::Complete {
                buffer,
                cwd,
                specs_dir,
                compare,
                session,
                repos_dir,
            } => {
                let specs_dir = specs_dir.unwrap_or_else(default_specs_dir);
                if let Some(session) = session {
                    return run_session(specs_dir, session, repos_dir).await;
                }
                let buffer = buffer.ok_or_else(|| eyre::eyre!("--buffer is required unless --session is set"))?;
                let cwd = cwd.unwrap_or_else(|| {
                    std::env::current_dir().map_or_else(|_err| "/".into(), |path| path.display().to_string())
                });
                let request = CompleteRequest {
                    buffer: buffer.clone(),
                    cwd: cwd.clone(),
                    cursor: None,
                    include_history: false,
                    ..CompleteRequest::default()
                };
                if compare {
                    let engines = CompareEngines::spawn(specs_dir)?;
                    let report = engines.compare(request).await?;
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    return Ok(if report.equal {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    });
                }
                let engine = EngineClient::spawn(specs_dir).map_err(|err| eyre::eyre!("{err}"))?;
                let result = engine.complete(request).await.map_err(|err| eyre::eyre!("{err}"))?;
                // Keep this diagnostic command lossless. Insertion metadata,
                // the normalized match term, and current-argument context are
                // precisely the fields needed when comparing the native
                // engine with the former WebView implementation.
                println!("{}", serde_json::to_string_pretty(&result)?);
                Ok(ExitCode::SUCCESS)
            },
        }
    }
}

impl CompareEngines {
    fn spawn(specs_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            native: EngineClient::spawn(specs_dir.clone()).map_err(|err| eyre::eyre!("{err}"))?,
            js: EngineClient::spawn(specs_dir).map_err(|err| eyre::eyre!("{err}"))?,
        })
    }

    async fn compare(&self, request: CompleteRequest) -> Result<CompareReport> {
        let native = self
            .native
            .complete(CompleteRequest {
                backend_override: Some(HookBackend::Native),
                ..request.clone()
            })
            .await
            .map_err(|err| eyre::eyre!("native: {err}"))?;
        let js = self
            .js
            .complete(CompleteRequest {
                backend_override: Some(HookBackend::Js),
                ..request.clone()
            })
            .await
            .map_err(|err| eyre::eyre!("js: {err}"))?;
        let diffs = complete_result_diffs(&native, &js);
        Ok(CompareReport {
            equal: diffs.is_empty(),
            buffer: request.buffer,
            cwd: request.cwd,
            diffs,
            native: Some(native),
            js: Some(js),
        })
    }
}

async fn run_session(specs_dir: PathBuf, session: PathBuf, repos_dir: Option<PathBuf>) -> Result<ExitCode> {
    let text = std::fs::read_to_string(&session).with_context(|| format!("reading {}", session.display()))?;
    let engines = CompareEngines::spawn(specs_dir)?;
    let mut compared = 0usize;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: SessionRow =
            serde_json::from_str(line).with_context(|| format!("{}:{}", session.display(), index + 1))?;
        let cwd = resolve_session_cwd(&row.cwd, repos_dir.as_deref())?;
        let report = engines
            .compare(CompleteRequest {
                buffer: row.buffer,
                cwd,
                include_history: false,
                ..CompleteRequest::default()
            })
            .await?;
        compared += 1;
        if !report.equal {
            println!("{}", serde_json::to_string_pretty(&report)?);
            bail!(
                "dual-path diff at {}:{} buffer={:?} ({} diffs)",
                session.display(),
                index + 1,
                report.buffer,
                report.diffs.len()
            );
        }
    }
    if compared < 100 {
        bail!(
            "session {} compared {compared} buffers; T3.3 requires ≥ 100",
            session.display()
        );
    }
    println!(
        "{}",
        serde_json::json!({
            "equal": true,
            "session": session,
            "compared": compared
        })
    );
    Ok(ExitCode::SUCCESS)
}

fn resolve_session_cwd(cwd: &str, repos_dir: Option<&Path>) -> Result<String> {
    let path = Path::new(cwd);
    if path.is_absolute() {
        return Ok(cwd.to_string());
    }
    match cwd {
        "git" | "npm" | "docker" | "kubectl" | "cargo" => {
            let root = repos_dir.ok_or_else(|| eyre::eyre!("session cwd `{cwd}` needs --repos-dir"))?;
            Ok(root.join(cwd).display().to_string())
        },
        _ => Ok(cwd.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    #[command(name = "engine")]
    struct Parse {
        #[command(flatten)]
        args: EngineArgs,
    }

    #[test]
    fn parses_compare_flag() {
        let parsed = Parse::parse_from(["engine", "complete", "--buffer", "git ch", "--compare"]).args;
        assert_eq!(
            parsed,
            EngineArgs {
                command: EngineCommand::Complete {
                    buffer: Some("git ch".into()),
                    cwd: None,
                    specs_dir: None,
                    compare: true,
                    session: None,
                    repos_dir: None,
                },
            }
        );
    }

    #[test]
    fn parses_session_flag() {
        let parsed = Parse::parse_from([
            "engine",
            "complete",
            "--session",
            "tests/dual-path/sessions/git.jsonl",
            "--repos-dir",
            "tests/dual-path/repos",
        ])
        .args;
        assert_eq!(
            parsed.command,
            EngineCommand::Complete {
                buffer: None,
                cwd: None,
                specs_dir: None,
                compare: false,
                session: Some(PathBuf::from("tests/dual-path/sessions/git.jsonl")),
                repos_dir: Some(PathBuf::from("tests/dual-path/repos")),
            }
        );
    }
}
