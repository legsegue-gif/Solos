//! Sessions, turns, and the event stream.
//!
//! Each session has a small piece of live state (the running turn, the
//! message being streamed) guarded together with its event counter. Every
//! event is applied to that state and numbered under the same lock, and a
//! snapshot is taken under it too, so a snapshot always equals "all events up
//! to `seq`" — the property clients rely on to resynchronise.

use crate::agent::{self, TurnConfig, TurnEnd, TurnHost};
use crate::providers::{anthropic::Anthropic, gemini::Gemini, openai::OpenAi, Capture, Provider};
use crate::sandbox::Sandbox;
use crate::store::{now_millis, Store, StoreError};
use crate::tools::Registry;
use async_trait::async_trait;
use solos_api::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

/// Looks up API keys where the client keeps them.
pub trait SecretResolver: Send + Sync {
    fn secret(&self, reference: &str) -> Option<String>;
}

/// Builds a provider for an endpoint. Replaceable so tests can script one.
pub type ProviderFactory =
    Arc<dyn Fn(&Endpoint, String, &Capture) -> Result<Arc<dyn Provider>, CoreError> + Send + Sync>;

pub struct EngineConfig {
    pub data_dir: PathBuf,
    pub sandbox: Arc<dyn Sandbox>,
    pub tools: Registry,
    pub secrets: Arc<dyn SecretResolver>,
    /// Request / stream capture for diagnosis; off when `None`.
    pub capture_dir: Option<PathBuf>,
    pub provider_factory: Option<ProviderFactory>,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    data_dir: PathBuf,
    store: Store,
    sandbox: Arc<dyn Sandbox>,
    tools: Registry,
    secrets: Arc<dyn SecretResolver>,
    capture: Capture,
    providers: ProviderFactory,
    settings: RwLock<Settings>,
    /// Context windows endpoints reported, by `endpoint_id/model`; kept in
    /// the store so they are known before the model list is fetched again.
    windows: RwLock<HashMap<String, u64>>,
    /// Windows the user set, by the same key; never touched by a fetch.
    custom_windows: RwLock<HashMap<String, u64>>,
    /// The chosen source per package manager, by `mirrors::key`; a kind
    /// not in it uses its official source. Stored on its own, like the
    /// windows, so a settings record from before it still reads.
    package_mirrors: RwLock<std::collections::BTreeMap<String, String>>,
    /// Skills the user turned off, by folder. The skills themselves are the
    /// folders on disk (`skills`).
    disabled_skills: RwLock<std::collections::BTreeSet<String>>,
    sessions: Mutex<HashMap<String, Arc<Slot>>>,
    /// Open terminals by id. A terminal belongs to no session.
    terminals: std::sync::Mutex<HashMap<String, Arc<dyn crate::sandbox::Terminal>>>,
    events: broadcast::Sender<Event>,
    /// Stops the tool bridge when the engine goes.
    bridge_stop: CancellationToken,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.bridge_stop.cancel();
    }
}

/// One session's live state and its event counter.
struct Slot {
    live: Mutex<Live>,
}

struct Live {
    info: SessionInfo,
    /// Not yet written to the store: nothing has been said in it.
    pending: bool,
    seq: u64,
    turn: Option<TurnLive>,
    /// A title is being written for this session.
    titling: bool,
}

struct TurnLive {
    id: String,
    cancel: CancellationToken,
    streaming: Option<Message>,
    running: Vec<String>,
}

fn storage(e: StoreError) -> CoreError {
    CoreError::Storage { detail: e.to_string() }
}

fn default_factory() -> ProviderFactory {
    Arc::new(|ep: &Endpoint, key: String, capture: &Capture| -> Result<Arc<dyn Provider>, CoreError> {
        match ep.protocol {
            Protocol::OpenAi => Ok(Arc::new(OpenAi::new(&ep.base_url, key, capture.clone()))),
            Protocol::Anthropic => Ok(Arc::new(Anthropic::new(&ep.base_url, key, capture.clone()))),
            Protocol::Gemini => Ok(Arc::new(Gemini::new(&ep.base_url, key, capture.clone()))),
        }
    })
}

impl Engine {
    pub async fn open(cfg: EngineConfig) -> Result<Self, CoreError> {
        std::fs::create_dir_all(&cfg.data_dir).map_err(|e| CoreError::Storage { detail: e.to_string() })?;
        let store = Store::open(&cfg.data_dir.join("solos.db")).map_err(storage)?;
        let settings: Settings = store
            .setting("settings")
            .await
            .map_err(storage)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let windows: HashMap<String, u64> = store
            .setting("model_windows")
            .await
            .map_err(storage)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let custom_windows: HashMap<String, u64> = store
            .setting("model_window_overrides")
            .await
            .map_err(storage)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let package_mirrors: std::collections::BTreeMap<String, String> = store
            .setting("package_mirrors")
            .await
            .map_err(storage)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let disabled_skills: std::collections::BTreeSet<String> = store
            .setting("skills_disabled")
            .await
            .map_err(storage)?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        // Before the first boot this only records them; boot writes them.
        for (kind, id) in crate::mirrors::kinds().into_iter().filter_map(|k| package_mirrors.get(crate::mirrors::key(k)).map(|id| (k, id))) {
            if let Err(e) = apply_mirror(cfg.sandbox.as_ref(), kind, id).await {
                tracing::warn!("the {} mirror could not be applied: {e}", crate::mirrors::key(kind));
            }
        }
        // The tools, for scripts in the sandbox (`solos`). Without it the
        // model still has every tool, so a failure is logged, not fatal.
        let bridge_stop = CancellationToken::new();
        match crate::bridge::serve(cfg.tools.clone(), cfg.sandbox.clone(), bridge_stop.clone()).await {
            Ok(env) => cfg.sandbox.set_guest_env(env),
            Err(e) => tracing::warn!("the tool bridge did not start; `solos` will not work in the sandbox: {e}"),
        }
        let (events, _) = broadcast::channel(4096);
        let engine = Self {
            inner: Arc::new(Inner {
                data_dir: cfg.data_dir.clone(),
                store,
                sandbox: cfg.sandbox,
                tools: cfg.tools,
                secrets: cfg.secrets,
                capture: Capture { dir: cfg.capture_dir },
                providers: cfg.provider_factory.unwrap_or_else(default_factory),
                settings: RwLock::new(settings),
                windows: RwLock::new(windows),
                custom_windows: RwLock::new(custom_windows),
                package_mirrors: RwLock::new(package_mirrors),
                disabled_skills: RwLock::new(disabled_skills),
                sessions: Mutex::new(HashMap::new()),
                terminals: std::sync::Mutex::new(HashMap::new()),
                events,
                bridge_stop,
            }),
        };
        engine.recover().await?;
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn sandbox(&self) -> Arc<dyn Sandbox> {
        self.inner.sandbox.clone()
    }

    /// Where a workspace file the model named (`solos://ws/…` or
    /// `/solos/ws/…`) is on this device; `None` for anything else.
    /// Where the guest's `/` is on the device, when the sandbox has one.
    pub fn guest_root_dir(&self) -> Option<std::path::PathBuf> {
        self.inner.sandbox.guest_root_dir()
    }

    /// For the settings screen: whether the sandbox runs (starting it if it
    /// has not been), and how much room it and the conversations take.
    pub async fn sandbox_status(&self) -> SandboxStatus {
        let sandbox = self.inner.sandbox.clone();
        let error = sandbox.boot().await.err().map(|e| e.to_string());
        let data_dir = self.inner.data_dir.clone();
        let (system, workspace) = (sandbox.system_dir(), sandbox.workspace_dir());
        let (system_bytes, workspace_bytes, database_bytes, temporary_bytes) = tokio::task::spawn_blocking(move || {
            let database = std::fs::read_dir(&data_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("solos.db"))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum();
            let temporary = crate::tools::browser::temporary_dirs(&workspace).iter().map(|d| folder_bytes(d)).sum();
            (system.as_deref().map(folder_bytes), folder_bytes(&workspace), database, temporary)
        })
        .await
        .unwrap_or((None, 0, 0, 0));
        SandboxStatus { error, system_bytes, workspace_bytes, database_bytes, temporary_bytes }
    }

    pub fn package_mirrors(&self) -> Vec<PackageMirror> {
        crate::mirrors::all()
    }

    /// The source in use for each package manager.
    pub fn chosen_package_mirrors(&self) -> Vec<PackageMirror> {
        let chosen = self.inner.package_mirrors.read().unwrap().clone();
        crate::mirrors::kinds()
            .into_iter()
            .filter_map(|k| crate::mirrors::find(k, chosen.get(crate::mirrors::key(k)).map_or(crate::mirrors::OFFICIAL, |s| s.as_str())))
            .collect()
    }

    /// Switches where one package manager gets packages from: its config in
    /// the guest now (starting the sandbox if needed), and at every boot.
    pub async fn set_package_mirror(&self, kind: MirrorKind, id: String) -> Result<(), CoreError> {
        self.inner.sandbox.boot().await.map_err(|e| CoreError::Sandbox { detail: e.to_string() })?;
        apply_mirror(self.inner.sandbox.as_ref(), kind, &id).await?;
        let snapshot = {
            let mut chosen = self.inner.package_mirrors.write().unwrap();
            chosen.insert(crate::mirrors::key(kind).to_string(), id);
            chosen.clone()
        };
        let value = serde_json::to_value(snapshot).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
        self.inner.store.put_setting("package_mirrors", value).await.map_err(storage)
    }

    /// How fast the sources answer, from this device: one kind's, or all.
    pub async fn test_package_mirrors(&self, kind: Option<MirrorKind>) -> Vec<MirrorSpeed> {
        let branch = self.inner.sandbox.package_branch().unwrap_or_else(|| "latest-stable".into());
        let mirrors = crate::mirrors::all().into_iter().filter(|m| kind.is_none_or(|k| m.kind == k)).collect();
        crate::mirrors::speed_test(mirrors, &branch, std::time::Duration::from_secs(8)).await
    }

    /// On a freshly unpacked Linux system, as the reference app does: times
    /// every source and switches each package manager whose fastest answer
    /// came from a mirror. Nothing on an existing system. Returns what it
    /// switched.
    pub async fn choose_package_mirrors_if_fresh(&self) -> Result<Vec<PackageMirror>, CoreError> {
        self.inner.sandbox.boot().await.map_err(|e| CoreError::Sandbox { detail: e.to_string() })?;
        if !self.inner.sandbox.freshly_installed() {
            return Ok(vec![]);
        }
        let mut switched = vec![];
        for (kind, id) in crate::mirrors::fastest_mirrors(&self.test_package_mirrors(None).await) {
            self.set_package_mirror(kind, id.clone()).await?;
            switched.extend(crate::mirrors::find(kind, &id));
        }
        Ok(switched)
    }

    /// Deletes what `SandboxStatus::temporary_bytes` counts and returns how
    /// much that was. Chats that showed one of those screenshots show a
    /// missing file afterwards.
    pub async fn clear_temporary_files(&self) -> Result<u64, CoreError> {
        let dirs = crate::tools::browser::temporary_dirs(&self.inner.sandbox.workspace_dir());
        tokio::task::spawn_blocking(move || {
            let mut freed = 0;
            for dir in dirs {
                let bytes = folder_bytes(&dir);
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => freed += bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(CoreError::Storage { detail: format!("{}: {e}", dir.display()) }),
                }
            }
            Ok(freed)
        })
        .await
        .map_err(|e| CoreError::Internal { detail: e.to_string() })?
    }

    /// The installed skills, by folder.
    pub fn skills(&self) -> Vec<Skill> {
        crate::skills::list(&self.inner.sandbox.workspace_dir(), &self.inner.disabled_skills.read().unwrap())
    }

    /// Install a skill from a GitHub link (as `skill_install` does) and turn
    /// it on: asking for it is asking to use it.
    pub async fn install_skill(&self, source: String) -> Result<Skill, CoreError> {
        let done = crate::skills::install(&source, &self.inner.sandbox.workspace_dir()).await?;
        self.set_skill_enabled(done.folder.clone(), true).await?;
        self.skills()
            .into_iter()
            .find(|s| s.folder == done.folder)
            .ok_or(CoreError::NoSuchSkill { folder: done.folder })
    }

    pub async fn remove_skill(&self, folder: String) -> Result<(), CoreError> {
        let ws = self.inner.sandbox.workspace_dir();
        let f = folder.clone();
        tokio::task::spawn_blocking(move || crate::skills::remove(&ws, &f))
            .await
            .map_err(|e| CoreError::Internal { detail: e.to_string() })??;
        // A skill installed again under this folder starts on.
        self.set_skill_enabled(folder, true).await
    }

    pub async fn set_skill_enabled(&self, folder: String, enabled: bool) -> Result<(), CoreError> {
        let snapshot = {
            let mut off = self.inner.disabled_skills.write().unwrap();
            let changed = if enabled { off.remove(&folder) } else { off.insert(folder) };
            if !changed {
                return Ok(());
            }
            off.clone()
        };
        let value = serde_json::to_value(snapshot).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
        self.inner.store.put_setting("skills_disabled", value).await.map_err(storage)
    }

    /// A skill's `SKILL.md`, for showing it.
    pub fn skill_instructions(&self, folder: String) -> Result<String, CoreError> {
        crate::skills::instructions(&self.inner.sandbox.workspace_dir(), &folder)
    }

    pub fn workspace_dir(&self) -> std::path::PathBuf {
        self.inner.sandbox.workspace_dir()
    }

    pub fn resolve_file(&self, reference: &str) -> Option<std::path::PathBuf> {
        crate::files::resolve(reference, &self.inner.sandbox.workspace_dir())
    }

    // -- settings --------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().unwrap().clone()
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<(), CoreError> {
        let value = serde_json::to_value(&settings).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
        self.inner.store.put_setting("settings", value).await.map_err(storage)?;
        *self.inner.settings.write().unwrap() = settings;
        Ok(())
    }

    pub async fn list_models(&self, endpoint: Endpoint) -> Result<Vec<ModelInfo>, CoreError> {
        let mut models = self.fetch_models(&endpoint).await?;
        crate::providers::model_order::sort_models(&mut models);
        let reported: Vec<(String, u64)> =
            models
                .iter()
                .filter_map(|m| m.context_window.map(|w| (window_key(&ModelChoice { endpoint_id: endpoint.id.clone(), model: m.id.clone() }), w)))
                .collect();
        if !reported.is_empty() {
            let snapshot = {
                let mut w = self.inner.windows.write().unwrap();
                w.extend(reported);
                w.clone()
            };
            let value = serde_json::to_value(snapshot).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
            self.inner.store.put_setting("model_windows", value).await.map_err(storage)?;
        }
        Ok(models)
    }

    /// Set the context window of one model by hand (`None` goes back to
    /// the endpoint's figure). Kept apart from what endpoints report, so a
    /// later fetch does not undo it.
    pub async fn set_model_window(&self, choice: ModelChoice, window: Option<u64>) -> Result<(), CoreError> {
        let snapshot = {
            let mut w = self.inner.custom_windows.write().unwrap();
            let key = window_key(&choice);
            match window.filter(|v| *v > 0) {
                Some(v) => w.insert(key, v),
                None => w.remove(&key),
            };
            w.clone()
        };
        let value = serde_json::to_value(snapshot).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
        self.inner.store.put_setting("model_window_overrides", value).await.map_err(storage)
    }

    pub fn model_window(&self, choice: ModelChoice) -> ModelWindow {
        let key = window_key(&choice);
        ModelWindow {
            reported: self.inner.windows.read().unwrap().get(&key).copied(),
            custom: self.inner.custom_windows.read().unwrap().get(&key).copied(),
        }
    }

    fn window_for(&self, choice: &ModelChoice) -> Option<u64> {
        let w = self.model_window(choice.clone());
        w.custom.or(w.reported)
    }

    async fn fetch_models(&self, endpoint: &Endpoint) -> Result<Vec<ModelInfo>, CoreError> {
        let key = self.key_for(endpoint)?;
        match endpoint.protocol {
            Protocol::OpenAi => OpenAi::new(&endpoint.base_url, key, self.inner.capture.clone()).list_models().await,
            Protocol::Anthropic => Anthropic::new(&endpoint.base_url, key, self.inner.capture.clone()).list_models().await,
            Protocol::Gemini => Gemini::new(&endpoint.base_url, key, self.inner.capture.clone()).list_models().await,
        }
    }

    fn key_for(&self, ep: &Endpoint) -> Result<String, CoreError> {
        self.inner
            .secrets
            .secret(&ep.secret_ref)
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| CoreError::MissingKey { endpoint_name: ep.name.clone() })
    }

    // -- terminals -------------------------------------------------------------

    /// A shell for the person. Its output goes to
    /// `output`; `exited` is called when it ends, after the last output.
    pub async fn open_terminal(
        &self,
        rows: u16,
        cols: u16,
        output: crate::sandbox::TerminalOutput,
        exited: Box<dyn FnOnce(Option<i32>) + Send>,
    ) -> Result<String, CoreError> {
        let id = agent::new_id();
        let engine = self.clone();
        let forget = id.clone();
        let exited: crate::sandbox::TerminalExited = Box::new(move |code| {
            engine.inner.terminals.lock().unwrap().remove(&forget);
            exited(code);
        });
        let terminal = self
            .inner
            .sandbox
            .open_terminal(rows, cols, output, exited)
            .await
            .map_err(|e| CoreError::Sandbox { detail: e.to_string() })?;
        self.inner.terminals.lock().unwrap().insert(id.clone(), terminal);
        Ok(id)
    }

    fn terminal(&self, id: &str) -> Result<Arc<dyn crate::sandbox::Terminal>, CoreError> {
        self.inner.terminals.lock().unwrap().get(id).cloned().ok_or_else(|| CoreError::NoSuchTerminal { terminal_id: id.to_string() })
    }

    pub fn terminal_input(&self, id: &str, bytes: Vec<u8>) -> Result<(), CoreError> {
        self.terminal(id)?.input(bytes);
        Ok(())
    }

    pub fn terminal_resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), CoreError> {
        self.terminal(id)?.resize(rows, cols);
        Ok(())
    }

    /// End the shell. `exited` still follows.
    pub fn terminal_close(&self, id: &str) -> Result<(), CoreError> {
        self.terminal(id)?.close();
        Ok(())
    }

    // -- sessions --------------------------------------------------------------

    /// A new session. It exists in memory until something is said in it, so
    /// opening one and walking away leaves nothing behind.
    pub async fn create_session(&self, model: Option<ModelChoice>) -> Result<SessionInfo, CoreError> {
        let now = now_millis();
        let info = SessionInfo {
            id: agent::new_id(),
            title: None,
            model: model.or_else(|| self.settings().default_model),
            thinking: None,
            created_at: now,
            updated_at: now,
            preview: None,
            pinned_at: None,
        };
        let slot = Arc::new(Slot { live: Mutex::new(Live { info: info.clone(), pending: true, seq: 0, turn: None, titling: false }) });
        self.inner.sessions.lock().await.insert(info.id.clone(), slot);
        Ok(info)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, CoreError> {
        self.inner.store.list_sessions().await.map_err(storage)
    }

    pub async fn delete_session(&self, id: String) -> Result<(), CoreError> {
        if let Some(slot) = self.inner.sessions.lock().await.remove(&id) {
            if let Some(t) = &slot.live.lock().await.turn {
                t.cancel.cancel();
            }
        }
        self.inner.store.delete_session(id).await.map_err(storage)
    }

    async fn slot(&self, id: &str) -> Result<Arc<Slot>, CoreError> {
        let mut map = self.inner.sessions.lock().await;
        if let Some(s) = map.get(id) {
            return Ok(s.clone());
        }
        let info = self
            .inner
            .store
            .session(id.to_string())
            .await
            .map_err(storage)?
            .ok_or_else(|| CoreError::SessionNotFound { session_id: id.to_string() })?;
        let slot = Arc::new(Slot { live: Mutex::new(Live { info, pending: false, seq: 0, turn: None, titling: false }) });
        map.insert(id.to_string(), slot.clone());
        Ok(slot)
    }

    /// The session as JSON — its settings, messages with every part,
    /// summaries and last error — for the user to copy when reporting a
    /// problem. Holds no endpoint keys: those are not part of a session.
    pub async fn export_session(&self, id: String) -> Result<String, CoreError> {
        let snap = self.snapshot(id).await?;
        Ok(serde_json::to_string_pretty(&snap).expect("a snapshot always serialises"))
    }

    pub async fn snapshot(&self, id: String) -> Result<Snapshot, CoreError> {
        let slot = self.slot(&id).await?;
        let live = slot.live.lock().await;
        let (messages, queued, compactions, last_error) = if live.pending {
            (vec![], vec![], vec![], None)
        } else {
            (
                self.inner.store.messages(id.clone()).await.map_err(storage)?,
                self.inner.store.queued(id.clone()).await.map_err(storage)?,
                self.inner.store.compactions(id.clone()).await.map_err(storage)?,
                // A running turn has not failed (yet).
                if live.turn.is_some() { None } else { self.inner.store.last_turn_error(id.clone()).await.map_err(storage)? },
            )
        };
        Ok(Snapshot {
            session: live.info.clone(),
            messages,
            turn: live.turn.as_ref().map(|t| RunningTurn {
                turn_id: t.id.clone(),
                streaming: t.streaming.clone(),
                running_tools: t.running.clone(),
            }),
            queued,
            compactions,
            last_error,
            seq: live.seq,
        })
    }

    pub async fn set_session_model(&self, id: String, model: ModelChoice) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let mut live = slot.live.lock().await;
        if !live.pending {
            self.inner.store.set_session_model(id.clone(), model.clone()).await.map_err(storage)?;
        }
        live.info.model = Some(model);
        let info = live.info.clone();
        self.emit_locked(&mut live, EventKind::SessionUpdated { info: info.clone() });
        Ok(info)
    }

    /// Switch thinking for one session; `None` follows the settings.
    pub async fn set_session_thinking(&self, id: String, thinking: Option<bool>) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let mut live = slot.live.lock().await;
        if !live.pending {
            self.inner.store.set_thinking(id.clone(), thinking).await.map_err(storage)?;
        }
        live.info.thinking = thinking;
        let info = live.info.clone();
        self.emit_locked(&mut live, EventKind::SessionUpdated { info: info.clone() });
        Ok(info)
    }

    // -- turns -----------------------------------------------------------------

    /// Say something. If a turn is running, it waits in the queue (stored)
    /// and runs when the current turn ends.
    pub async fn send(&self, session_id: String, text: String) -> Result<(), CoreError> {
        self.send_with(session_id, text, vec![]).await
    }

    /// Say something with files attached. They are copied into the
    /// workspace's `attachments/` before anything else happens, so the
    /// model can open them and the app may drop its own copies.
    pub async fn send_with(&self, session_id: String, text: String, attachments: Vec<AttachmentSource>) -> Result<(), CoreError> {
        let mut parts = Vec::new();
        if !text.trim().is_empty() {
            parts.push(Part::Text { text });
        }
        let workspace = self.inner.sandbox.workspace_dir();
        for a in attachments {
            let (ws, src, name) = (workspace.clone(), PathBuf::from(&a.host_path), a.name.clone());
            let (path, size) = tokio::task::spawn_blocking(move || crate::files::import_attachment(&ws, &src, &name))
                .await
                .map_err(|e| CoreError::Internal { detail: e.to_string() })?
                .map_err(|e| CoreError::Storage { detail: format!("{}: {e}", a.name) })?;
            parts.push(Part::Attachment { path, name: a.name, mime: a.mime, size });
        }
        if parts.is_empty() {
            return Ok(());
        }
        let slot = self.slot(&session_id).await?;
        let mut live = slot.live.lock().await;
        if live.pending {
            self.inner.store.insert_session(live.info.clone()).await.map_err(storage)?;
            live.pending = false;
        }
        if live.turn.is_some() {
            self.inner.store.push_queued(session_id.clone(), parts).await.map_err(storage)?;
            let queued = self.inner.store.queued(session_id).await.map_err(storage)?;
            self.emit_locked(&mut live, EventKind::Queued { queued });
            return Ok(());
        }
        self.start_turn(slot.clone(), &mut live, Some(parts)).await
    }

    /// Answer the last user message again: everything after it is deleted
    /// and a new turn runs. With `text`, the message is edited first — or,
    /// with `message_id`, an earlier user message is, and everything after
    /// it goes. The deleted messages are what the user asked to discard, so
    /// the deletion is real (not a mark) and not undoable.
    pub async fn retry(&self, session_id: String, message_id: Option<String>, text: Option<String>) -> Result<(), CoreError> {
        let slot = self.slot(&session_id).await?;
        let mut live = slot.live.lock().await;
        if live.turn.is_some() {
            return Err(CoreError::Busy);
        }
        let messages = self.inner.store.messages(session_id.clone()).await.map_err(storage)?;
        let target = match &message_id {
            Some(id) => messages.iter().find(|m| &m.id == id && m.role == Role::User),
            None => messages.iter().rev().find(|m| m.role == Role::User),
        }
        .ok_or(CoreError::NothingToRetry)?;
        // Checked before anything is deleted, so a missing key leaves the
        // conversation as it was.
        self.turn_provider(&live)?;
        self.inner.store.truncate(session_id.clone(), Some(target.id.clone())).await.map_err(storage)?;
        self.emit_locked(&mut live, EventKind::Truncated { after: Some(target.id.clone()) });
        let compactions = self.inner.store.compactions(session_id.clone()).await.map_err(storage)?;
        self.emit_locked(&mut live, EventKind::CompactionsChanged { compactions });
        if let Some(text) = text {
            let mut edited = target.clone();
            edited.parts = edited
                .parts
                .into_iter()
                .filter(|p| !matches!(p, Part::Text { .. }))
                .chain(std::iter::once(Part::Text { text }))
                .collect();
            self.inner.store.replace_parts(edited.id.clone(), edited.parts.clone()).await.map_err(storage)?;
            self.emit_locked(&mut live, EventKind::MessageCommitted { message: edited });
        }
        self.start_turn(slot.clone(), &mut live, None).await
    }

    /// Carry on a turn that was cut off (stopped, failed, or ended by the
    /// system) from the transcript as it stands.
    pub async fn resume(&self, session_id: String) -> Result<(), CoreError> {
        let slot = self.slot(&session_id).await?;
        let mut live = slot.live.lock().await;
        if live.turn.is_some() {
            return Err(CoreError::Busy);
        }
        // The model owes a reply only after the user or a tool spoke last.
        let messages = self.inner.store.messages(session_id).await.map_err(storage)?;
        if !messages.last().is_some_and(|m| m.role != Role::Assistant) {
            return Err(CoreError::NothingToResume);
        }
        self.start_turn(slot.clone(), &mut live, None).await
    }

    pub async fn rename_session(&self, id: String, title: String) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let mut live = slot.live.lock().await;
        let title = Some(title.trim().to_string()).filter(|t| !t.is_empty());
        if !live.pending {
            self.inner.store.set_title(id.clone(), title.clone()).await.map_err(storage)?;
        }
        live.info.title = title;
        let info = live.info.clone();
        self.emit_locked(&mut live, EventKind::SessionUpdated { info: info.clone() });
        Ok(info)
    }

    /// Pin to the top of the list, or unpin.
    pub async fn set_pinned(&self, id: String, pinned: bool) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let mut live = slot.live.lock().await;
        let at = pinned.then(now_millis);
        if !live.pending {
            self.inner.store.set_pinned(id.clone(), at).await.map_err(storage)?;
        }
        live.info.pinned_at = at;
        let info = live.info.clone();
        self.emit_locked(&mut live, EventKind::SessionUpdated { info: info.clone() });
        Ok(info)
    }

    /// A new session with a copy of this one's messages and summaries, named
    /// `title` (the app words it, in the user's language). Not pinned.
    pub async fn duplicate_session(&self, id: String, title: Option<String>) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let live = slot.live.lock().await;
        let now = now_millis();
        let info = SessionInfo {
            id: agent::new_id(),
            title: title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).or_else(|| live.info.title.clone()),
            model: live.info.model.clone(),
            thinking: live.info.thinking,
            created_at: now,
            updated_at: now,
            preview: live.info.preview.clone(),
            pinned_at: None,
        };
        let pending = live.pending;
        drop(live);
        if !pending {
            self.inner.store.duplicate_session(id, info.clone()).await.map_err(storage)?;
        }
        let slot = Arc::new(Slot { live: Mutex::new(Live { info: info.clone(), pending, seq: 0, turn: None, titling: false }) });
        self.inner.sessions.lock().await.insert(info.id.clone(), slot);
        Ok(info)
    }

    /// Ask the session's model for a new title, replacing the current one.
    pub async fn regenerate_title(&self, id: String) -> Result<SessionInfo, CoreError> {
        let slot = self.slot(&id).await?;
        let (provider, model) = {
            let live = slot.live.lock().await;
            self.turn_provider(&live)?
        };
        let transcript = self.inner.store.messages(id.clone()).await.map_err(storage)?;
        let req = crate::title::request(&model.model, &transcript).ok_or(CoreError::EmptyResponse)?;
        let title = crate::title::generate(provider.as_ref(), req, std::time::Duration::from_secs(60)).await?;
        self.rename_session(id, title).await
    }

    /// The conversation as plain text: who said what, and the calls made,
    /// for reading or pasting elsewhere. Thinking and tool output are left
    /// out; the JSON export keeps everything.
    pub async fn export_session_text(&self, id: String) -> Result<String, CoreError> {
        let snap = self.snapshot(id).await?;
        let mut out = String::new();
        if let Some(title) = &snap.session.title {
            out.push_str(&format!("{title}\n\n"));
        }
        for m in &snap.messages {
            let mut body = Vec::new();
            for p in &m.parts {
                match p {
                    Part::Text { text } if !text.trim().is_empty() => body.push(text.trim().to_string()),
                    Part::Attachment { name, .. } => body.push(format!("[{name}]")),
                    Part::ToolCall { name, title, .. } => body.push(format!("> {} ({name})", title.clone().unwrap_or_else(|| name.clone()))),
                    _ => {}
                }
            }
            if body.is_empty() {
                continue;
            }
            let who = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => continue,
            };
            out.push_str(&format!("{who}:\n{}\n\n", body.join("\n")));
        }
        Ok(out.trim_end().to_string() + "\n")
    }

    /// Delete every message and anything queued; the session itself, its
    /// title and its model stay.
    pub async fn clear_session(&self, id: String) -> Result<(), CoreError> {
        let slot = self.slot(&id).await?;
        let mut live = slot.live.lock().await;
        if live.turn.is_some() {
            return Err(CoreError::Busy);
        }
        if !live.pending {
            self.inner.store.truncate(id.clone(), None).await.map_err(storage)?;
        }
        self.emit_locked(&mut live, EventKind::Truncated { after: None });
        if !live.pending {
            let compactions = self.inner.store.compactions(id.clone()).await.map_err(storage)?;
            self.emit_locked(&mut live, EventKind::CompactionsChanged { compactions });
        }
        self.emit_locked(&mut live, EventKind::Queued { queued: vec![] });
        Ok(())
    }

    /// Summarise the conversation up to its recent turns, so the model sees
    /// the summary in their place. Asked for by the user; nothing is
    /// deleted, and `undo_compaction` restores the full history.
    pub async fn compact(&self, session_id: String) -> Result<Compaction, CoreError> {
        let slot = self.slot(&session_id).await?;
        let (provider, choice) = {
            let live = slot.live.lock().await;
            if live.turn.is_some() {
                return Err(CoreError::Busy);
            }
            self.turn_provider(&live)?
        };
        let messages = self.inner.store.messages(session_id.clone()).await.map_err(storage)?;
        let compactions = self.inner.store.compactions(session_id.clone()).await.map_err(storage)?;
        let (previous, span) = crate::context::compaction_span(&messages, &compactions).ok_or(CoreError::NothingToCompact)?;
        let through = span.last().map(|m| m.id.clone()).ok_or(CoreError::NothingToCompact)?;
        let summary = summarize(provider.as_ref(), &choice.model, previous, span, 0).await?;
        let compaction = Compaction { id: agent::new_id(), through_message_id: through, summary, created_at: now_millis(), undone: false };
        let mut live = slot.live.lock().await;
        self.inner.store.add_compaction(session_id.clone(), compaction.clone()).await.map_err(storage)?;
        let compactions = self.inner.store.compactions(session_id).await.map_err(storage)?;
        self.emit_locked(&mut live, EventKind::CompactionsChanged { compactions });
        Ok(compaction)
    }

    /// Send the summarised messages to the model again.
    pub async fn undo_compaction(&self, session_id: String, compaction_id: String) -> Result<(), CoreError> {
        let slot = self.slot(&session_id).await?;
        let mut live = slot.live.lock().await;
        self.inner.store.set_compaction_undone(compaction_id, true).await.map_err(storage)?;
        let compactions = self.inner.store.compactions(session_id).await.map_err(storage)?;
        self.emit_locked(&mut live, EventKind::CompactionsChanged { compactions });
        Ok(())
    }

    pub async fn search_sessions(&self, query: String) -> Result<Vec<SessionInfo>, CoreError> {
        if query.trim().is_empty() {
            return self.list_sessions().await;
        }
        self.inner.store.search_sessions(query.trim().to_string()).await.map_err(storage)
    }

    pub async fn cancel(&self, session_id: String) -> Result<(), CoreError> {
        let slot = self.slot(&session_id).await?;
        let live = slot.live.lock().await;
        if let Some(t) = &live.turn {
            t.cancel.cancel();
        }
        Ok(())
    }

    /// The provider and model a turn in this session would use.
    fn turn_provider(&self, live: &Live) -> Result<(Arc<dyn Provider>, ModelChoice), CoreError> {
        let choice = live.info.model.clone().or_else(|| self.settings().default_model).ok_or(CoreError::NoModel)?;
        let endpoint = self
            .settings()
            .endpoints
            .into_iter()
            .find(|e| e.id == choice.endpoint_id)
            .ok_or_else(|| CoreError::UnknownEndpoint { endpoint_id: choice.endpoint_id.clone() })?;
        let key = self.key_for(&endpoint)?;
        Ok(((self.inner.providers)(&endpoint, key, &self.inner.capture)?, choice))
    }

    /// Start a turn, with a new user message or (`None`) from the transcript
    /// as it stands.
    async fn start_turn(&self, slot: Arc<Slot>, live: &mut Live, input: Option<Vec<Part>>) -> Result<(), CoreError> {
        let session_id = live.info.id.clone();
        // Resolve everything that can fail before anything is written, so a
        // missing key is an answer to `send`, not a half-started turn.
        let (provider, choice) = self.turn_provider(live)?;
        let turn_id = agent::new_id();
        self.inner.store.start_turn(turn_id.clone(), session_id.clone()).await.map_err(storage)?;
        let user = match input {
            Some(parts) => {
                let user = Message { id: agent::new_id(), role: Role::User, parts, created_at: now_millis() };
                self.inner.store.append_message(session_id.clone(), user.clone()).await.map_err(storage)?;
                Some(user)
            }
            None => None,
        };
        let cancel = CancellationToken::new();
        live.turn = Some(TurnLive { id: turn_id.clone(), cancel: cancel.clone(), streaming: None, running: vec![] });
        self.emit_locked(live, EventKind::TurnStarted { turn_id: turn_id.clone() });
        if let Some(user) = user {
            self.emit_locked(live, EventKind::MessageCommitted { message: user });
        }

        let transcript = crate::context::visible(
            &self.inner.store.messages(session_id.clone()).await.map_err(storage)?,
            &self.inner.store.compactions(session_id.clone()).await.map_err(storage)?,
        );
        let cfg = TurnConfig {
            session_id: session_id.clone(),
            model: choice.model.clone(),
            system: crate::prompt::system_prompt(&self.inner.sandbox.info(), &self.inner.tools, &self.skills()),
            thinking: live.info.thinking.unwrap_or(self.settings().thinking),
            workspace: Some(self.inner.sandbox.workspace_dir()),
            window: self.window_for(&choice),
            ..TurnConfig::default()
        };
        let engine = self.clone();
        tokio::spawn(async move {
            let host = SessionHost { engine: engine.clone(), slot: slot.clone(), provider: provider.clone(), model: choice.model };
            if let Err(e) = engine.inner.sandbox.boot().await {
                engine.finish_turn(&slot, &turn_id, TurnEnd::Failed(CoreError::Sandbox { detail: e.to_string() })).await;
                return;
            }
            let end = agent::run_turn(&host, provider.as_ref(), &engine.inner.tools, engine.inner.sandbox.clone(), transcript, &cfg, cancel).await;
            engine.finish_turn(&slot, &turn_id, end).await;
        });
        Ok(())
    }

    /// `start_turn` behind a box with its `Send` bound stated: the turn it
    /// starts ends in `finish_turn`, which starts the next one, and without
    /// the box the compiler cannot close that loop when checking `Send`.
    fn start_turn_boxed<'a>(
        &'a self,
        slot: Arc<Slot>,
        live: &'a mut Live,
        input: Option<Vec<Part>>,
    ) -> futures::future::BoxFuture<'a, Result<(), CoreError>> {
        Box::pin(self.start_turn(slot, live, input))
    }

    /// Close the turn, then start the next queued input — after a failure
    /// too, so nothing the user sent is dropped.
    async fn finish_turn(&self, slot: &Arc<Slot>, turn_id: &str, end: TurnEnd) {
        let mut live = slot.live.lock().await;
        let session_id = live.info.id.clone();
        let (state, outcome) = match end {
            TurnEnd::Completed => ("completed", TurnOutcome::Completed),
            TurnEnd::Cancelled => ("cancelled", TurnOutcome::Cancelled),
            TurnEnd::Failed(e) => ("failed", TurnOutcome::Failed { error: e }),
        };
        let error_json = match &outcome {
            TurnOutcome::Failed { error } => serde_json::to_string(error).ok(),
            _ => None,
        };
        if let Err(e) = self.inner.store.end_turn(turn_id.to_string(), state, error_json).await {
            tracing::error!("could not record the end of turn {turn_id}: {e}");
        }
        let cancelled = matches!(outcome, TurnOutcome::Cancelled);
        // Session details first: `TurnFinished` is the last event of a turn,
        // and a one-shot client may stop listening at it.
        if let Ok(Some(info)) = self.inner.store.session(session_id.clone()).await {
            live.info = info.clone();
            self.emit_locked(&mut live, EventKind::SessionUpdated { info });
        }
        live.turn = None;
        self.emit_locked(&mut live, EventKind::TurnFinished { turn_id: turn_id.to_string(), outcome });
        // Stopping means stop: queued messages stay queued and visible.
        if cancelled {
            return;
        }
        match self.inner.store.pop_queued(session_id.clone()).await {
            Ok(Some(next)) => {
                let queued = self.inner.store.queued(session_id).await.unwrap_or_default();
                self.emit_locked(&mut live, EventKind::Queued { queued });
                if let Err(e) = self.start_turn_boxed(slot.clone(), &mut live, Some(next.clone())).await {
                    // Could not start: put it back so it is not lost.
                    let _ = self.inner.store.push_queued(live.info.id.clone(), next).await;
                    tracing::error!("queued input could not start: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::error!("could not read the queue: {e}"),
        }
    }

    /// Apply an event to the live state, number it, and broadcast it. The
    /// caller holds the session lock, which is what keeps snapshots and
    /// sequence numbers consistent.
    fn emit_locked(&self, live: &mut Live, kind: EventKind) {
        apply(live, &kind);
        live.seq += 1;
        let _ = self.inner.events.send(Event { session_id: live.info.id.clone(), seq: live.seq, kind });
    }

    /// Close out turns a killed process left running and make their
    /// transcripts sendable again.
    async fn recover(&self) -> Result<(), CoreError> {
        for (turn_id, session_id) in self.inner.store.unfinished_turns().await.map_err(storage)? {
            let messages = self.inner.store.messages(session_id.clone()).await.map_err(storage)?;
            if let Some(last) = messages.last().filter(|m| m.role == Role::Assistant) {
                let open: Vec<String> = last
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolCall { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                if !open.is_empty() {
                    let seal = agent::seal(&open, "not run: the app stopped during the turn");
                    self.inner.store.append_message(session_id.clone(), seal).await.map_err(storage)?;
                }
            }
            let err = serde_json::to_string(&CoreError::Internal { detail: "interrupted".into() }).ok();
            self.inner.store.end_turn(turn_id, "interrupted", err).await.map_err(storage)?;
        }
        Ok(())
    }
}

/// Summarise `span`, carrying `previous` in. If the summariser's own
/// request is too long, the span is split in two at a message boundary and
/// summarised in order, the first summary carried into the second; at most
/// three levels deep.
fn summarize<'a>(
    provider: &'a dyn Provider,
    model: &'a str,
    previous: Option<String>,
    span: &'a [Message],
    depth: u32,
) -> futures::future::BoxFuture<'a, Result<String, CoreError>> {
    Box::pin(async move {
        let mut text = String::new();
        if let Some(p) = &previous {
            text.push_str(&format!("Summary of the conversation before this part:\n\n{p}\n\n"));
        }
        text.push_str(&format!("Conversation to summarise:\n\n{}", crate::context::transcript_text(span)));
        let req = crate::providers::ChatRequest {
            model: model.to_string(),
            system: crate::context::SUMMARY_SYSTEM.to_string(),
            messages: vec![Message { id: String::new(), role: Role::User, parts: vec![Part::Text { text }], created_at: 0 }],
            tools: vec![],
            thinking: false,
            workspace: None,
        };
        match crate::title::collect_text(provider, req, std::time::Duration::from_secs(300)).await {
            Ok(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            Ok(_) => Err(CoreError::EmptyResponse),
            Err(CoreError::ContextTooLong { .. }) if depth < 3 && span.len() >= 2 => {
                let mid = span.len() / 2;
                let first = summarize(provider, model, previous, &span[..mid], depth + 1).await?;
                summarize(provider, model, Some(first), &span[mid..], depth + 1).await
            }
            Err(e) => Err(e),
        }
    })
}

/// The live-state half of the reducer: what the core itself keeps of the
/// events it sends, so a snapshot mid-turn shows the partial reply.
fn apply(live: &mut Live, kind: &EventKind) {
    let Some(turn) = live.turn.as_mut() else { return };
    match kind {
        EventKind::MessageStarted { message_id } => {
            turn.streaming = Some(Message { id: message_id.clone(), role: Role::Assistant, parts: vec![], created_at: now_millis() });
        }
        EventKind::TextDelta { delta, .. } => {
            if let Some(m) = turn.streaming.as_mut() {
                match m.parts.last_mut() {
                    Some(Part::Text { text }) => text.push_str(delta),
                    _ => m.parts.push(Part::Text { text: delta.clone() }),
                }
            }
        }
        EventKind::ThinkingDelta { delta, .. } => {
            if let Some(m) = turn.streaming.as_mut() {
                match m.parts.last_mut() {
                    Some(Part::Thinking { text, .. }) => text.push_str(delta),
                    _ => m.parts.push(Part::Thinking { text: delta.clone(), signature: None, redacted: None, origin: None }),
                }
            }
        }
        EventKind::ToolCallStarted { call_id, name, .. } => {
            if let Some(m) = turn.streaming.as_mut() {
                m.parts.push(Part::ToolCall { id: call_id.clone(), name: name.clone(), input_json: String::new(), title: None, signature: None });
            }
        }
        EventKind::ToolCallReady { call_id, input_json, title, .. } => {
            if let Some(m) = turn.streaming.as_mut() {
                for p in &mut m.parts {
                    if let Part::ToolCall { id, input_json: i, title: t, .. } = p {
                        if id == call_id {
                            *i = input_json.clone();
                            *t = title.clone();
                        }
                    }
                }
            }
        }
        EventKind::ToolRunning { call_id } => turn.running.push(call_id.clone()),
        EventKind::MessageCommitted { message } => match message.role {
            Role::Assistant => turn.streaming = None,
            Role::Tool => {
                for p in &message.parts {
                    if let Part::ToolResult { call_id, .. } = p {
                        turn.running.retain(|r| r != call_id);
                    }
                }
            }
            Role::User => {}
        },
        _ => {}
    }
}

/// A turn's view of its session.
struct SessionHost {
    engine: Engine,
    slot: Arc<Slot>,
    /// The turn's model, which also writes the session's title.
    provider: Arc<dyn Provider>,
    model: String,
}

impl SessionHost {
    /// Title the session in the background once it has a first reply.
    fn start_title(&self, live: &mut Live) {
        if live.info.title.is_some() || live.titling {
            return;
        }
        live.titling = true;
        let engine = self.engine.clone();
        let slot = self.slot.clone();
        let provider = self.provider.clone();
        let model = self.model.clone();
        let id = live.info.id.clone();
        tokio::spawn(async move {
            let transcript = engine.inner.store.messages(id.clone()).await.unwrap_or_default();
            let generated = match crate::title::request(&model, &transcript) {
                Some(req) => crate::title::generate(provider.as_ref(), req, std::time::Duration::from_secs(60)).await,
                None => Err(CoreError::EmptyResponse),
            };
            let title = match generated {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!("title for {id}: {e}; using the first message");
                    crate::title::fallback(&transcript)
                }
            };
            let mut live = slot.live.lock().await;
            live.titling = false;
            // Named by the user in the meantime: theirs wins.
            let (Some(title), None) = (title, live.info.title.as_ref()) else { return };
            if let Err(e) = engine.inner.store.set_title(id, Some(title.clone())).await {
                tracing::error!("could not store a title: {e}");
                return;
            }
            live.info.title = Some(title);
            let info = live.info.clone();
            engine.emit_locked(&mut live, EventKind::SessionUpdated { info });
        });
    }
}

#[async_trait]
impl TurnHost for SessionHost {
    async fn emit(&self, kind: EventKind) {
        let mut live = self.slot.live.lock().await;
        self.engine.emit_locked(&mut live, kind);
    }

    async fn commit(&self, message: Message) -> Result<(), CoreError> {
        let mut live = self.slot.live.lock().await;
        let id = live.info.id.clone();
        self.engine.inner.store.append_message(id, message.clone()).await.map_err(storage)?;
        let is_reply = message.role == Role::Assistant;
        self.engine.emit_locked(&mut live, EventKind::MessageCommitted { message });
        if is_reply {
            self.start_title(&mut live);
        }
        Ok(())
    }
}

fn window_key(choice: &ModelChoice) -> String {
    format!("{}/{}", choice.endpoint_id, choice.model)
}

/// The bytes of the files under `dir`. Links are counted as themselves, not
/// followed: the workspace appears inside the guest's tree through one.
fn folder_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Writes a source's config into the guest, through the sandbox.
async fn apply_mirror(sandbox: &dyn Sandbox, kind: MirrorKind, id: &str) -> Result<(), CoreError> {
    let mirror = crate::mirrors::find(kind, id).ok_or_else(|| CoreError::Internal { detail: format!("no {} mirror {id}", crate::mirrors::key(kind)) })?;
    let branch = sandbox.package_branch().unwrap_or_else(|| "latest-stable".into());
    let (path, content) = crate::mirrors::config(&mirror, &branch);
    sandbox.set_boot_file(path.to_string(), content).await.map_err(|e| CoreError::Sandbox { detail: e.to_string() })
}
