//! The Solos core for apps, through UniFFI.
//!
//! Every call is `async` and runs on the core's own runtime; a caller only
//! ever awaits, it never blocks. Events arrive through one [`EventSink`]. If
//! the sink falls behind and events are dropped, it is told to resynchronise
//! (fetch snapshots) rather than left with a silent gap.

use solos_api::*;
use solos_core::sandbox::host::HostSandbox;
use solos_core::sandbox::Sandbox;
use solos_core::tools::Registry;
use solos_core::{Engine, EngineConfig};
use solos_sandbox_ish::IshSandbox;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast::error::RecvError;

uniffi::setup_scaffolding!();

/// Receives events on a core thread. Implementations hand them to their UI
/// thread and return at once.
#[uniffi::export(with_foreign)]
pub trait EventSink: Send + Sync {
    fn on_event(&self, event: Event);
    /// Events were dropped: every open session should fetch a snapshot.
    fn on_resync(&self);
}

/// Receives one terminal's output, on a core thread. Hand it to the UI
/// thread and return at once.
#[uniffi::export(with_foreign)]
pub trait TerminalSink: Send + Sync {
    fn output(&self, data: Vec<u8>);
    /// The shell ended; nothing more will arrive.
    fn exited(&self, code: Option<i32>);
}

/// The platform's web view, for the `browser` tool. One entry point, JSON in
/// and JSON out (see `solos_core::tools::browser::BrowserBridge` for the
/// operations). Called on a core thread that may block; the implementation
/// hops to its main thread itself.
#[uniffi::export(with_foreign)]
pub trait BrowserHost: Send + Sync {
    fn call(&self, op: String, input_json: String) -> String;
}

/// The platform's device capabilities (calendar, photos, …). Only those
/// `capabilities` names become tools. Called on a core thread that may block.
#[uniffi::export(with_foreign)]
pub trait DeviceHost: Send + Sync {
    fn capabilities(&self) -> Vec<String>;
    fn call(&self, capability: String, input_json: String) -> String;
    /// Ask the person whether the assistant may read this kind of data;
    /// `capability` names the tool that wants to (`device_calendar`). Called on
    /// a core thread that may block while the question is on screen. `None`:
    /// the person cannot be asked just now (the app is not on screen).
    fn consent(&self, kind: ConsentKind, capability: String) -> Option<bool>;
}

struct Device(Arc<dyn DeviceHost>);
impl solos_core::tools::device::DeviceBridge for Device {
    fn capabilities(&self) -> Vec<String> {
        self.0.capabilities()
    }
    fn call(&self, capability: &str, input: serde_json::Value) -> serde_json::Value {
        let reply = self.0.call(capability.to_string(), input.to_string());
        serde_json::from_str(&reply).unwrap_or_else(|e| serde_json::json!({"error": format!("the device answered malformed JSON: {e}")}))
    }
    fn consent(&self, kind: ConsentKind, capability: &str) -> Option<bool> {
        self.0.consent(kind, capability.to_string())
    }
}

struct Browser(Arc<dyn BrowserHost>);
impl solos_core::tools::browser::BrowserBridge for Browser {
    fn call(&self, op: &str, input: serde_json::Value) -> serde_json::Value {
        let reply = self.0.call(op.to_string(), input.to_string());
        serde_json::from_str(&reply).unwrap_or_else(|e| serde_json::json!({"error": format!("the browser answered malformed JSON: {e}")}))
    }
}

/// Where API keys live on this platform (the Keychain on iOS).
#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    fn secret(&self, reference: String) -> Option<String>;
    /// Keep `value` under `reference`; an empty value deletes it. Returns
    /// whether it was kept.
    fn store(&self, reference: String, value: String) -> bool;
}

#[derive(uniffi::Record)]
pub struct CoreConfig {
    /// The app's data directory: database, workspace, unpacked rootfs.
    pub data_dir: String,
    /// The rootfs zip in the app bundle. `None` runs commands on the host's
    /// shell instead (macOS, tests).
    pub bundled_rootfs: Option<String>,
    /// Write request bodies and raw response streams here (diagnosis only).
    pub capture_dir: Option<String>,
    /// The folder `/solos/ws` shows; `None` keeps it inside `data_dir`. What
    /// an earlier version kept in `data_dir/workspace` is moved here once.
    #[uniffi(default = None)]
    pub workspace_dir: Option<String>,
}

struct Secrets(Arc<dyn SecretStore>);
impl solos_core::SecretResolver for Secrets {
    fn secret(&self, reference: &str) -> Option<String> {
        self.0.secret(reference.to_string())
    }
    fn store(&self, reference: &str, value: &str) -> bool {
        self.0.store(reference.to_string(), value.to_string())
    }
}

#[derive(uniffi::Object)]
pub struct SolosCore {
    rt: tokio::runtime::Runtime,
    engine: Engine,
    pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn internal(e: impl std::fmt::Display) -> CoreError {
    CoreError::Internal { detail: e.to_string() }
}

/// Open the core. Safe to call from any thread; it does its work on the
/// core's runtime.
#[uniffi::export]
pub async fn open_core(
    config: CoreConfig,
    secrets: Arc<dyn SecretStore>,
    browser: Option<Arc<dyn BrowserHost>>,
    device: Option<Arc<dyn DeviceHost>>,
) -> Result<Arc<SolosCore>, CoreError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .thread_name("solos-core")
        .build()
        .map_err(internal)?;
    let data_dir = PathBuf::from(&config.data_dir);
    let workspace = config.workspace_dir.as_ref().map(PathBuf::from).unwrap_or_else(|| data_dir.join("workspace"));
    if let Err(e) = solos_core::workspace::migrate(&data_dir.join("workspace"), &workspace) {
        return Err(CoreError::Storage { detail: format!("moving the workspace to {}: {e}", workspace.display()) });
    }
    let sandbox: Arc<dyn Sandbox> = match &config.bundled_rootfs {
        Some(zip) => Arc::new(IshSandbox::new(PathBuf::from(zip), data_dir.join("rootfs"), workspace)),
        None => Arc::new(HostSandbox::new(data_dir.join("guest"))),
    };
    let mut tools = Registry::builtin();
    if let Some(host) = browser {
        tools.add(Arc::new(solos_core::tools::browser::BrowserTool::new(Arc::new(Browser(host)))));
    }
    if let Some(host) = device {
        solos_core::tools::device::register(&mut tools, Arc::new(Device(host)));
    }
    let cfg = EngineConfig {
        data_dir,
        sandbox,
        tools,
        secrets: Arc::new(Secrets(secrets)),
        capture_dir: config.capture_dir.map(PathBuf::from),
        provider_factory: None,
    };
    let opened = rt.spawn(Engine::open(cfg)).await.map_err(internal).and_then(|r| r);
    match opened {
        Ok(engine) => Ok(Arc::new(SolosCore { rt, engine, pump: Mutex::new(None) })),
        Err(e) => {
            // A runtime may not be dropped from inside an async context.
            std::thread::spawn(move || drop(rt));
            Err(e)
        }
    }
}

impl SolosCore {
    /// Run `f` on the core's runtime and await it from wherever the caller is.
    async fn run<T, F, Fut>(&self, f: F) -> Result<T, CoreError>
    where
        T: Send + 'static,
        F: FnOnce(Engine) -> Fut,
        Fut: std::future::Future<Output = Result<T, CoreError>> + Send + 'static,
    {
        self.rt.spawn(f(self.engine.clone())).await.map_err(internal)?
    }
}

#[uniffi::export]
impl SolosCore {
    /// Deliver events to `sink` from now on, replacing any previous sink.
    pub fn subscribe(&self, sink: Arc<dyn EventSink>) {
        let mut rx = self.engine.subscribe();
        let task = self.rt.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => sink.on_event(ev),
                    Err(RecvError::Lagged(_)) => sink.on_resync(),
                    Err(RecvError::Closed) => break,
                }
            }
        });
        if let Some(old) = self.pump.lock().unwrap().replace(task) {
            old.abort();
        }
    }

    pub async fn boot_sandbox(&self) -> Result<(), CoreError> {
        self.run(|e| async move { e.sandbox().boot().await.map_err(|err| CoreError::Sandbox { detail: err.to_string() }) })
            .await
    }

    /// Where a workspace or shared-folder file the model named
    /// (`solos://ws/…`, `/solos/ws/…`, `solos://mnt/…`, `/solos/mnt/…`) is on
    /// this device. Path arithmetic; a link inside a shared folder is looked at.
    pub fn resolve_file(&self, reference: String) -> Option<String> {
        self.engine.resolve_file(&reference).map(|p| p.to_string_lossy().into_owned())
    }

    /// What the person agreed to let the assistant read from the device.
    pub fn consents(&self) -> Consents {
        self.engine.consents()
    }

    /// Change an answer (Settings): `Some(true)` allows, `Some(false)`
    /// refuses, `None` asks again at the next use.
    pub fn set_consent(&self, kind: ConsentKind, answer: Option<bool>) {
        self.engine.set_consent(kind, answer)
    }

    /// The folders the user shares with the model.
    pub fn mounts(&self) -> Vec<Mount> {
        self.engine.mounts()
    }

    /// Replace the shared folders: the app calls it at start-up with the
    /// folders it could open again, and whenever the user changes them.
    pub fn set_mounts(&self, mounts: Vec<Mount>) -> Result<(), CoreError> {
        self.engine.set_mounts(mounts)
    }

    pub fn settings(&self) -> Settings {
        self.engine.settings()
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<(), CoreError> {
        self.run(|e| async move { e.set_settings(settings).await }).await
    }

    pub async fn list_models(&self, endpoint: Endpoint) -> Result<Vec<ModelInfo>, CoreError> {
        self.run(|e| async move { e.list_models(endpoint).await }).await
    }

    /// The models an endpoint listed the last time it was asked; `None`
    /// before that. No request is made.
    pub fn cached_models(&self, endpoint_id: String) -> Option<ModelList> {
        self.engine.cached_models(&endpoint_id)
    }

    pub async fn create_session(&self, model: Option<ModelChoice>) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.create_session(model).await }).await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, CoreError> {
        self.run(|e| async move { e.list_sessions().await }).await
    }

    pub async fn delete_session(&self, session_id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.delete_session(session_id).await }).await
    }

    pub async fn snapshot(&self, session_id: String) -> Result<Snapshot, CoreError> {
        self.run(|e| async move { e.snapshot(session_id).await }).await
    }

    pub async fn export_session(&self, session_id: String) -> Result<String, CoreError> {
        self.run(|e| async move { e.export_session(session_id).await }).await
    }

    pub async fn set_session_model(&self, session_id: String, model: ModelChoice) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.set_session_model(session_id, model).await }).await
    }

    /// Thinking for one session; `None` follows the settings.
    pub async fn set_session_thinking(&self, session_id: String, thinking: Option<bool>) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.set_session_thinking(session_id, thinking).await }).await
    }

    /// Agent mode for one session; `None` follows the settings.
    pub async fn set_session_agent_mode(&self, session_id: String, agent_mode: Option<bool>) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.set_session_agent_mode(session_id, agent_mode).await }).await
    }

    /// Say something, with files attached; the core copies them into the
    /// workspace before this returns.
    pub async fn send(&self, session_id: String, text: String, attachments: Vec<AttachmentSource>) -> Result<(), CoreError> {
        self.run(|e| async move { e.send_with(session_id, text, attachments).await }).await
    }

    /// Answer again from the last user message, or from `message_id`,
    /// optionally with its text replaced; later messages are deleted.
    pub async fn retry(&self, session_id: String, message_id: Option<String>, text: Option<String>) -> Result<(), CoreError> {
        self.run(|e| async move { e.retry(session_id, message_id, text).await }).await
    }

    /// Carry on a turn that was cut off.
    pub async fn resume(&self, session_id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.resume(session_id).await }).await
    }

    pub async fn rename_session(&self, session_id: String, title: String) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.rename_session(session_id, title).await }).await
    }

    pub async fn clear_session(&self, session_id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.clear_session(session_id).await }).await
    }

    pub async fn search_sessions(&self, query: String) -> Result<Vec<SessionInfo>, CoreError> {
        self.run(|e| async move { e.search_sessions(query).await }).await
    }

    /// Summarise the start of the conversation (the user agreed to it).
    pub async fn compact(&self, session_id: String) -> Result<Compaction, CoreError> {
        self.run(|e| async move { e.compact(session_id).await }).await
    }

    pub async fn undo_compaction(&self, session_id: String, compaction_id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.undo_compaction(session_id, compaction_id).await }).await
    }

    /// A model's context window set by hand; `None` forgets it.
    /// A login shell on a terminal of `rows` × `cols`.
    pub async fn open_terminal(&self, rows: u16, cols: u16, sink: Arc<dyn TerminalSink>) -> Result<String, CoreError> {
        let out = sink.clone();
        self.run(move |e| async move {
            e.open_terminal(rows, cols, Arc::new(move |bytes| out.output(bytes)), Box::new(move |code| sink.exited(code))).await
        })
        .await
    }

    /// Keystrokes or a paste, in order.
    pub fn terminal_input(&self, terminal_id: String, data: Vec<u8>) -> Result<(), CoreError> {
        self.engine.terminal_input(&terminal_id, data)
    }

    pub fn terminal_resize(&self, terminal_id: String, rows: u16, cols: u16) -> Result<(), CoreError> {
        self.engine.terminal_resize(&terminal_id, rows, cols)
    }

    pub fn terminal_close(&self, terminal_id: String) -> Result<(), CoreError> {
        self.engine.terminal_close(&terminal_id)
    }

    /// Where the guest's `/` is on the device, for the file browser. Only
    /// the workspace inside it may be changed from the app.
    pub fn guest_root_dir(&self) -> Option<String> {
        self.engine.guest_root_dir().map(|p| p.to_string_lossy().into_owned())
    }

    pub async fn set_pinned(&self, session_id: String, pinned: bool) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.set_pinned(session_id, pinned).await }).await
    }

    pub async fn duplicate_session(&self, session_id: String, title: Option<String>) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.duplicate_session(session_id, title).await }).await
    }

    pub async fn regenerate_title(&self, session_id: String) -> Result<SessionInfo, CoreError> {
        self.run(|e| async move { e.regenerate_title(session_id).await }).await
    }

    pub async fn export_session_text(&self, session_id: String) -> Result<String, CoreError> {
        self.run(|e| async move { e.export_session_text(session_id).await }).await
    }

    /// Whether the sandbox runs, and how much room things take.
    pub async fn sandbox_status(&self) -> Result<SandboxStatus, CoreError> {
        self.run(|e| async move { Ok(e.sandbox_status().await) }).await
    }

    /// Every source for every package manager, official first in each.
    pub fn package_mirrors(&self) -> Vec<PackageMirror> {
        self.engine.package_mirrors()
    }

    /// The source in use for each package manager.
    pub fn chosen_package_mirrors(&self) -> Vec<PackageMirror> {
        self.engine.chosen_package_mirrors()
    }

    pub async fn set_package_mirror(&self, kind: MirrorKind, id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.set_package_mirror(kind, id).await }).await
    }

    /// Times one package manager's sources, at once.
    pub async fn test_package_mirrors(&self, kind: MirrorKind) -> Result<Vec<MirrorSpeed>, CoreError> {
        self.run(|e| async move { Ok(e.test_package_mirrors(Some(kind)).await) }).await
    }

    /// After a fresh Linux system is unpacked: switches each package manager
    /// to its fastest mirror, if one beat the official source.
    pub async fn choose_package_mirrors_if_fresh(&self) -> Result<Vec<PackageMirror>, CoreError> {
        self.run(|e| async move { e.choose_package_mirrors_if_fresh().await }).await
    }

    /// The installed skills, by folder.
    pub fn skills(&self) -> Vec<Skill> {
        self.engine.skills()
    }

    /// Installs (or updates) a skill from a GitHub link and turns it on.
    pub async fn install_skill(&self, source: String) -> Result<Skill, CoreError> {
        self.run(|e| async move { e.install_skill(source).await }).await
    }

    /// Installs a skill from its `SKILL.md`, pasted.
    pub async fn install_skill_text(&self, text: String) -> Result<Skill, CoreError> {
        self.run(|e| async move { e.install_skill_text(text).await }).await
    }

    /// Installs a skill from a file on the device (`.zip`, `.skill`,
    /// `SKILL.md`, or a folder with one).
    pub async fn install_skill_file(&self, path: String) -> Result<Skill, CoreError> {
        self.run(|e| async move { e.install_skill_file(path).await }).await
    }

    /// Installs a skill again from the GitHub link it came from.
    pub async fn update_skill(&self, folder: String) -> Result<Skill, CoreError> {
        self.run(|e| async move { e.update_skill(folder).await }).await
    }

    /// A skill's files, relative to its folder.
    pub fn skill_files(&self, folder: String) -> Result<Vec<String>, CoreError> {
        self.engine.skill_files(folder)
    }

    pub async fn remove_skill(&self, folder: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.remove_skill(folder).await }).await
    }

    pub async fn set_skill_enabled(&self, folder: String, enabled: bool) -> Result<(), CoreError> {
        self.run(|e| async move { e.set_skill_enabled(folder, enabled).await }).await
    }

    /// A skill's `SKILL.md`.
    pub fn skill_instructions(&self, folder: String) -> Result<String, CoreError> {
        self.engine.skill_instructions(folder)
    }

    pub fn mcp_servers(&self) -> Vec<McpServer> {
        self.engine.mcp_servers()
    }

    /// Adds servers from pasted `mcpServers` JSON, connecting to each; one
    /// that cannot be reached is kept with its error.
    pub async fn add_mcp_servers(&self, json: String) -> Result<Vec<McpServer>, CoreError> {
        self.run(|e| async move { e.add_mcp_servers(json).await }).await
    }

    /// Adds one server as a form gives it (or replaces the one of its name).
    pub async fn add_mcp_server(&self, server: McpServer) -> Result<McpServer, CoreError> {
        self.run(|e| async move { e.add_mcp_server(server).await }).await
    }

    /// Replaces the server `name` with `server`, which may be renamed.
    pub async fn update_mcp_server(&self, name: String, server: McpServer) -> Result<McpServer, CoreError> {
        self.run(|e| async move { e.update_mcp_server(name, server).await }).await
    }

    pub async fn remove_mcp_server(&self, name: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.remove_mcp_server(name).await }).await
    }

    pub async fn set_mcp_server_enabled(&self, name: String, enabled: bool) -> Result<(), CoreError> {
        self.run(|e| async move { e.set_mcp_server_enabled(name, enabled).await }).await
    }

    /// Connects again and lists the server's tools afresh.
    pub async fn refresh_mcp_server(&self, name: String) -> Result<McpServer, CoreError> {
        self.run(|e| async move { e.refresh_mcp_server(name).await }).await
    }

    /// Deletes the browser tool's saved pages and screenshots; returns the
    /// bytes freed.
    pub async fn clear_temporary_files(&self) -> Result<u64, CoreError> {
        self.run(|e| async move { e.clear_temporary_files().await }).await
    }

    /// The device folder behind `/solos/ws`.
    pub fn workspace_dir(&self) -> String {
        self.engine.workspace_dir().to_string_lossy().into_owned()
    }

    pub fn model_window(&self, model: ModelChoice) -> ModelWindow {
        self.engine.model_window(model)
    }

    pub async fn set_model_window(&self, model: ModelChoice, window: Option<u64>) -> Result<(), CoreError> {
        self.run(|e| async move { e.set_model_window(model, window).await }).await
    }

    pub async fn cancel(&self, session_id: String) -> Result<(), CoreError> {
        self.run(|e| async move { e.cancel(session_id).await }).await
    }
}
