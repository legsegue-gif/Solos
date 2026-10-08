//! The sandbox on iOS: the iSH kernel running Alpine Linux in the app's own
//! process.
//!
//! Every call into the kernel blocks, so each one runs on a blocking thread;
//! nothing here parks an async executor.

mod ffi;

use async_trait::async_trait;
use solos_core::sandbox::{
    ExecResult, ExecSpec, OutputSink, Sandbox, SandboxError, SandboxInfo, Terminal, TerminalExited, TerminalOutput, GUEST_WORKSPACE,
};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const GUEST_ENV: &[&str] = &[
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    "HOME=/root",
    "USER=root",
    "SHELL=/bin/sh",
    "TERM=xterm-256color",
    "LANG=C.UTF-8",
    // Node tries a name's IPv6 address first by default; where there is no
    // IPv6 route (measured on this Mac's network) the attempt times out in
    // the emulator and Node often gives up on IPv4 too: 1 of 5 fetches
    // failed with ETIMEDOUT, against 5 of 5 with IPv4 first. And it gives
    // each address 250 ms before trying the next, too little here: 12306-mcp
    // failed to start 1 time in 4 (AggregateError, ETIMEDOUT), and 0 in 4
    // with 2.5 s.
    "NODE_OPTIONS=--dns-result-order=ipv4first --network-family-autoselection-attempt-timeout=2500",
];

/// iSH starts every `node` with `--jitless`, which leaves no WebAssembly,
/// and `--require`s these two files when they exist: a stand-in for the
/// WebAssembly HTTP parser undici loads, and a `fetch` over Node's own
/// `http`. iSH's app lays them over its system (`RootfsPatch.bundle`); ours
/// is plain Alpine, so they are written at every boot. Without them `fetch`
/// throws "WebAssembly is not defined", and with it every npm MCP server
/// that uses it.
const NODE_POLYFILLS: [(&str, &str); 2] = [
    ("/lib/wasm-polyfill.js", include_str!("../../../../deps/ish/app/RootfsPatch.bundle/files/lib/wasm-polyfill.js")),
    ("/lib/fetch-polyfill.js", include_str!("../../../../deps/ish/app/RootfsPatch.bundle/files/lib/fetch-polyfill.js")),
];

/// The fetch polyfill's headers, made to answer as real `fetch`'s do:
/// `get("set-cookie")` joins the cookies into one string, and
/// `getSetCookie()` lists them (12306-mcp reads its cookies that way and
/// failed without it, in the reference app too).
fn node_polyfill(path: &str, content: &str) -> String {
    if path != "/lib/fetch-polyfill.js" {
        return content.to_string();
    }
    content.replacen(
        "          get: k => h[k.toLowerCase()] || null,\n",
        "          get: k => { const v = h[k.toLowerCase()]; return v == null ? null : Array.isArray(v) ? v.join(\", \") : v; },\n          getSetCookie: () => [].concat(h[\"set-cookie\"] || []),\n",
        1,
    )
}

pub struct IshSandbox {
    /// The rootfs zip shipped in the app bundle.
    bundled_rootfs: PathBuf,
    /// Where it is unpacked: holds `alpine-rootfs/{data,meta.db}`.
    root: PathBuf,
    /// The host folder `/solos/ws` shows.
    workspace: PathBuf,
    booted: tokio::sync::Mutex<Option<Result<(), String>>>,
    exits: Arc<ExitRegistry>,
    ptys: Arc<PtyRoutes>,
    guest_env: std::sync::Mutex<Vec<String>>,
    /// Guest files written at every boot, by path: the mirrors' config.
    boot_files: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    /// This launch unpacked the system afresh.
    fresh: Arc<std::sync::atomic::AtomicBool>,
}

impl IshSandbox {
    pub fn new(bundled_rootfs: PathBuf, root: PathBuf, workspace: PathBuf) -> Self {
        Self {
            bundled_rootfs,
            root,
            workspace,
            booted: tokio::sync::Mutex::new(None),
            exits: Arc::new(ExitRegistry::default()),
            ptys: Arc::new(PtyRoutes::default()),
            guest_env: Default::default(),
            boot_files: std::sync::Mutex::new(NODE_POLYFILLS.iter().map(|(p, c)| (p.to_string(), node_polyfill(p, c))).collect()),
            fresh: Default::default(),
        }
    }

    fn rootfs_dir(&self) -> PathBuf {
        self.root.join("alpine-rootfs")
    }

    /// A new guest process's environment, less what the caller adds.
    fn base_env(&self) -> Vec<String> {
        let mut env: Vec<String> = GUEST_ENV.iter().map(|s| s.to_string()).collect();
        env.push(format!("TZ={}", posix_tz()));
        env.extend(self.guest_env.lock().unwrap().iter().cloned());
        env
    }
}

#[async_trait]
impl Sandbox for IshSandbox {
    async fn boot(&self) -> Result<(), SandboxError> {
        let mut state = self.booted.lock().await;
        if let Some(r) = &*state {
            return r.clone().map_err(SandboxError::Unavailable);
        }
        let (zip, root, rootfs, ws, exits, ptys) =
            (self.bundled_rootfs.clone(), self.root.clone(), self.rootfs_dir(), self.workspace.clone(), self.exits.clone(), self.ptys.clone());
        let boot_files = self.boot_files.lock().unwrap().clone();
        let fresh = self.fresh.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            if unpack_once(&zip, &root)? {
                fresh.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
            ffi::install_exit_handler(exits);
            ffi::install_pty_handler(Arc::new(move |id, bytes| ptys.deliver(id, bytes)));
            ffi::boot(&rootfs)?;
            ffi::bind_mount(GUEST_WORKSPACE, &ws)?;
            ffi::set_dns()?;
            ffi::write_guest_file("/etc/profile.d/solos.sh", SHELL_PROFILE.as_bytes(), 0o644)?;
            if !solos_core::bridge::GUEST_CLI.is_empty() {
                ffi::write_guest_file(solos_core::bridge::GUEST_CLI_PATH, solos_core::bridge::GUEST_CLI, 0o755)?;
            }
            for (path, content) in &boot_files {
                ffi::write_guest_file(path, content.as_bytes(), 0o644)?;
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|e| Err(format!("boot thread failed: {e}")));
        if let Err(e) = &result {
            tracing::error!("sandbox boot failed: {e}");
        }
        *state = Some(result.clone());
        result.map_err(SandboxError::Unavailable)
    }

    async fn exec(&self, spec: ExecSpec, sink: OutputSink, cancel: CancellationToken) -> Result<ExecResult, SandboxError> {
        self.boot().await?;
        let script = format!(
            "exec 2>&1 </dev/null\ncd '{}' 2>/dev/null || cd /root\n{}\n",
            spec.cwd.replace('\'', "'\\''"),
            spec.script
        );
        let mut env = self.base_env();
        env.extend(spec.env.iter().map(|(k, v)| format!("{k}={v}")));
        let proc = tokio::task::spawn_blocking(move || ffi::spawn("/bin/sh", &["/bin/sh".to_string()], &env))
            .await
            .map_err(|e| SandboxError::Spawn(e.to_string()))?
            .map_err(SandboxError::Spawn)?;
        let exit = self.exits.register(proc.pid);

        // The script goes in on stdin, so nothing about it needs quoting.
        let stdin = proc.stdin;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut f = stdin;
            let _ = f.write_all(script.as_bytes());
        });

        let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
        let readers: Vec<_> = [proc.stdout, proc.stderr]
            .into_iter()
            .map(|mut f| {
                let (sink, collected) = (sink.clone(), collected.clone());
                tokio::task::spawn_blocking(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match f.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                sink(&String::from_utf8_lossy(&buf[..n]));
                                collected.lock().unwrap().extend_from_slice(&buf[..n]);
                            }
                        }
                    }
                })
            })
            .collect();
        let so_far = || String::from_utf8_lossy(&collected.lock().unwrap()).into_owned();

        let pid = proc.pid;
        let kill = || {
            tokio::task::spawn_blocking(move || ffi::kill_group(pid));
        };
        tokio::select! {
            status = exit => {
                // The pipes close when the last writer exits; drain them.
                for r in readers { let _ = r.await; }
                let code = status.ok().map(wait_status_to_code);
                Ok(ExecResult { exit_code: code, output: so_far(), timed_out: false, cancelled: false })
            }
            _ = tokio::time::sleep(spec.timeout) => {
                kill();
                self.exits.forget(pid);
                Ok(ExecResult { exit_code: None, output: so_far(), timed_out: true, cancelled: false })
            }
            _ = cancel.cancelled() => {
                kill();
                self.exits.forget(pid);
                Ok(ExecResult { exit_code: None, output: so_far(), timed_out: false, cancelled: true })
            }
        }
    }

    fn info(&self) -> SandboxInfo {
        SandboxInfo {
            description: "Alpine Linux 3.21 (aarch64) running in the iSH emulator on this device".into(),
            notes: if solos_core::bridge::GUEST_CLI.is_empty() { NOTES.into() } else { format!("{NOTES}\n{SOLOS_NOTE}") },
        }
    }

    fn set_guest_env(&self, env: Vec<(String, String)>) {
        *self.guest_env.lock().unwrap() = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    }

    async fn spawn(&self, command: String, cwd: String, env: Vec<(String, String)>) -> Result<solos_core::sandbox::Process, SandboxError> {
        self.boot().await?;
        let script = format!("cd '{}' 2>/dev/null || cd /root\nexec {command}", cwd.replace('\'', "'\\''"));
        let mut envp = self.base_env();
        envp.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let proc = tokio::task::spawn_blocking(move || ffi::spawn("/bin/sh", &argv, &envp))
            .await
            .map_err(|e| SandboxError::Spawn(e.to_string()))?
            .map_err(SandboxError::Spawn)?;
        // Its exit is taken off the registry when it comes, so a server
        // that ends leaves nothing behind there.
        let pid = proc.pid;
        let exit = self.exits.register(pid);
        tokio::spawn(async move {
            let _ = exit.await;
        });
        let kill = Arc::new(move || {
            std::thread::spawn(move || ffi::kill_group(pid));
        });
        Ok(solos_core::sandbox::Process::from_pipes(proc.stdin, proc.stdout, proc.stderr, kill))
    }

    fn workspace_dir(&self) -> PathBuf {
        self.workspace.clone()
    }

    async fn set_boot_file(&self, path: String, content: String) -> Result<(), SandboxError> {
        self.boot_files.lock().unwrap().insert(path.clone(), content.clone());
        // Not booted yet: boot writes it. Booted: write it now.
        if !matches!(&*self.booted.lock().await, Some(Ok(()))) {
            return Ok(());
        }
        tokio::task::spawn_blocking(move || ffi::write_guest_file(&path, content.as_bytes(), 0o644))
        .await
        .map_err(|e| SandboxError::Io(e.to_string()))?
        .map_err(|e: String| SandboxError::Io(e))
    }

    fn freshly_installed(&self) -> bool {
        self.fresh.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn package_branch(&self) -> Option<String> {
        alpine_branch(&self.rootfs_dir())
    }

    fn system_dir(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn guest_root_dir(&self) -> Option<PathBuf> {
        Some(self.rootfs_dir().join("data"))
    }

    async fn open_terminal(&self, rows: u16, cols: u16, output: TerminalOutput, exited: TerminalExited) -> Result<Arc<dyn Terminal>, SandboxError> {
        self.boot().await?;
        let env = self.base_env();
        // A login shell at home, as a shell starts and as the reference app
        // opens it; the workspace is `cd /solos/ws` away. A new guest task
        // starts at `/`, so the `cd` has to be said.
        let argv: Vec<String> = ["/bin/sh", "-c", "cd; exec /bin/sh -l"].iter().map(|s| s.to_string()).collect();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let ptys = self.ptys.clone();
        let (pid, pty_id) = tokio::task::spawn_blocking(move || {
            // Output can arrive before the spawn returns the terminal's id;
            // the route for "the terminal being opened" catches it.
            let _opening = ptys.opening(tx.clone());
            let (pid, pty_id) = ffi::spawn_pty("/bin/sh", &argv, &env, rows.max(1), cols.max(1))?;
            ptys.bind(pty_id, tx);
            Ok::<_, String>((pid, pty_id))
        })
        .await
        .map_err(|e| SandboxError::Spawn(e.to_string()))?
        .map_err(SandboxError::Spawn)?;

        // The shell's end: close the terminal, then stop routing to it. The
        // pump below sends whatever is still queued before saying so.
        let (code_tx, code_rx) = oneshot::channel::<Option<i32>>();
        let exit = self.exits.register(pid);
        let ptys = self.ptys.clone();
        tokio::spawn(async move {
            let status = exit.await.ok();
            tokio::task::spawn_blocking(move || ffi::pty_close(pty_id));
            ptys.unbind(pty_id);
            let _ = code_tx.send(status.map(wait_status_to_code));
        });
        // One chunk per wake, of everything already waiting: the kernel
        // hands over a few bytes at a time, thousands of times a second.
        tokio::spawn(async move {
            while let Some(mut chunk) = rx.recv().await {
                while chunk.len() < COALESCE {
                    match rx.try_recv() {
                        Ok(more) => chunk.extend_from_slice(&more),
                        Err(_) => break,
                    }
                }
                output(chunk);
            }
            exited(code_rx.await.ok().flatten());
        });

        // Input, resizes and closing go through one queue on one thread, so
        // a resize never overtakes the keystrokes typed before it.
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<TermCmd>();
        std::thread::Builder::new()
            .name(format!("solos-pty-{pty_id}"))
            .spawn(move || feed_terminal(pid, pty_id, cmd_rx))
            .map_err(|e| SandboxError::Spawn(e.to_string()))?;
        Ok(Arc::new(IshTerminal { cmds: Mutex::new(cmd_tx) }))
    }
}

/// What a login shell in the terminal sets up, rewritten at every boot. The
/// stock prompt shows the host name, which in the emulator is the device's
/// (on a simulator, the Mac's); the aliases are the reference app's; the
/// history file keeps ↑ working across terminals and restarts.
const SHELL_PROFILE: &str = "\
# Written by Solos when the sandbox starts; changes here are overwritten.
export PS1='\\u@solos:\\w\\$ '
export HISTFILE=\"$HOME/.ash_history\" HISTSIZE=1000
alias ll='ls -la' la='ls -A' l='ls -CF'
alias grep='grep --color=auto'
";

/// Largest chunk of terminal output handed over at once.
const COALESCE: usize = 64 * 1024;

enum TermCmd {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}

struct IshTerminal {
    cmds: Mutex<std::sync::mpsc::Sender<TermCmd>>,
}

impl Terminal for IshTerminal {
    fn input(&self, bytes: Vec<u8>) {
        let _ = self.cmds.lock().unwrap().send(TermCmd::Input(bytes));
    }
    fn resize(&self, rows: u16, cols: u16) {
        let _ = self.cmds.lock().unwrap().send(TermCmd::Resize(rows.max(1), cols.max(1)));
    }
    fn close(&self) {
        let _ = self.cmds.lock().unwrap().send(TermCmd::Close);
    }
}

impl Drop for IshTerminal {
    fn drop(&mut self) {
        let _ = self.cmds.lock().unwrap().send(TermCmd::Close);
    }
}

fn feed_terminal(pid: i32, pty_id: i32, cmds: std::sync::mpsc::Receiver<TermCmd>) {
    while let Ok(cmd) = cmds.recv() {
        match cmd {
            TermCmd::Input(bytes) => {
                let mut rest = &bytes[..];
                while !rest.is_empty() {
                    match ffi::pty_input(pty_id, rest) {
                        Ok(Some(n)) => rest = &rest[n.min(rest.len())..],
                        // Full until the program reads: wait, never drop.
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                        Err(()) => return,
                    }
                }
            }
            TermCmd::Resize(rows, cols) => ffi::pty_resize(pty_id, rows, cols),
            TermCmd::Close => {
                ffi::kill_group(pid);
                return;
            }
        }
    }
}

/// Which terminal's output goes where. The kernel reports output by
/// terminal id on its own threads, so this must answer without waiting.
#[derive(Default)]
struct PtyRoutes {
    routes: Mutex<HashMap<i32, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    /// The terminal being opened, whose id is not known yet.
    opening: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    /// One terminal is opened at a time, so `opening` is unambiguous.
    open_lock: Mutex<()>,
}

impl PtyRoutes {
    fn opening(&self, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) -> OpeningGuard<'_> {
        let guard = self.open_lock.lock().unwrap();
        *self.opening.lock().unwrap() = Some(tx);
        OpeningGuard { routes: self, _guard: guard }
    }

    fn bind(&self, id: i32, tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) {
        self.routes.lock().unwrap().entry(id).or_insert(tx);
    }

    fn unbind(&self, id: i32) {
        self.routes.lock().unwrap().remove(&id);
    }

    #[cfg_attr(ish_stub, allow(dead_code))]
    fn deliver(&self, id: i32, bytes: &[u8]) {
        let mut routes = self.routes.lock().unwrap();
        if !routes.contains_key(&id) {
            match self.opening.lock().unwrap().clone() {
                Some(tx) => {
                    routes.insert(id, tx);
                }
                None => return,
            }
        }
        if let Some(tx) = routes.get(&id) {
            let _ = tx.send(bytes.to_vec());
        }
    }
}

struct OpeningGuard<'a> {
    routes: &'a PtyRoutes,
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl Drop for OpeningGuard<'_> {
    fn drop(&mut self) {
        *self.routes.opening.lock().unwrap() = None;
    }
}

/// What the model needs to know about this guest. Every line was checked in
/// it (simulator, 2026-09-25); re-check a line before changing it, and
/// after changing the rootfs.
const NOTES: &str = "\
- The shell is busybox ash, not bash: no arrays, `[[ ]]`, `{1..3}` or `<<<`. A syntax error anywhere stops the whole script before any of it runs.
- Present: wget (HTTPS works), tar, unzip, awk, sed, find, od, xxd. Missing until installed: bash, curl, python3, git, jq, file, openssl. `apk add <name>` takes from seconds to about a minute (python3 measured at 10 s and 48 s); check with `command -v` first. For Python libraries prefer Alpine's packages (`apk add py3-pillow`) over pip.
- Commands get no input and no terminal: `read` sees end of input, and interactive programs (vi, top, ssh password prompts) cannot be used.
- A command started with `&` keeps the call open until it exits, unless its output is redirected: `cmd >/dev/null 2>&1 &` returns at once and keeps running.
- This Linux is emulated and runs tens of times slower than the device itself. Keep heavy computation small, and put several steps in one script rather than one call each.";

/// Said only when the command is installed.
const SOLOS_NOTE: &str = "\
- A script can call your tools too, with the same fields: `solos call <tool> --field value`, or `solos device <capability> <action> --field value` for the device (`solos device calendar list --from today`). The result is JSON on stdout. Use it when a script needs many calls or should act on its own results; `solos --help` has the details.";

/// The device's current UTC offset as a POSIX TZ string (`<+09>-9`). The
/// rootfs has no zone database, and this form needs none; it is computed per
/// command, so a change of zone or daylight saving is picked up.
fn posix_tz() -> String {
    #[repr(C)]
    struct Tm {
        sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32, wday: i32, yday: i32, isdst: i32,
        gmtoff: std::os::raw::c_long,
        zone: *const std::os::raw::c_char,
    }
    extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    let now = unsafe { time(std::ptr::null_mut()) };
    if unsafe { localtime_r(&now, &mut tm) }.is_null() {
        return "UTC0".into();
    }
    posix_tz_for_offset(tm.gmtoff as i64)
}

fn posix_tz_for_offset(seconds_east: i64) -> String {
    let sign = if seconds_east >= 0 { '+' } else { '-' };
    let abs = seconds_east.abs();
    let (h, m) = (abs / 3600, (abs % 3600) / 60);
    // POSIX counts west as positive, so the sign of the offset flips.
    let posix_sign = if seconds_east >= 0 { "-" } else { "" };
    if m == 0 {
        format!("<{sign}{h:02}>{posix_sign}{h}")
    } else {
        format!("<{sign}{h:02}{m:02}>{posix_sign}{h}:{m:02}")
    }
}

/// Unpack the bundled rootfs the first time, or when the bundle changed.
/// Unpacks the bundled system unless this one is already there; says
/// whether it did.
fn unpack_once(zip_path: &Path, root: &Path) -> Result<bool, String> {
    let stamp_file = root.join(".rootfs-stamp");
    let meta = std::fs::metadata(zip_path).map_err(|e| format!("{}: {e}", zip_path.display()))?;
    let stamp = format!("{}", meta.len());
    if root.join("alpine-rootfs/meta.db").exists() && std::fs::read_to_string(&stamp_file).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(false);
    }
    let _ = std::fs::remove_dir_all(root.join("alpine-rootfs"));
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    archive.extract(root).map_err(|e| e.to_string())?;
    std::fs::write(&stamp_file, stamp).map_err(|e| e.to_string())?;
    Ok(true)
}

/// A wait status as a shell would report it: the exit code, or 128 plus the
/// signal that killed the process.
fn wait_status_to_code(status: i32) -> i32 {
    if status & 0x7f == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    }
}

/// Guest pids waiting to hear that they exited. The kernel reports exits on
/// its own threads, sometimes before the spawn call has even returned.
#[derive(Default)]
pub(crate) struct ExitRegistry {
    waiting: Mutex<HashMap<i32, oneshot::Sender<i32>>>,
    early: Mutex<HashMap<i32, i32>>,
}

impl ExitRegistry {
    fn register(&self, pid: i32) -> oneshot::Receiver<i32> {
        let (tx, rx) = oneshot::channel();
        if let Some(code) = self.early.lock().unwrap().remove(&pid) {
            let _ = tx.send(code);
        } else {
            self.waiting.lock().unwrap().insert(pid, tx);
        }
        rx
    }

    #[cfg_attr(ish_stub, allow(dead_code))]
    pub(crate) fn notify(&self, pid: i32, status: i32) {
        if let Some(tx) = self.waiting.lock().unwrap().remove(&pid) {
            let _ = tx.send(status);
        } else {
            self.early.lock().unwrap().insert(pid, status);
        }
    }

    fn forget(&self, pid: i32) {
        self.waiting.lock().unwrap().remove(&pid);
        self.early.lock().unwrap().remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fetch_polyfill_answers_cookies_as_fetch_does() {
        let (path, content) = NODE_POLYFILLS[1];
        let patched = node_polyfill(path, content);
        assert!(patched.contains("getSetCookie: () =>"), "the polyfill changed upstream; patch it again");
        assert!(patched.contains("v.join(\", \")"));
        assert_eq!(node_polyfill(NODE_POLYFILLS[0].0, NODE_POLYFILLS[0].1), NODE_POLYFILLS[0].1);
    }

    #[test]
    fn wait_statuses_read_like_a_shell_reports_them() {
        assert_eq!(wait_status_to_code(0), 0);
        assert_eq!(wait_status_to_code(3 << 8), 3);
        assert_eq!(wait_status_to_code(9), 137);
    }

    #[test]
    fn utc_offsets_become_posix_zones() {
        assert_eq!(posix_tz_for_offset(9 * 3600), "<+09>-9");
        assert_eq!(posix_tz_for_offset(-5 * 3600), "<-05>5");
        assert_eq!(posix_tz_for_offset(5 * 3600 + 1800), "<+0530>-5:30");
        assert_eq!(posix_tz_for_offset(0), "<+00>-0");
    }

    #[test]
    fn output_before_the_terminal_id_is_known_reaches_that_terminal() {
        let routes = PtyRoutes::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        {
            let _opening = routes.opening(tx.clone());
            routes.deliver(3, b"prompt$ ");
            routes.bind(3, tx);
        }
        routes.deliver(3, b"ls");
        routes.deliver(4, b"nobody's");
        assert_eq!(rx.try_recv().unwrap(), b"prompt$ ");
        assert_eq!(rx.try_recv().unwrap(), b"ls");
        assert!(rx.try_recv().is_err(), "output for an unknown terminal goes nowhere");
        routes.unbind(3);
        routes.deliver(3, b"late");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn an_exit_reported_before_anyone_waits_is_not_lost() {
        let r = ExitRegistry::default();
        r.notify(7, 0);
        let mut rx = r.register(7);
        assert_eq!(rx.try_recv().unwrap(), 0);
    }
}

/// `v3.21` from the guest's `/etc/alpine-release` (`3.21.3`), read on the
/// host side: reading the guest's files from here is safe, writing is not.
fn alpine_branch(rootfs: &Path) -> Option<String> {
    let release = std::fs::read_to_string(rootfs.join("data/etc/alpine-release")).ok()?;
    let mut parts = release.trim().split('.');
    Some(format!("v{}.{}", parts.next()?, parts.next()?))
}


#[cfg(test)]
mod package_tests {
    use super::*;

    #[test]
    fn the_branch_comes_from_the_release_file() {
        let dir = std::env::temp_dir().join(format!("solos-branch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("data/etc")).unwrap();
        assert_eq!(alpine_branch(&dir), None);
        std::fs::write(dir.join("data/etc/alpine-release"), "3.21.3\n").unwrap();
        assert_eq!(alpine_branch(&dir).as_deref(), Some("v3.21"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
