//! MCP servers: other programs' tools, offered to the model as its own.
//!
//! The core is the client. An HTTP server is spoken to directly; a stdio
//! server runs in the guest as a long-running process (`Sandbox::spawn`),
//! one JSON message per line each way. A server's tools are listed when it
//! is added and kept with its entry, so they are offered without starting
//! it; the process starts when one of them is first called and lives as
//! long as the engine does (docs/architecture.md 4.10).

use crate::sandbox::{Process, Sandbox, GUEST_WORKSPACE};
use crate::store::Store;
use crate::tools::{clip, object_schema, Tool, ToolContext, ToolImage, ToolOutput, ToolSource, ToolSpec, TITLE_ARG};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use solos_api::{CoreError, McpServer, McpTool};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};
use tokio_util::sync::CancellationToken;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SETTING: &str = "mcp_servers";
/// `npx -y` fetches the package first, and the guest is emulated: a first
/// start took 505 s for 12306-mcp (225 packages) in the simulator.
const STDIO_START: Duration = Duration::from_secs(900);
const HTTP_START: Duration = Duration::from_secs(30);
const CALL: Duration = Duration::from_secs(300);
/// The same limit as a shell command's output.
const MAX_RESULT_CHARS: usize = 20_000;
const MAX_DESCRIPTION_CHARS: usize = 1_500;
/// Most APIs cap a tool's name here.
const MAX_TOOL_NAME: usize = 64;
/// Where images a tool returned are put for the model to see.
const IMAGES: &str = ".solos/mcp";

// ---------------------------------------------------------------- config

/// Servers from pasted JSON: `{"mcpServers": {name: entry}}` (or
/// `"servers"`, as some editors write it), a bare `{name: entry}` map, or a
/// single entry with a `name`.
pub fn parse_config(text: &str) -> Result<Vec<McpServer>, CoreError> {
    let bad = |detail: String| CoreError::NotAnMcpConfig { detail };
    // A phone's keyboard turns typed quotes into curly ones; JSON has none.
    let text = text.replace(['\u{201C}', '\u{201D}'], "\"").replace(['\u{2018}', '\u{2019}'], "'");
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| bad(format!("not JSON ({e})")))?;
    let obj = v.as_object().ok_or_else(|| bad("not a JSON object".into()))?;
    let entries: Vec<(String, &Value)> = match obj.get("mcpServers").or_else(|| obj.get("servers")).and_then(Value::as_object) {
        Some(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        None if is_entry(obj) => match obj.get("name").and_then(Value::as_str) {
            Some(name) => vec![(name.to_string(), &v)],
            None => return Err(bad("the entry has no `name`".into())),
        },
        None => obj.iter().map(|(k, v)| (k.clone(), v)).collect(),
    };
    if entries.is_empty() {
        return Err(bad("there are no servers in it".into()));
    }
    entries.into_iter().map(|(name, e)| entry(&name, e)).collect()
}

fn is_entry(o: &Map<String, Value>) -> bool {
    o.contains_key("command") || o.contains_key("url")
}

/// One server from its entry, as READMEs give it.
pub fn entry(name: &str, e: &Value) -> Result<McpServer, CoreError> {
    let name = name.trim();
    let bad = |detail: String| CoreError::NotAnMcpConfig { detail };
    if name.is_empty() {
        return Err(bad("a server needs a name".into()));
    }
    let o = e.as_object().ok_or_else(|| bad(format!("{name}: the entry is not an object")))?;
    let text = |k: &str| o.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if text("type") == "sse" {
        return Err(bad(format!("{name}: the older SSE transport is not supported; use the server's streamable HTTP address")));
    }
    let url = [text("url"), text("serverUrl")].into_iter().find(|u| !u.is_empty()).unwrap_or_default();
    let command = text("command");
    if url.is_empty() && command.is_empty() {
        return Err(bad(format!("{name}: the entry has neither `command` nor `url`")));
    }
    let args = match o.get("args") {
        Some(Value::Array(a)) => a.iter().map(scalar).collect(),
        Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
        _ => vec![],
    };
    let map = |k: &str| -> HashMap<String, String> {
        o.get(k).and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), scalar(v))).collect()).unwrap_or_default()
    };
    Ok(McpServer {
        name: name.to_string(),
        url,
        headers: map("headers"),
        command,
        args,
        env: map("env"),
        enabled: !o.get("disabled").and_then(Value::as_bool).unwrap_or(false),
        tools: vec![],
        error: None,
    })
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The name a server's tool goes by: `mcp_<server>_<tool>`, in the letters
/// every API accepts.
pub fn tool_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
    format!("mcp_{}_{}", clean(server), clean(tool)).chars().take(MAX_TOOL_NAME).collect()
}

/// The command line a stdio server runs as, every word quoted.
fn command_line(s: &McpServer) -> String {
    let quote = |w: &str| format!("'{}'", w.replace('\'', "'\\''"));
    std::iter::once(s.command.as_str()).chain(s.args.iter().map(String::as_str)).map(quote).collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------- transport

#[async_trait]
trait Transport: Send + Sync {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String>;
    async fn notify(&self, method: &str, params: Value) -> Result<(), String>;
    fn alive(&self) -> bool;
    fn close(&self);
}

/// A JSON-RPC answer's result, or its error as text.
fn answer(msg: &Value) -> Result<Value, String> {
    match msg.get("error") {
        Some(e) => Err(match e.get("message").and_then(Value::as_str) {
            Some(m) => format!("{m} (error {})", e.get("code").map(scalar).unwrap_or_default()),
            None => e.to_string(),
        }),
        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
    }
}

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
/// A server's connection, once started; locked while it starts.
type Slot = Arc<Mutex<Option<Arc<dyn Transport>>>>;

/// A server in the guest, one JSON message per line on its stdin and stdout.
struct Stdio {
    stdin: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    stderr: Arc<StdMutex<String>>,
    kill: Arc<dyn Fn() + Send + Sync>,
}

impl Stdio {
    fn start(p: Process) -> Self {
        let Process { stdin, mut stdout, stderr, kill } = p;
        let pending: Pending = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (pend, live, reply, tail) = (pending.clone(), alive.clone(), stdin.clone(), stderr.clone());
        tokio::spawn(async move {
            let mut buf = Vec::new();
            while let Some(chunk) = stdout.recv().await {
                buf.extend_from_slice(&chunk);
                while let Some(end) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=end).collect();
                    Self::handle(&line, &pend, &reply);
                }
            }
            live.store(false, Ordering::SeqCst);
            let why = ended(&tail);
            for (_, tx) in pend.lock().unwrap().drain() {
                let _ = tx.send(Err(why.clone()));
            }
        });
        Self { stdin, pending, next: AtomicU64::new(1), alive, stderr, kill }
    }

    /// An answer goes to whoever asked; a request from the server (it may
    /// ping) is answered at once. Lines that are not JSON are a server's
    /// logging on the wrong stream, and are skipped.
    fn handle(line: &[u8], pending: &Pending, reply: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>) {
        let Ok(msg) = serde_json::from_slice::<Value>(line) else { return };
        let id = msg.get("id").cloned();
        match (msg.get("method").and_then(Value::as_str), id) {
            (Some(method), Some(id)) => {
                let out = if method == "ping" {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("{method} is not supported by this client")}})
                };
                let _ = reply.send(format!("{out}\n").into_bytes());
            }
            (None, Some(id)) => {
                if let Some(tx) = id.as_u64().and_then(|id| pending.lock().unwrap().remove(&id)) {
                    let _ = tx.send(answer(&msg));
                }
            }
            _ => {}
        }
    }
}

fn ended(stderr: &StdMutex<String>) -> String {
    let tail = stderr.lock().unwrap().trim().to_string();
    if tail.is_empty() {
        "the server stopped".into()
    } else {
        let tail: String = tail.chars().rev().take(Process::STDERR_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
        format!("the server stopped. The end of its stderr:\n{tail}")
    }
}

#[async_trait]
impl Transport for Stdio {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.alive() {
            return Err(ended(&self.stderr));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.stdin.send(format!("{msg}\n").into_bytes()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(ended(&self.stderr));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ended(&self.stderr)),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let tail = self.stderr.lock().unwrap().trim().to_string();
                let tail: String = tail.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
                Err(if tail.is_empty() {
                    format!("no answer to {method} in {} s", timeout.as_secs())
                } else {
                    format!("no answer to {method} in {} s. The end of its stderr:\n{tail}", timeout.as_secs())
                })
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.stdin.send(format!("{msg}\n").into_bytes()).map_err(|_| ended(&self.stderr))
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn close(&self) {
        self.alive.store(false, Ordering::SeqCst);
        (self.kill)();
    }
}

/// A server over streamable HTTP: each message a POST, answered with JSON
/// or with an event stream that carries the answer.
struct Http {
    url: String,
    headers: Vec<(String, String)>,
    client: reqwest::Client,
    session: StdMutex<Option<String>>,
    initialized: AtomicBool,
    next: AtomicU64,
}

impl Http {
    fn new(s: &McpServer) -> Self {
        Self {
            url: s.url.clone(),
            headers: s.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            client: crate::providers::http::client(),
            session: Default::default(),
            initialized: AtomicBool::new(false),
            next: AtomicU64::new(1),
        }
    }

    async fn post(&self, body: &Value, timeout: Duration) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(id) = self.session.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", id);
        }
        if self.initialized.load(Ordering::SeqCst) {
            req = req.header("MCP-Protocol-Version", PROTOCOL_VERSION);
        }
        let resp = req.json(body).send().await.map_err(|e| format!("could not reach {}: {e}", self.url))?;
        if let Some(id) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().unwrap() = Some(id.to_string());
        }
        let status = resp.status().as_u16();
        match status {
            200..=299 => Ok(resp),
            401 | 403 => Err(format!("the server refused the request (HTTP {status}); it may need a key in `headers`")),
            _ => {
                let text = resp.text().await.unwrap_or_default();
                Err(format!("HTTP {status}: {}", text.chars().take(500).collect::<String>()))
            }
        }
    }
}

#[async_trait]
impl Transport for Http {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        use futures::StreamExt;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let resp = self.post(&body, timeout).await?;
        let is_stream = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("text/event-stream"));
        let matches = |msg: &Value| msg.get("id").and_then(Value::as_u64) == Some(id) && msg.get("method").is_none();
        let out = if is_stream {
            let mut parser = crate::providers::sse::SseParser::default();
            let mut stream = resp.bytes_stream();
            let mut found = None;
            'read: while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("the answer broke off: {e}"))?;
                for data in parser.push(&chunk) {
                    if let Ok(msg) = serde_json::from_str::<Value>(&data) {
                        if matches(&msg) {
                            found = Some(msg);
                            break 'read;
                        }
                    }
                }
            }
            found.ok_or_else(|| format!("the server's event stream ended without an answer to {method}"))?
        } else {
            let msg: Value = resp.json().await.map_err(|e| format!("the answer was not JSON: {e}"))?;
            match msg {
                Value::Array(all) => all.into_iter().find(|m| matches(m)).ok_or_else(|| format!("no answer to {method} in the batch"))?,
                one => one,
            }
        };
        if method == "initialize" {
            self.initialized.store(true, Ordering::SeqCst);
        }
        answer(&out)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.post(&json!({"jsonrpc": "2.0", "method": method, "params": params}), HTTP_START).await.map(|_| ())
    }

    fn alive(&self) -> bool {
        true
    }

    fn close(&self) {
        // Ends the server's session; it is all right if the server ignores it.
        if let Some(id) = self.session.lock().unwrap().take() {
            let req = self.client.delete(&self.url).header("Mcp-Session-Id", id);
            tokio::spawn(async move {
                let _ = req.send().await;
            });
        }
    }
}

// --------------------------------------------------------------- servers

/// The servers, their live connections, and their tools for the model.
#[derive(Clone)]
pub struct Mcp {
    inner: Arc<Inner>,
}

struct Inner {
    servers: RwLock<Vec<McpServer>>,
    /// One slot per server that has been started, each with its own lock,
    /// so a server that is slow to start holds up only its own calls.
    live: StdMutex<HashMap<String, Slot>>,
    sandbox: Arc<dyn Sandbox>,
    store: Store,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for slot in self.live.lock().unwrap().values() {
            if let Ok(t) = slot.try_lock() {
                if let Some(t) = t.as_ref() {
                    t.close();
                }
            }
        }
    }
}

impl Mcp {
    pub async fn open(store: Store, sandbox: Arc<dyn Sandbox>) -> Result<Self, CoreError> {
        let servers: Vec<McpServer> = store
            .setting(SETTING)
            .await
            .map_err(|e| CoreError::Storage { detail: e.to_string() })?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        Ok(Self { inner: Arc::new(Inner { servers: RwLock::new(servers), live: Default::default(), sandbox, store }) })
    }

    pub fn servers(&self) -> Vec<McpServer> {
        self.inner.servers.read().unwrap().clone()
    }

    fn server(&self, name: &str) -> Result<McpServer, CoreError> {
        self.servers().into_iter().find(|s| s.name == name).ok_or_else(|| CoreError::NoSuchMcpServer { name: name.to_string() })
    }

    async fn save(&self) -> Result<(), CoreError> {
        let value = serde_json::to_value(self.servers()).map_err(|e| CoreError::Internal { detail: e.to_string() })?;
        self.inner.store.put_setting(SETTING, value).await.map_err(|e| CoreError::Storage { detail: e.to_string() })
    }

    fn upsert(&self, server: McpServer) {
        let mut all = self.inner.servers.write().unwrap();
        match all.iter_mut().find(|s| s.name == server.name) {
            Some(s) => *s = server,
            None => all.push(server),
        }
    }

    fn slot(&self, name: &str) -> Slot {
        self.inner.live.lock().unwrap().entry(name.to_string()).or_default().clone()
    }

    async fn stop(&self, name: &str) {
        let slot = self.inner.live.lock().unwrap().remove(name);
        if let Some(slot) = slot {
            if let Some(t) = slot.lock().await.take() {
                t.close();
            }
        }
    }

    /// Start or reach a server, shake hands, and list its tools.
    async fn connect(&self, s: &McpServer) -> Result<(Arc<dyn Transport>, Vec<McpTool>), String> {
        let (transport, start): (Arc<dyn Transport>, Duration) = if !s.url.is_empty() {
            (Arc::new(Http::new(s)), HTTP_START)
        } else {
            let env = s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let p = self.inner.sandbox.spawn(command_line(s), GUEST_WORKSPACE.to_string(), env).await.map_err(|e| e.to_string())?;
            (Arc::new(Stdio::start(p)), STDIO_START)
        };
        let shake = async {
            let init = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "Solos", "version": env!("CARGO_PKG_VERSION")}
            });
            transport.request("initialize", init, start).await?;
            transport.notify("notifications/initialized", json!({})).await?;
            let mut tools = Vec::new();
            let mut cursor: Option<String> = None;
            for _page in 0..20 {
                let params = match &cursor {
                    Some(c) => json!({"cursor": c}),
                    None => json!({}),
                };
                let page = transport.request("tools/list", params, start).await?;
                for t in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
                    let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                    tools.push(McpTool {
                        name: name.to_string(),
                        description: t.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                        input_schema: t.get("inputSchema").map(Value::to_string).unwrap_or_default(),
                    });
                }
                cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
            Ok::<_, String>(tools)
        };
        match shake.await {
            Ok(tools) => Ok((transport, tools)),
            Err(e) => {
                transport.close();
                Err(e)
            }
        }
    }

    /// Add servers (or replace those of the same names), connecting to each.
    /// With `keep_failed`, one that cannot be reached is kept with its error,
    /// for the user to see and fix; otherwise the first failure is the answer
    /// and nothing is saved.
    pub async fn add(&self, servers: Vec<McpServer>, keep_failed: bool) -> Result<Vec<McpServer>, CoreError> {
        let mut done = Vec::new();
        for mut s in servers {
            self.stop(&s.name).await;
            if s.enabled {
                match self.connect(&s).await {
                    Ok((t, tools)) => {
                        s.tools = tools;
                        s.error = None;
                        *self.slot(&s.name).lock().await = Some(t);
                    }
                    Err(e) if keep_failed => s.error = Some(e),
                    Err(detail) => return Err(CoreError::McpServerFailed { name: s.name, detail }),
                }
            }
            self.upsert(s.clone());
            done.push(s);
        }
        self.save().await?;
        Ok(done)
    }

    pub async fn remove(&self, name: &str) -> Result<(), CoreError> {
        self.server(name)?;
        self.stop(name).await;
        self.inner.servers.write().unwrap().retain(|s| s.name != name);
        self.save().await
    }

    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError> {
        let mut s = self.server(name)?;
        if !enabled {
            self.stop(name).await;
        }
        s.enabled = enabled;
        self.upsert(s);
        self.save().await
    }

    /// Connect again and list the tools afresh.
    pub async fn refresh(&self, name: &str) -> Result<McpServer, CoreError> {
        let mut s = self.server(name)?;
        s.enabled = true;
        Ok(self.add(vec![s], true).await?.remove(0))
    }

    /// Call a tool, starting the server first if it is not running (or has
    /// stopped since).
    async fn call(&self, server: &str, tool: &str, args: Value, cancel: &CancellationToken) -> Result<Value, String> {
        let s = self.server(server).map_err(|e| e.to_string())?;
        let transport = {
            let slot = self.slot(server);
            let mut held = slot.lock().await;
            match held.as_ref().filter(|t| t.alive()) {
                Some(t) => t.clone(),
                None => {
                    let (t, tools) = tokio::select! {
                        r = self.connect(&s) => r?,
                        _ = cancel.cancelled() => return Err("stopped".into()),
                    };
                    if tools != s.tools || s.error.is_some() {
                        self.upsert(McpServer { tools, error: None, ..s.clone() });
                        if let Err(e) = self.save().await {
                            tracing::warn!("the MCP server list could not be saved: {e}");
                        }
                    }
                    *held = Some(t.clone());
                    t
                }
            }
        };
        tokio::select! {
            r = transport.request("tools/call", json!({"name": tool, "arguments": args}), CALL) => r,
            _ = cancel.cancelled() => Err("stopped".into()),
        }
    }
}

impl ToolSource for Mcp {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.servers()
            .into_iter()
            .filter(|s| s.enabled)
            .flat_map(|s| {
                let mcp = self.clone();
                s.tools.clone().into_iter().map(move |t| Arc::new(ServerTool { mcp: mcp.clone(), server: s.name.clone(), tool: t }) as Arc<dyn Tool>)
            })
            .collect()
    }
}

/// One of a server's tools, as the model sees it.
struct ServerTool {
    mcp: Mcp,
    server: String,
    tool: McpTool,
}

impl ServerTool {
    /// The server's schema, with the call's label added when it has no
    /// `title` of its own (then the call is labelled by the tool's name).
    fn schema(&self) -> (Value, bool) {
        let mut schema: Value = serde_json::from_str(&self.tool.input_schema).unwrap_or_else(|_| json!({}));
        if !schema.is_object() {
            schema = json!({});
        }
        let o = schema.as_object_mut().expect("an object");
        o.remove("$schema");
        o.insert("type".into(), json!("object"));
        let props = o.entry("properties").or_insert_with(|| json!({}));
        if !props.is_object() {
            *props = json!({});
        }
        if props.get(TITLE_ARG).is_some() {
            return (schema, false);
        }
        let labelled = object_schema(json!({}), &[]);
        props.as_object_mut().unwrap().insert(TITLE_ARG.into(), labelled["properties"][TITLE_ARG].clone());
        let req = o.entry("required").or_insert_with(|| json!([]));
        if let Some(r) = req.as_array_mut() {
            r.insert(0, json!(TITLE_ARG));
        }
        (schema, true)
    }
}

#[async_trait]
impl Tool for ServerTool {
    fn spec(&self) -> ToolSpec {
        let mut description: String = self.tool.description.chars().take(MAX_DESCRIPTION_CHARS).collect();
        if description.len() < self.tool.description.len() {
            description.push('…');
        }
        ToolSpec {
            name: tool_name(&self.server, &self.tool.name),
            description: format!("{description} (Tool `{}` of the MCP server {}.)", self.tool.name, self.server),
            schema: self.schema().0,
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let mut args = input.clone();
        if self.schema().1 {
            if let Some(o) = args.as_object_mut() {
                o.remove(TITLE_ARG);
            }
        }
        match self.mcp.call(&self.server, &self.tool.name, args, &ctx.cancel).await {
            Ok(result) => to_output(&result, &ctx.sandbox.workspace_dir()),
            Err(e) => ToolOutput::error(format!("The MCP server {} could not run {}: {e}", self.server, self.tool.name)),
        }
    }
}

/// A `tools/call` result for the model: text as text, images as workspace
/// files it is shown, anything else as its JSON.
pub fn to_output(result: &Value, workspace: &std::path::Path) -> ToolOutput {
    let mut parts: Vec<String> = Vec::new();
    let mut images = Vec::new();
    for item in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(item.get("text").and_then(Value::as_str).unwrap_or_default().to_string()),
            Some("image") => match save_image(item, workspace) {
                Some(img) => {
                    parts.push(format!("[image: {}]", img.path));
                    images.push(img);
                }
                None => parts.push("[an image that could not be read]".into()),
            },
            Some("resource") => {
                let r = item.get("resource").cloned().unwrap_or_default();
                match r.get("text").and_then(Value::as_str) {
                    Some(t) => parts.push(t.to_string()),
                    None => parts.push(format!("[resource {}]", r.get("uri").map(scalar).unwrap_or_default())),
                }
            }
            Some("resource_link") => parts.push(format!(
                "[{}: {}]",
                item.get("name").map(scalar).unwrap_or_default(),
                item.get("uri").map(scalar).unwrap_or_default()
            )),
            _ => parts.push(item.to_string()),
        }
    }
    let mut text = parts.join("\n");
    if text.trim().is_empty() {
        text = match result.get("structuredContent") {
            Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            None => "(the tool returned nothing)".into(),
        };
    }
    ToolOutput { text: clip(&text, MAX_RESULT_CHARS), is_error: result.get("isError").and_then(Value::as_bool).unwrap_or(false), images }
}

fn save_image(item: &Value, workspace: &std::path::Path) -> Option<ToolImage> {
    let data = base64::engine::general_purpose::STANDARD.decode(item.get("data")?.as_str()?).ok()?;
    let mime = item.get("mimeType").and_then(Value::as_str).unwrap_or("image/png").to_string();
    let ext = match mime.as_str() {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    let dir = workspace.join(IMAGES);
    std::fs::create_dir_all(&dir).ok()?;
    let file = format!("{}.{ext}", uuid::Uuid::new_v4());
    std::fs::write(dir.join(&file), &data).ok()?;
    Some(ToolImage { path: format!("{GUEST_WORKSPACE}/{IMAGES}/{file}"), mime, size: data.len() as u64 })
}

// ---------------------------------------------------------------- mcp_add

/// `mcp_add`: the model adds a server it was given, the way Settings does.
pub struct McpAdd(pub Mcp);

#[async_trait]
impl Tool for McpAdd {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mcp_add".into(),
            description: "Add an MCP server (or replace the one of this name) and connect to it. Give its entry as its README shows it: {\"command\": \"npx\", \"args\": [\"-y\", \"<package>\"]} for one that runs here, or {\"url\": \"https://…\", \"headers\": {…}} for a remote one. A command runs in this Linux system, so install what it needs first; the first start of an npx server downloads it and can take ten minutes. Returns the server's tools, which you call from then on as mcp_<name>_<tool>, or why it could not start.".into(),
            schema: object_schema(
                json!({
                    "name": {"type": "string", "description": "A short name for the server, as in its README's config (the key under mcpServers)."},
                    "config": {"type": "object", "description": "The server's entry: `command`, `args`, `env`; or `url`, `headers`."}
                }),
                &["name", "config"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let name = input.get("name").and_then(Value::as_str).unwrap_or_default();
        let Some(config) = input.get("config") else {
            return ToolOutput::error("`config` is required: the server's entry.");
        };
        // A model may pass the entry as a JSON string.
        let config = match config {
            Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        let server = match entry(name, &config) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(e.to_string()),
        };
        let added = tokio::select! {
            r = self.0.add(vec![server], false) => r,
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Stopped."),
        };
        match added {
            Ok(mut done) => {
                let s = done.remove(0);
                let tools = s
                    .tools
                    .iter()
                    .map(|t| format!("- {}: {}", tool_name(&s.name, &t.name), t.description.lines().next().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolOutput::ok(format!("Added the MCP server `{}` with {}:\n{tools}", s.name, count(s.tools.len())))
            }
            Err(CoreError::McpServerFailed { name, detail }) => {
                ToolOutput::error(format!("The MCP server `{name}` did not start, so it was not added: {detail}"))
            }
            Err(e) => ToolOutput::error(e.to_string()),
        }
    }
}

fn count(tools: usize) -> String {
    if tools == 1 { "1 tool".into() } else { format!("{tools} tools") }
}

/// The system prompt's part about MCP servers, there with none added too.
pub fn prompt_section(servers: &[McpServer]) -> String {
    let mut out = String::from(
        "\n\nMCP servers:\n\
- An MCP server gives you more tools. To add one the user asks for, use `mcp_add` with the server's entry from its README; do not run the server yourself. A stdio server's command runs in this Linux system, so install what it needs first (`apk add nodejs npm` for npx, `apk add python3 py3-pip` for Python ones). Its tools are then yours, named mcp_<server>_<tool>.\n",
    );
    let on: Vec<&McpServer> = servers.iter().filter(|s| s.enabled).collect();
    if on.is_empty() {
        out.push_str("- No servers are added.");
    } else {
        out.push_str("Added servers: ");
        out.push_str(&on.iter().map(|s| format!("{} ({})", s.name, count(s.tools.len()))).collect::<Vec<_>>().join(", "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_are_read_in_every_usual_shape() {
        let all = parse_config(r#"{"mcpServers": {"12306-mcp": {"command": "npx", "args": ["-y", "12306-mcp"]}, "web": {"url": "https://x/mcp", "headers": {"Authorization": "Bearer k"}, "disabled": true}}}"#).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].name.as_str(), all[0].command.as_str(), all[0].args.clone()), ("12306-mcp", "npx", vec!["-y".to_string(), "12306-mcp".to_string()]));
        assert!(all[0].enabled);
        assert_eq!(all[1].headers["Authorization"], "Bearer k");
        assert!(!all[1].enabled);
        assert_eq!(parse_config(r#"{"a": {"command": "x"}}"#).unwrap()[0].name, "a");
        assert_eq!(parse_config("{\u{201C}t\u{201D}: {\u{201C}command\u{201D}: \u{201C}x\u{201D}}}").unwrap()[0].command, "x");
        assert_eq!(parse_config(r#"{"servers": {"b": {"type": "http", "url": "https://y"}}}"#).unwrap()[0].url, "https://y");
        assert_eq!(parse_config(r#"{"name": "c", "command": "uvx", "args": "mcp-server-time --local-timezone Asia/Shanghai", "env": {"N": 3}}"#).unwrap()[0].env["N"], "3");
        for bad in ["[]", "{}", "not json", r#"{"a": {"args": []}}"#, r#"{"a": {"type": "sse", "url": "https://z/sse"}}"#, r#"{"command": "x"}"#] {
            assert!(matches!(parse_config(bad), Err(CoreError::NotAnMcpConfig { .. })), "{bad}");
        }
    }

    #[test]
    fn tool_names_use_safe_letters_and_fit() {
        assert_eq!(tool_name("12306-mcp", "get-tickets"), "mcp_12306-mcp_get-tickets");
        assert_eq!(tool_name("my server", "a.b/c"), "mcp_my_server_a_b_c");
        assert_eq!(tool_name(&"s".repeat(50), &"t".repeat(50)).len(), MAX_TOOL_NAME);
    }

    #[test]
    fn a_command_line_quotes_every_word() {
        let s = entry("x", &json!({"command": "npx", "args": ["-y", "it's"]})).unwrap();
        assert_eq!(command_line(&s), "'npx' '-y' 'it'\\''s'");
    }

    #[test]
    fn results_keep_text_images_and_errors() {
        let ws = std::env::temp_dir().join(format!("solos-mcp-{}", uuid::Uuid::new_v4()));
        let png = base64::engine::general_purpose::STANDARD.encode([0x89, b'P', b'N', b'G']);
        let out = to_output(&json!({"content": [{"type": "text", "text": "7"}, {"type": "image", "data": png, "mimeType": "image/png"}], "isError": true}), &ws);
        assert!(out.is_error);
        assert!(out.text.starts_with("7\n[image: /solos/ws/.solos/mcp/"));
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].size, 4);
        let rel = out.images[0].path.strip_prefix("/solos/ws/").unwrap();
        assert_eq!(std::fs::read(ws.join(rel)).unwrap().len(), 4);
        assert_eq!(to_output(&json!({"content": [], "structuredContent": {"a": 1}}), &ws).text, "{\n  \"a\": 1\n}");
        assert_eq!(to_output(&json!({}), &ws).text, "(the tool returned nothing)");
    }

    #[test]
    fn a_server_schema_gets_the_label_unless_it_has_a_title_of_its_own() {
        let store_less = |schema: &str| ServerTool {
            mcp: Mcp { inner: Arc::new(Inner { servers: Default::default(), live: Default::default(), sandbox: Arc::new(crate::sandbox::host::HostSandbox::new("/tmp")), store: Store::open(&std::env::temp_dir().join(format!("solos-mcp-{}.db", uuid::Uuid::new_v4()))).unwrap() }) },
            server: "s".into(),
            tool: McpTool { name: "t".into(), description: "d".into(), input_schema: schema.into() },
        };
        let (s, added) = store_less(r#"{"$schema": "x", "type": "object", "properties": {"a": {"type": "number"}}, "required": ["a"]}"#).schema();
        assert!(added);
        assert_eq!(s["required"], json!(["title", "a"]));
        assert!(s.get("$schema").is_none());
        let (s, added) = store_less(r#"{"type": "object", "properties": {"title": {"type": "string"}}}"#).schema();
        assert!(!added);
        assert_eq!(s["properties"]["title"]["type"], "string");
        let (s, added) = store_less("").schema();
        assert!(added && s["properties"]["title"].is_object());
    }

    #[test]
    fn the_prompt_always_says_how_to_add_a_server() {
        assert!(prompt_section(&[]).contains("`mcp_add`") && prompt_section(&[]).contains("No servers are added."));
        let s = McpServer { tools: vec![McpTool { name: "t".into(), description: String::new(), input_schema: String::new() }], ..entry("a", &json!({"url": "https://a"})).unwrap() };
        let off = McpServer { name: "b".into(), enabled: false, ..s.clone() };
        assert!(prompt_section(&[s, off]).ends_with("Added servers: a (1 tool)"));
    }
}
