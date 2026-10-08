//! Where the model's commands run.
//!
//! The core only ever talks to a [`Sandbox`]. On iOS that is the iSH kernel in
//! the app's process; on a development machine it is the host's own shell.
//! Both present the same guest layout, so a path the model sees means the
//! same thing everywhere:
//!
//! - `/solos/ws` — the workspace: the default working directory for the
//!   model's commands. The user's terminal opens at home and reaches it by
//!   `cd /solos/ws`.

pub mod host;

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The workspace, as the guest sees it.
pub const GUEST_WORKSPACE: &str = "/solos/ws";

#[derive(Debug, Clone, thiserror::Error)]
pub enum SandboxError {
    #[error("the sandbox is not available: {0}")]
    Unavailable(String),
    #[error("could not start the command: {0}")]
    Spawn(String),
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub struct ExecSpec {
    /// A shell script, run by the guest's `/bin/sh`.
    pub script: String,
    /// Guest path to run in.
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// `None` when the command was killed (timeout or cancellation).
    pub exit_code: Option<i32>,
    /// stdout and stderr interleaved as they arrived.
    pub output: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

/// Receives a terminal's output.
pub type TerminalOutput = Arc<dyn Fn(Vec<u8>) + Send + Sync>;
/// Told a terminal's shell has ended, with its exit code when it has one.
pub type TerminalExited = Box<dyn FnOnce(Option<i32>) + Send>;

/// An open terminal.
pub trait Terminal: Send + Sync {
    /// Keystrokes and pastes, in order. Never drops bytes: what the shell is
    /// not ready for waits.
    fn input(&self, bytes: Vec<u8>);
    fn resize(&self, rows: u16, cols: u16);
    /// End the shell and everything it started.
    fn close(&self);
}

/// Receives output as it is produced, for live display.
pub type OutputSink = Arc<dyn Fn(&str) + Send + Sync>;

/// A long-running process with piped stdin and stdout, such as an MCP
/// server: what it reads is written in order, what it prints arrives in
/// chunks, and its stdout ending means it has ended.
pub struct Process {
    pub stdin: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub stdout: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    /// The end of what it wrote to stderr, for saying why it failed.
    pub stderr: Arc<std::sync::Mutex<String>>,
    /// Ends it and everything it started.
    pub kill: Arc<dyn Fn() + Send + Sync>,
}

impl Process {
    /// How much of stderr is kept.
    pub const STDERR_TAIL: usize = 4_000;

    /// A process from its three blocking pipes. Each is served by a thread
    /// of its own, since a server lives for as long as the engine does.
    pub fn from_pipes(
        mut stdin: impl std::io::Write + Send + 'static,
        mut stdout: impl std::io::Read + Send + 'static,
        mut stderr: impl std::io::Read + Send + 'static,
        kill: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Some(bytes) = in_rx.blocking_recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                if out_tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let tail = Arc::new(std::sync::Mutex::new(String::new()));
        let t = tail.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 4096];
            while let Ok(n @ 1..) = stderr.read(&mut buf) {
                let mut s = t.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > Self::STDERR_TAIL * 2 {
                    let mut cut = s.len() - Self::STDERR_TAIL;
                    while !s.is_char_boundary(cut) {
                        cut += 1;
                    }
                    s.drain(..cut);
                }
            }
        });
        Self { stdin: in_tx, stdout: out_rx, stderr: tail, kill }
    }

    pub fn stderr_tail(&self) -> String {
        let s = self.stderr.lock().unwrap();
        let mut cut = s.len().saturating_sub(Self::STDERR_TAIL);
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        s[cut..].to_string()
    }
}

#[derive(Debug, Clone)]
pub struct SandboxInfo {
    /// For the system prompt: what kind of machine the model is on.
    pub description: String,
    /// For the system prompt: what this sandbox has, lacks and does
    /// differently, one fact per line. Each line must hold when checked in
    /// this sandbox; the sandbox owns them because only it can check them.
    pub notes: String,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Make the sandbox ready. Idempotent; cheap once booted.
    async fn boot(&self) -> Result<(), SandboxError>;

    /// Run a script to completion, streaming its output to `sink`.
    async fn exec(
        &self,
        spec: ExecSpec,
        sink: OutputSink,
        cancel: CancellationToken,
    ) -> Result<ExecResult, SandboxError>;

    fn info(&self) -> SandboxInfo;

    /// A login shell on a terminal of `rows` × `cols`, at home.
    /// Everything it prints goes to `output`, in order and complete, in
    /// chunks as large as have piled up; `exited` is called once, when the
    /// shell ends. Sandboxes without terminals say so.
    async fn open_terminal(
        &self,
        rows: u16,
        cols: u16,
        output: TerminalOutput,
        exited: TerminalExited,
    ) -> Result<Arc<dyn Terminal>, SandboxError> {
        let _ = (rows, cols, output, exited);
        Err(SandboxError::Unavailable("this sandbox has no terminal".into()))
    }

    /// Start `command` (a shell command line) as a long-running process
    /// in `cwd`, with `env` after the sandbox's own variables. Sandboxes
    /// that cannot keep one say so.
    async fn spawn(&self, command: String, cwd: String, env: Vec<(String, String)>) -> Result<Process, SandboxError> {
        let _ = (command, cwd, env);
        Err(SandboxError::Unavailable("this sandbox cannot keep a process running".into()))
    }

    /// Variables every process started from now on gets, after the
    /// sandbox's own and before a script's: how `solos` reaches the tools.
    fn set_guest_env(&self, env: Vec<(String, String)>) {
        let _ = env;
    }

    /// A guest file the sandbox keeps as given (`path`, what it says):
    /// written at every boot, and at once when it runs, with any missing
    /// folders. For the package mirrors' config. Sandboxes without a guest
    /// system of their own ignore it.
    async fn set_boot_file(&self, _path: String, _content: String) -> Result<(), SandboxError> {
        Ok(())
    }

    /// Whether this start unpacked the guest's system afresh (a first
    /// launch, or a new system in an update): when the package mirrors are
    /// chosen on their own. Known once booted.
    fn freshly_installed(&self) -> bool {
        false
    }

    /// The guest's release as its repositories name it (`v3.21`), for
    /// fetching the right index; `None` without a package manager.
    fn package_branch(&self) -> Option<String> {
        None
    }

    /// The host folder `/solos/ws` shows, for the app to open files the
    /// model made.
    fn workspace_dir(&self) -> std::path::PathBuf;

    /// The host folder that holds everything of the guest's own system
    /// (its files and their metadata), for showing how much room it takes.
    fn system_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// The host folder that holds the guest's `/`, for looking around it.
    /// Only the workspace inside it may be changed from the host: the rest
    /// belongs to the guest's own filesystem, which keeps its metadata
    /// elsewhere and would not know.
    fn guest_root_dir(&self) -> Option<std::path::PathBuf> {
        None
    }
}
