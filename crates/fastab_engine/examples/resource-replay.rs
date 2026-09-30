//! One EngineClient per JSONL replay. No command, path, environment, suggestion
//! or error text is written to reports. See docs/completion-resource-lifecycle-plan.md.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use fastab_engine::{
    CompleteRequest, CompleteResult, CompletionCancelled, CompletionTask, EngineClient, EngineClientOptions, SessionId,
};
use fastab_settings::JsonStore;
use futures::{StreamExt, task::LocalSpawnExt};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Complete {
        #[serde(default)]
        session: u64,
        request: CompleteRequest,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
    },
    Submit {
        id: String,
        #[serde(default)]
        session: u64,
        request: CompleteRequest,
    },
    Await {
        id: String,
        #[serde(default)]
        outcome: Outcome,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
    },
    EndInput {
        #[serde(default)]
        session: u64,
    },
    Wait {
        ms: u64,
    },
    WaitForFile {
        path: PathBuf,
        #[serde(default = "wait_timeout")]
        timeout_ms: u64,
    },
    Snapshot {
        #[serde(default)]
        expect_engine: Option<bool>,
        #[serde(default)]
        equals: HashMap<String, Value>,
        #[serde(default)]
        timeout_ms: u64,
    },
    ClearCaches,
}

fn wait_timeout() -> u64 {
    5000
}

#[derive(Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    #[default]
    Success,
    Cancelled,
    Failed,
    Any,
}

struct Observation {
    elapsed_us: u128,
    result: anyhow::Result<CompleteResult>,
}
struct Watch {
    task: CompletionTask,
    started: Instant,
    reply: mpsc::Sender<Observation>,
}

// A single observer executor timestamps future resolution even while the
// driver waits for a fixture handshake or runs a later step. Await order must
// not inflate one request's measured latency with another request's delay.
struct Observer {
    tx: Option<futures::channel::mpsc::UnboundedSender<Watch>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Observer {
    fn new() -> anyhow::Result<Self> {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Watch>();
        let thread = std::thread::Builder::new()
            .name("replay-observer".into())
            .spawn(move || {
                let mut pool = futures::executor::LocalPool::new();
                let spawner = pool.spawner();
                pool.run_until(async move {
                    while let Some(watch) = rx.next().await {
                        spawner
                            .spawn_local(async move {
                                let result = watch.task.await;
                                let _ = watch.reply.send(Observation {
                                    elapsed_us: watch.started.elapsed().as_micros(),
                                    result,
                                });
                            })
                            .expect("observer executor is alive");
                    }
                });
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }
    fn submit(
        &self,
        client: &EngineClient,
        session: u64,
        request: CompleteRequest,
    ) -> anyhow::Result<mpsc::Receiver<Observation>> {
        let started = Instant::now();
        let task = client.complete_for_session(SessionId::new(session.into()), request);
        let (reply, rx) = mpsc::channel();
        self.tx
            .as_ref()
            .context("observer closed")?
            .unbounded_send(Watch { task, started, reply })
            .map_err(|_closed| anyhow::anyhow!("observer closed"))?;
        Ok(rx)
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct Metrics {
    successful_us: Vec<u128>,
    cancelled: usize,
    failed: usize,
}
impl Metrics {
    fn record(
        &mut self,
        observation: Observation,
        expected: Outcome,
        contains: &[String],
        absent: &[String],
    ) -> anyhow::Result<Value> {
        let elapsed_us = observation.elapsed_us;
        let (outcome, status, count) = match observation.result {
            Ok(result) => {
                self.successful_us.push(elapsed_us);
                ensure!(
                    contains
                        .iter()
                        .all(|name| result.suggestions.iter().any(|row| &row.name == name)),
                    "required suggestion missing"
                );
                ensure!(
                    absent
                        .iter()
                        .all(|name| result.suggestions.iter().all(|row| &row.name != name)),
                    "unexpected suggestion present"
                );
                (Outcome::Success, "success", Some(result.suggestions.len()))
            },
            Err(error) if error.is::<CompletionCancelled>() => {
                self.cancelled += 1;
                (Outcome::Cancelled, "cancelled", None)
            },
            Err(_) => {
                self.failed += 1;
                (Outcome::Failed, "failed", None)
            },
        };
        ensure!(
            expected == Outcome::Any || expected == outcome,
            "unexpected completion outcome"
        );
        ensure!(
            outcome == Outcome::Success || (contains.is_empty() && absent.is_empty()),
            "suggestion assertions require success"
        );
        Ok(json!({"status":status,"elapsed_us":elapsed_us,"suggestions":count}))
    }
    fn summary(&mut self) -> Value {
        self.successful_us.sort_unstable();
        let percentile = |percent: usize| -> Option<u128> {
            if self.successful_us.is_empty() {
                return None;
            }
            let index = (self.successful_us.len() * percent).div_ceil(100).saturating_sub(1);
            self.successful_us.get(index).copied()
        };
        json!({"op":"summary", "successful":self.successful_us.len(),
            "cancelled":self.cancelled,"failed":self.failed,
            "success_p50_us":percentile(50),"success_p95_us":percentile(95),"success_p99_us":percentile(99)})
    }
}

fn footprint(enabled: bool) -> Value {
    if !enabled {
        return Value::Null;
    }
    #[cfg(target_os = "macos")]
    {
        // Read this process's ledger directly. Spawning a diagnostic child
        // during every sample can perturb the allocator we are measuring.
        let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: the flavor matches the fully sized output buffer; only read
        // initialized fields after a successful kernel call for our own PID.
        let status = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                usage.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            return json!({"available":false});
        }
        let usage = unsafe { usage.assume_init() };
        json!({"phys_footprint_bytes":usage.ri_phys_footprint,
            "phys_footprint_peak_bytes":usage.ri_lifetime_max_phys_footprint})
    }
    #[cfg(not(target_os = "macos"))]
    Value::Null
}

fn self_check_fixture() -> anyhow::Result<tempfile::TempDir> {
    let fixture = tempfile::tempdir()?;
    let specs = fixture.path().join("specs");
    std::fs::create_dir(&specs)?;
    std::fs::write(
        specs.join("tool.json"),
        r#"{"names":["tool"],"subcommands":[{"names":["child"],"loadSpec":"child"}]}"#,
    )?;
    std::fs::write(
        specs.join("child.json"),
        r#"{"names":["child"],"subcommands":[{"names":["value"]}]}"#,
    )?;
    let ready = fixture.path().join("ready");
    std::fs::write(
        specs.join("slow.json"),
        serde_json::to_vec(&json!({
            "names":["slow"],"args":[{"script":["/bin/sh","-c","printf ready > \"$1\"; sleep 30","replay",ready]}]
        }))?,
    )?;
    let request = |buffer: &str| json!({"buffer":buffer,"cwd":"/","include_history":false});
    let scenario = vec![
        json!({"op":"snapshot","expect_engine":false}),
        json!({"op":"end_input","session":1}),
        json!({"op":"snapshot","expect_engine":false}),
        json!({"op":"complete","session":1,"request":request("tool child "),"contains":["value"]}),
        json!({"op":"end_input","session":2}),
        json!({"op":"snapshot","equals":{"/engine/registry/idle_file_count":0}}),
        json!({"op":"end_input","session":1}),
        json!({"op":"end_input","session":1}),
        json!({"op":"snapshot","timeout_ms":3000,"equals":{"/engine/registry/cached_file_count":0}}),
        json!({"op":"complete","session":1,"request":request("tool child "),"contains":["value"]}),
        json!({"op":"submit","id":"slow","session":1,"request":request("slow ")}),
        json!({"op":"wait_for_file","path":ready}),
        json!({"op":"submit","id":"latest","session":2,"request":request("tool child ")}),
        json!({"op":"await","id":"slow","outcome":"cancelled"}),
        json!({"op":"await","id":"latest","contains":["value"]}),
        json!({"op":"clear_caches"}),
        json!({"op":"snapshot","expect_engine":true,"equals":{"/engine/registry/cached_file_count":0}}),
    ];
    std::fs::write(
        fixture.path().join("scenario.jsonl"),
        scenario.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"),
    )?;
    Ok(fixture)
}

fn run() -> anyhow::Result<()> {
    // Process-local global backend, visible on attempt threads too. Never
    // write the user's settings, load shell history or run a custom history
    // command as a side effect of a deterministic resource replay.
    *fastab_settings::OldSettings::data_lock().write() = Some(
        [
            ("autocomplete.history.disableLoading".into(), json!(true)),
            ("autocomplete.sortMethod".into(), json!("default")),
            ("autocomplete.hideAutoExecuteSuggestion".into(), json!(true)),
            ("autocomplete.scriptTimeout".into(), json!(5000)),
        ]
        .into_iter()
        .collect(),
    );
    let mut args = std::env::args().skip(1);
    let first = args.next().context("missing replay arguments")?;
    let fixture = (first == "--self-check").then(self_check_fixture).transpose()?;
    let (specs, scenario) = if let Some(fixture) = &fixture {
        (fixture.path().join("specs"), fixture.path().join("scenario.jsonl"))
    } else {
        (
            PathBuf::from(first),
            PathBuf::from(args.next().context("missing scenario")?),
        )
    };
    let mut options = EngineClientOptions::default();
    if fixture.is_some() {
        options.spec_idle_grace = Duration::from_millis(100);
    }
    let mut measure_footprint = false;
    for arg in args {
        if arg == "--footprint" {
            measure_footprint = true;
        } else {
            options.spec_idle_grace = Duration::from_millis(arg.parse().context("invalid grace milliseconds")?);
        }
    }
    let client = EngineClient::spawn_with_options(specs, options)?;
    let observer = Observer::new()?;
    let mut pending = HashMap::new();
    let mut metrics = Metrics::default();
    let file = std::fs::File::open(scenario).context("cannot open scenario")?;
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.context("cannot read scenario")?;
        if line.trim().is_empty() {
            continue;
        }
        let step: Step = serde_json::from_str(&line)
            .map_err(|_invalid| anyhow::anyhow!("invalid scenario at step {}", index + 1))?;
        let result = match step {
            Step::Complete {
                session,
                request,
                contains,
                absent,
            } => {
                let observation = observer
                    .submit(&client, session, request)?
                    .recv()
                    .context("observer stopped")?;
                metrics.record(observation, Outcome::Success, &contains, &absent)?
            },
            Step::Submit { id, session, request } => {
                ensure!(!pending.contains_key(&id), "duplicate pending request");
                pending.insert(id, observer.submit(&client, session, request)?);
                json!({"status":"submitted"})
            },
            Step::Await {
                id,
                outcome,
                contains,
                absent,
            } => {
                let observation = pending
                    .remove(&id)
                    .context("unknown pending request")?
                    .recv()
                    .context("observer stopped")?;
                metrics.record(observation, outcome, &contains, &absent)?
            },
            Step::EndInput { session } => {
                client.end_input(SessionId::new(session.into()))?;
                json!({"status":"input_ended"})
            },
            Step::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(ms));
                json!({"status":"waited","ms":ms})
            },
            Step::WaitForFile { path, timeout_ms } => {
                let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                while !path.is_file() {
                    ensure!(Instant::now() < deadline, "fixture handshake timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                json!({"status":"handshake"})
            },
            Step::Snapshot {
                expect_engine,
                equals,
                timeout_ms,
            } => {
                let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                let diagnostics = loop {
                    let diagnostics = futures::executor::block_on(client.diagnostics())?;
                    let engine_matches = expect_engine.is_none_or(|expected| diagnostics.engine.is_some() == expected);
                    let diagnostics = serde_json::to_value(diagnostics)?;
                    if engine_matches
                        && equals
                            .iter()
                            .all(|(pointer, expected)| diagnostics.pointer(pointer) == Some(expected))
                    {
                        break diagnostics;
                    }
                    ensure!(Instant::now() < deadline, "diagnostic assertion failed");
                    std::thread::sleep(Duration::from_millis(10));
                };
                json!({"status":"snapshot","diagnostics":diagnostics,"memory":footprint(measure_footprint)})
            },
            Step::ClearCaches => {
                client.clear_caches()?;
                json!({"status":"caches_cleared"})
            },
        };
        println!("{}", json!({"step":index + 1,"result":result}));
    }
    if !pending.is_empty() {
        bail!("unobserved requests at end of scenario");
    }
    println!("{}", metrics.summary());
    Ok(())
}

fn main() {
    if run().is_err() {
        // Engine errors may contain script stderr or environment/path values.
        eprintln!("resource replay failed; inspect scenario assertions and numeric step output");
        std::process::exit(1);
    }
}
