//! Background commands: `shell` with `background: true`, and `shell_jobs`.
//!
//! A job outlives the call that started it and the turn that made the
//! call, so a long download or build does not hold the conversation. It
//! does not outlive the app: jobs are kept in memory only, and a restart
//! starts with an empty list rather than ids that point at nothing.

use super::{object_schema, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::sandbox::{ExecResult, ExecSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Output kept per job: a job that prints without end still has to fit in
/// memory, and the end is the part anyone reads.
const KEEP_OUTPUT: usize = 200_000;
/// What `output` returns at most, from the end.
const SHOW_OUTPUT: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Exited(i32),
    TimedOut,
    Killed,
}

impl JobState {
    fn label(&self) -> String {
        match self {
            JobState::Running => "running".into(),
            JobState::Exited(0) => "finished (exit code 0)".into(),
            JobState::Exited(c) => format!("failed (exit code {c})"),
            JobState::TimedOut => "killed at its timeout".into(),
            JobState::Killed => "stopped".into(),
        }
    }
}

struct Job {
    session_id: String,
    command: String,
    started: Instant,
    ended: Option<Instant>,
    state: JobState,
    output: String,
    dropped: bool,
    cancel: CancellationToken,
}

#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<BTreeMap<u64, Job>>,
    next: AtomicU64,
}

impl Jobs {
    /// Start `spec` in the background and return its id at once.
    pub fn start(self: &Arc<Self>, ctx: &ToolContext, command: &str, spec: ExecSpec) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        // Its own token: stopping the turn must not stop the job.
        let cancel = CancellationToken::new();
        self.jobs.lock().unwrap().insert(
            id,
            Job {
                session_id: ctx.session_id.clone(),
                command: command.to_string(),
                started: Instant::now(),
                ended: None,
                state: JobState::Running,
                output: String::new(),
                dropped: false,
                cancel: cancel.clone(),
            },
        );
        let jobs = self.clone();
        let sink_jobs = self.clone();
        let sandbox = ctx.sandbox.clone();
        tokio::spawn(async move {
            let sink = Arc::new(move |chunk: &str| sink_jobs.append(id, chunk));
            let result = sandbox.exec(spec, sink, cancel).await;
            jobs.finish(id, result);
        });
        id
    }

    fn append(&self, id: u64, chunk: &str) {
        let mut map = self.jobs.lock().unwrap();
        let Some(job) = map.get_mut(&id) else { return };
        job.output.push_str(chunk);
        if job.output.len() > KEEP_OUTPUT {
            let mut cut = job.output.len() - KEEP_OUTPUT;
            while !job.output.is_char_boundary(cut) {
                cut += 1;
            }
            // Start the kept part at a line when one is near.
            if let Some(nl) = job.output[cut..].find('\n').filter(|n| *n < 4096) {
                cut += nl + 1;
            }
            job.output.drain(..cut);
            job.dropped = true;
        }
    }

    fn finish(&self, id: u64, result: Result<ExecResult, crate::sandbox::SandboxError>) {
        let mut map = self.jobs.lock().unwrap();
        let Some(job) = map.get_mut(&id) else { return };
        job.ended = Some(Instant::now());
        job.state = match result {
            _ if job.state == JobState::Killed => JobState::Killed,
            Ok(r) if r.cancelled => JobState::Killed,
            Ok(r) if r.timed_out => JobState::TimedOut,
            Ok(r) => JobState::Exited(r.exit_code.unwrap_or(-1)),
            Err(e) => {
                job.output.push_str(&format!("\n[the command could not run: {e}]"));
                JobState::Exited(-1)
            }
        };
    }

    fn kill(&self, session_id: &str, id: u64) -> Result<(), String> {
        let mut map = self.jobs.lock().unwrap();
        match map.get_mut(&id).filter(|j| j.session_id == session_id) {
            None => Err(format!("There is no job {id} in this chat.")),
            Some(j) if j.state != JobState::Running => Err(format!("Job {id} is not running: {}.", j.state.label())),
            Some(j) => {
                j.state = JobState::Killed;
                j.cancel.cancel();
                Ok(())
            }
        }
    }

    fn describe(id: u64, j: &Job) -> String {
        let secs = j.ended.unwrap_or_else(Instant::now).duration_since(j.started).as_secs();
        let first = j.command.lines().next().unwrap_or("");
        format!("job {id} · {} · {secs}s · {first}", j.state.label())
    }
}

/// `shell_jobs`: see and stop what `shell` started in the background.
pub struct ShellJobs(pub Arc<Jobs>);

#[async_trait]
impl Tool for ShellJobs {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell_jobs".into(),
            description: "See and stop the commands this chat started with `shell` and background: true. \
                `list` gives each job's id, state, running time and command; `output` gives what a job has printed \
                so far (the end of it, when long); `kill` stops one. Jobs end when the app is closed."
                .into(),
            schema: object_schema(
                json!({
                    "action": {"type": "string", "enum": ["list", "output", "kill"]},
                    "job": {"type": "integer", "description": "The job id, for output and kill."}
                }),
                &["action"],
            ),
            parallel: true,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let job = input.get("job").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
        match input.get("action").and_then(Value::as_str) {
            Some("list") => {
                let map = self.0.jobs.lock().unwrap();
                let lines: Vec<String> =
                    map.iter().rev().filter(|(_, j)| j.session_id == ctx.session_id).map(|(id, j)| Jobs::describe(*id, j)).collect();
                if lines.is_empty() {
                    ToolOutput::ok("No background jobs in this chat.")
                } else {
                    ToolOutput::ok(lines.join("\n"))
                }
            }
            Some("output") => {
                let Some(id) = job else { return ToolOutput::error("`job` is required for output.") };
                let map = self.0.jobs.lock().unwrap();
                let Some(j) = map.get(&id).filter(|j| j.session_id == ctx.session_id) else {
                    return ToolOutput::error(format!("There is no job {id} in this chat."));
                };
                let count = j.output.chars().count();
                let body: String = j.output.chars().skip(count.saturating_sub(SHOW_OUTPUT)).collect();
                let cut = j.dropped || count > SHOW_OUTPUT;
                let head = format!("{}{}", Jobs::describe(id, j), if cut { " · earlier output not shown" } else { "" });
                ToolOutput::ok(if body.is_empty() { format!("{head}\n[no output yet]") } else { format!("{head}\n{body}") })
            }
            Some("kill") => {
                let Some(id) = job else { return ToolOutput::error("`job` is required for kill.") };
                match self.0.kill(&ctx.session_id, id) {
                    Ok(()) => ToolOutput::ok(format!("Stopped job {id}.")),
                    Err(e) => ToolOutput::error(e),
                }
            }
            _ => ToolOutput::error("`action` must be list, output or kill."),
        }
    }
}

/// How long a background job may run before it is killed, when the call
/// does not say: long enough for any download or build, short enough that a
/// forgotten `tail -f` does not run for the life of the app.
pub const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;

    fn ctx(session: &str, sandbox: Arc<HostSandbox>) -> ToolContext {
        std::fs::create_dir_all(crate::sandbox::Sandbox::workspace_dir(&*sandbox)).unwrap();
        ToolContext { session_id: session.into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }
    }

    fn spec(script: &str) -> ExecSpec {
        ExecSpec { script: script.into(), cwd: crate::sandbox::GUEST_WORKSPACE.into(), env: vec![], timeout: BACKGROUND_TIMEOUT }
    }

    async fn until_done(tool: &ShellJobs, c: &ToolContext, id: u64) -> String {
        for _ in 0..100 {
            let out = tool.call(c, &json!({"action": "output", "job": id})).await.text;
            if !out.contains("· running ·") {
                return out;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job {id} did not finish");
    }

    #[tokio::test]
    async fn a_job_runs_on_after_the_call_and_reports_how_it_ended() {
        let dir = std::env::temp_dir().join(format!("solos-jobs-{}", uuid::Uuid::new_v4()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        let jobs = Arc::new(Jobs::default());
        let tool = ShellJobs(jobs.clone());
        let a = ctx("a", sandbox.clone());
        let id = jobs.start(&a, "echo one; sleep 0.2; echo two; exit 3", spec("echo one; sleep 0.2; echo two; exit 3"));
        let listed = tool.call(&a, &json!({"action": "list"})).await.text;
        assert!(listed.contains(&format!("job {id} · running")), "{listed}");
        let out = until_done(&tool, &a, id).await;
        assert!(out.contains("failed (exit code 3)") && out.ends_with("one\ntwo\n"), "{out}");

        // Another chat neither sees nor stops it.
        let b = ctx("b", sandbox.clone());
        assert_eq!(tool.call(&b, &json!({"action": "list"})).await.text, "No background jobs in this chat.");
        assert!(tool.call(&b, &json!({"action": "kill", "job": id})).await.is_error);
    }

    #[tokio::test]
    async fn a_killed_job_says_so_and_output_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("solos-jobs-{}", uuid::Uuid::new_v4()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        let jobs = Arc::new(Jobs::default());
        let tool = ShellJobs(jobs.clone());
        let c = ctx("a", sandbox);
        let id = jobs.start(&c, "sleep 30", spec("sleep 30"));
        assert!(!tool.call(&c, &json!({"action": "kill", "job": id})).await.is_error);
        let out = until_done(&tool, &c, id).await;
        assert!(out.contains("· stopped ·"), "{out}");

        for i in 0..30_000 {
            jobs.append(id, &format!("line {i}\n"));
        }
        let map = jobs.jobs.lock().unwrap();
        let j = map.get(&id).unwrap();
        assert!(j.dropped && j.output.len() <= KEEP_OUTPUT && j.output.ends_with("line 29999\n"));
    }
}
