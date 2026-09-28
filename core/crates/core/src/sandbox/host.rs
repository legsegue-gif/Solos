//! The host machine's own shell, standing in for the guest on macOS and
//! Linux: tests, the desktop CLI, and development without a simulator.
//!
//! Guest paths under `/solos` are mapped onto a directory on the host, so the
//! model's `cd /solos/ws` lands in a real folder.

use super::{ExecResult, ExecSpec, OutputSink, Sandbox, SandboxError, SandboxInfo, Terminal, TerminalExited, TerminalOutput};
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub struct HostSandbox {
    /// What `/solos` means on this machine.
    root: PathBuf,
    guest_env: std::sync::Mutex<Vec<(String, String)>>,
}

impl HostSandbox {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), guest_env: Default::default() }
    }

    /// Map a guest path under `/solos` to the host. Other paths pass through.
    fn host_path(&self, guest: &str) -> PathBuf {
        match guest.strip_prefix("/solos") {
            Some(rest) => self.root.join(rest.trim_start_matches('/')),
            None => PathBuf::from(guest),
        }
    }

    fn rewrite(&self, script: &str) -> String {
        // The model writes guest paths; on the host they have to point into
        // `root`. A plain prefix swap covers the paths it is told about.
        script.replace("/solos/", &format!("{}/", self.root.display()))
    }
}

#[async_trait]
impl Sandbox for HostSandbox {
    async fn boot(&self) -> Result<(), SandboxError> {
        let ws = self.host_path(super::GUEST_WORKSPACE);
        tokio::fs::create_dir_all(&ws)
            .await
            .map_err(|e| SandboxError::Unavailable(format!("{}: {e}", ws.display())))
    }

    async fn exec(
        &self,
        spec: ExecSpec,
        sink: OutputSink,
        cancel: CancellationToken,
    ) -> Result<ExecResult, SandboxError> {
        let cwd = self.host_path(&spec.cwd);
        let mut spec = spec;
        let mut env = self.guest_env.lock().unwrap().clone();
        env.append(&mut spec.env);
        spec.env = env;
        run(&cwd, &self.rewrite(&spec.script), &spec, sink, cancel).await
    }

    fn set_guest_env(&self, env: Vec<(String, String)>) {
        *self.guest_env.lock().unwrap() = env;
    }

    fn info(&self) -> SandboxInfo {
        SandboxInfo {
            description: format!("the host's shell ({}), standing in for the device sandbox", std::env::consts::OS),
            notes: String::new(),
        }
    }

    /// A shell on a real pty, by way of `script(1)`, which every macOS and
    /// Linux has. Enough for tests and the desktop; it cannot be resized,
    /// and it opens in the workspace, since the host has no guest home.
    /// On macOS the shell is zsh without rc files: `/bin/sh` there is bash
    /// 3.2, whose line editor loses parts of a large paste when the machine
    /// is busy (10 of 64 runs of a 40 KB heredoc; zsh and bash without line
    /// editing lost none).
    async fn open_terminal(&self, _rows: u16, _cols: u16, output: TerminalOutput, exited: TerminalExited) -> Result<Arc<dyn Terminal>, SandboxError> {
        self.boot().await?;
        let mut cmd = Command::new("script");
        if cfg!(target_os = "macos") {
            cmd.args(["-q", "/dev/null", "/bin/zsh", "-f"]);
        } else {
            cmd.args(["-qfc", "/bin/sh", "/dev/null"]);
        }
        let mut child = cmd
            .current_dir(self.host_path(super::GUEST_WORKSPACE))
            .envs(self.guest_env.lock().unwrap().iter().cloned())
            .env("PS1", "$ ")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .map_err(|e| SandboxError::Spawn(e.to_string()))?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut stdin = child.stdin.take().expect("piped");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<Vec<u8>>>();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    Some(bytes) => {
                        if stdin.write_all(&bytes).await.is_err() {
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => break,
                }
            }
        });
        let pid = child.id();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => output(buf[..n].to_vec()),
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code());
            exited(code);
        });
        Ok(Arc::new(HostTerminal { input: tx, pid: Mutex::new(pid) }))
    }

    fn workspace_dir(&self) -> PathBuf {
        self.host_path(super::GUEST_WORKSPACE)
    }
}

async fn run(
    cwd: &Path,
    script: &str,
    spec: &ExecSpec,
    sink: OutputSink,
    cancel: CancellationToken,
) -> Result<ExecResult, SandboxError> {
    let mut child = Command::new("/bin/sh")
        .current_dir(cwd)
        .envs(spec.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // One stream, in the order it was written: redirect in the shell.
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()
        .map_err(|e| SandboxError::Spawn(e.to_string()))?;

    let mut stdin = child.stdin.take().expect("piped");
    let body = format!("exec 2>&1\n{script}\n");
    stdin.write_all(body.as_bytes()).await.map_err(|e| SandboxError::Io(e.to_string()))?;
    drop(stdin);

    let mut stdout = child.stdout.take().expect("piped");
    // Kept outside the reader so a timeout or a cancel still returns what the
    // command had printed by then — often the part that says why it hung.
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink_buf = collected.clone();
    let reader = async move {
        let mut buf = [0u8; 8192];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    sink(&String::from_utf8_lossy(&buf[..n]));
                    sink_buf.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        }
    };
    let so_far = || String::from_utf8_lossy(&collected.lock().unwrap()).into_owned();

    let pgid = child.id();
    let kill_group = || {
        if let Some(pid) = pgid {
            // The whole process group, so a pipeline does not outlive us.
            unsafe { libc_kill(-(pid as i32), 9) };
        }
    };

    tokio::select! {
        status = async { reader.await; child.wait().await } => {
            Ok(ExecResult {
                exit_code: status.ok().and_then(|s| s.code()),
                output: so_far(),
                timed_out: false,
                cancelled: false,
            })
        }
        _ = tokio::time::sleep(spec.timeout) => {
            kill_group();
            Ok(ExecResult { exit_code: None, output: so_far(), timed_out: true, cancelled: false })
        }
        _ = cancel.cancelled() => {
            kill_group();
            Ok(ExecResult { exit_code: None, output: so_far(), timed_out: false, cancelled: true })
        }
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn spec(script: &str, timeout: u64) -> ExecSpec {
        ExecSpec { script: script.into(), cwd: "/solos/ws".into(), env: vec![], timeout: Duration::from_secs(timeout) }
    }

    fn sandbox() -> HostSandbox {
        HostSandbox::new(std::env::temp_dir().join(format!("solos-host-{}", uuid::Uuid::new_v4())))
    }

    #[tokio::test]
    async fn runs_in_the_workspace_and_merges_stderr() {
        let sb = sandbox();
        sb.boot().await.unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let s2 = seen.clone();
        let sink: OutputSink = Arc::new(move |c| s2.lock().unwrap().push_str(c));
        let r = sb
            .exec(spec("echo out; echo err >&2; basename \"$(pwd)\"; exit 3", 10), sink, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.exit_code, Some(3));
        assert_eq!(r.output, "out\nerr\nws\n");
        assert_eq!(*seen.lock().unwrap(), r.output);
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command() {
        let sb = sandbox();
        sb.boot().await.unwrap();
        let started = std::time::Instant::now();
        let r = sb.exec(spec("sleep 30", 1), Arc::new(|_| {}), CancellationToken::new()).await.unwrap();
        assert!(r.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

struct HostTerminal {
    input: tokio::sync::mpsc::UnboundedSender<Option<Vec<u8>>>,
    pid: Mutex<Option<u32>>,
}

impl Terminal for HostTerminal {
    fn input(&self, bytes: Vec<u8>) {
        let _ = self.input.send(Some(bytes));
    }
    fn resize(&self, _rows: u16, _cols: u16) {}
    fn close(&self) {
        let _ = self.input.send(None);
        if let Some(pid) = self.pid.lock().unwrap().take() {
            // The whole group: `script`, the shell, and what it started.
            let _ = std::process::Command::new("kill").args(["-9", &format!("-{pid}")]).status();
        }
    }
}
