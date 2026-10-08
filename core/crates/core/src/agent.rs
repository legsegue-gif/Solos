//! The turn loop.
//!
//! ```text
//! loop:
//!   stream one assistant message
//!   commit it
//!   no tool calls → done
//!   run the calls, commit their results
//! ```
//!
//! Everything is committed before the next request goes out, so a turn that
//! is interrupted anywhere leaves a transcript that can be sent again as is.

use crate::providers::{ChatRequest, FinishReason, Provider, StreamEvent};
use crate::store::now_millis;
use crate::tools::{Registry, ToolContext, ToolOutput, TITLE_ARG};
use crate::sandbox::Sandbox;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use solos_api::{CoreError, EventKind, Message, Part, Role};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Where a turn reports what happens: the engine, which stores, sequences and
/// broadcasts it.
#[async_trait]
pub trait TurnHost: Send + Sync {
    async fn emit(&self, kind: EventKind);
    async fn commit(&self, message: Message) -> Result<(), CoreError>;
}

pub struct TurnConfig {
    /// The session the turn belongs to, for tools that keep things per
    /// session (background jobs).
    pub session_id: String,
    pub model: String,
    pub system: String,
    pub thinking: bool,
    /// Off is plain chat: the request carries no tools and the history's
    /// tool rounds are written out as text (`context::without_tool_rounds`).
    pub agent: bool,
    pub max_rounds: u32,
    /// How long the model may say nothing before the stream is given up on.
    pub idle_timeout: Duration,
    pub max_attempts: u32,
    /// The host folder behind `/solos/ws`, where attached images are read
    /// from when a request is built.
    pub workspace: Option<std::path::PathBuf>,
    /// The model's context window, when the endpoint said; see `context`.
    pub window: Option<u64>,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            model: String::new(),
            system: String::new(),
            thinking: true,
            agent: true,
            max_rounds: 50,
            idle_timeout: Duration::from_secs(120),
            max_attempts: 3,
            workspace: None,
            window: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum TurnEnd {
    Completed,
    Cancelled,
    Failed(CoreError),
}

/// Same call, same answer this many times in a row: the model is stuck.
const LOOP_LIMIT: usize = 4;

pub async fn run_turn(
    host: &dyn TurnHost,
    provider: &dyn Provider,
    tools: &Registry,
    sandbox: Arc<dyn Sandbox>,
    mut transcript: Vec<Message>,
    cfg: &TurnConfig,
    cancel: CancellationToken,
) -> TurnEnd {
    let mut recent: VecDeque<u64> = VecDeque::new();
    for _round in 0..cfg.max_rounds {
        if cancel.is_cancelled() {
            return TurnEnd::Cancelled;
        }
        let specs = if cfg.agent { tools.specs() } else { vec![] };
        let messages = match fit_window(cfg, &specs, &transcript) {
            Ok(m) => m,
            Err(e) => return TurnEnd::Failed(e),
        };
        let req = ChatRequest {
            model: cfg.model.clone(),
            system: cfg.system.clone(),
            messages,
            tools: specs,
            thinking: cfg.thinking,
            workspace: cfg.workspace.clone(),
        };
        let assistant = match stream_with_retries(host, provider, req, cfg, &cancel).await {
            Streamed::Done(m) => m,
            Streamed::Cancelled(partial) => {
                if let Some(m) = partial {
                    if let Err(e) = commit_with_seal(host, m, "not run: the turn was stopped").await {
                        return TurnEnd::Failed(e);
                    }
                }
                return TurnEnd::Cancelled;
            }
            Streamed::Failed(partial, e) => {
                if let Some(m) = partial {
                    if let Err(e) = commit_with_seal(host, m, "not run: the reply was cut off").await {
                        return TurnEnd::Failed(e);
                    }
                }
                return TurnEnd::Failed(e);
            }
        };

        let calls = tool_calls(&assistant);
        if let Err(e) = host.commit(assistant.clone()).await {
            return TurnEnd::Failed(e);
        }
        transcript.push(assistant);
        if calls.is_empty() {
            return TurnEnd::Completed;
        }

        let results = run_tools(host, tools, sandbox.clone(), &cfg.session_id, &calls, &cancel).await;
        for (call, (out, _)) in calls.iter().zip(&results) {
            recent.push_back(fingerprint(&call.name, &call.input_raw, &out.text));
            while recent.len() > LOOP_LIMIT {
                recent.pop_front();
            }
        }
        let result_msg = Message {
            id: new_id(),
            role: Role::Tool,
            created_at: now_millis(),
            parts: {
                // The results, then any images they carry: the providers
                // put those where each protocol lets a tool return a picture.
                let mut images = Vec::new();
                let mut parts: Vec<Part> = calls
                    .iter()
                    .zip(results)
                    .map(|(c, (out, ms))| {
                        images.extend(out.images);
                        Part::ToolResult { call_id: c.id.clone(), output: out.text, is_error: out.is_error, duration_ms: Some(ms) }
                    })
                    .collect();
                parts.extend(images.into_iter().map(|i| Part::Attachment {
                    name: i.path.rsplit('/').next().unwrap_or(&i.path).to_string(),
                    path: i.path,
                    mime: i.mime,
                    size: i.size,
                }));
                parts
            },
        };
        if let Err(e) = host.commit(result_msg.clone()).await {
            return TurnEnd::Failed(e);
        }
        transcript.push(result_msg);

        if cancel.is_cancelled() {
            return TurnEnd::Cancelled;
        }
        if recent.len() == LOOP_LIMIT && recent.iter().all(|f| *f == recent[0]) {
            return TurnEnd::Failed(CoreError::Loop { tool: calls[0].name.clone() });
        }
    }
    TurnEnd::Failed(CoreError::TooManyRounds { rounds: cfg.max_rounds })
}

/// The transcript to send, with old tool output cleared if the window is
/// filling; or, near the limit, a stop that asks the user (see `context`).
fn fit_window(cfg: &TurnConfig, specs: &[crate::tools::ToolSpec], transcript: &[Message]) -> Result<Vec<Message>, CoreError> {
    let mut messages = if cfg.agent { transcript.to_vec() } else { crate::context::without_tool_rounds(transcript) };
    let Some(window) = cfg.window else { return Ok(messages) };
    let policy = crate::context::Policy::for_window(window);
    let tools_json = serde_json::to_string(&specs.iter().map(|t| &t.schema).collect::<Vec<_>>()).unwrap_or_default();
    let mut used = crate::context::estimate(&cfg.system, &tools_json, &messages);
    if policy.clear_above > 0 && used > policy.clear_above {
        crate::context::clear_old_tool_output(&mut messages, used, policy.clear_to);
        used = crate::context::estimate(&cfg.system, &tools_json, &messages);
    }
    let (limit, can_summarize) = if policy.ask_above > 0 { (policy.ask_above, true) } else { (policy.full_above, false) };
    if used >= limit {
        return Err(CoreError::ContextNearlyFull { used, window, can_summarize });
    }
    Ok(messages)
}

enum Streamed {
    Done(Message),
    Cancelled(Option<Message>),
    Failed(Option<Message>, CoreError),
}

/// One assistant message, retried when nothing useful arrived: a network
/// failure before the first token, or an answer with no text and no calls.
async fn stream_with_retries(
    host: &dyn TurnHost,
    provider: &dyn Provider,
    req: ChatRequest,
    cfg: &TurnConfig,
    cancel: &CancellationToken,
) -> Streamed {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let (msg, outcome) = stream_once(host, provider, req.clone(), cfg, cancel).await;
        match outcome {
            Ok(()) if has_answer(&msg) => return Streamed::Done(msg),
            Ok(()) => {
                if attempt < 2 {
                    host.emit(EventKind::Retrying { attempt, reason: CoreError::EmptyResponse }).await;
                    continue;
                }
                return Streamed::Failed(None, CoreError::EmptyResponse);
            }
            Err(Stop::Cancelled) => return Streamed::Cancelled(non_empty(msg)),
            Err(Stop::Failed(e)) => {
                if !has_answer(&msg) && e.is_transient() && attempt < cfg.max_attempts {
                    host.emit(EventKind::Retrying { attempt, reason: e.clone() }).await;
                    let wait = Duration::from_secs(2u64.pow(attempt).min(20));
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => continue,
                        _ = cancel.cancelled() => return Streamed::Cancelled(None),
                    }
                }
                return Streamed::Failed(non_empty(msg), e);
            }
        }
    }
}

enum Stop {
    Cancelled,
    Failed(CoreError),
}

/// Stream one message, emitting deltas as they arrive. Returns what was
/// assembled, however the stream ended.
async fn stream_once(
    host: &dyn TurnHost,
    provider: &dyn Provider,
    req: ChatRequest,
    cfg: &TurnConfig,
    cancel: &CancellationToken,
) -> (Message, Result<(), Stop>) {
    let mut asm = Assembler::new(crate::providers::origin(provider.protocol(), &req.model));
    let stream = tokio::select! {
        s = provider.stream(req, cancel.clone()) => s,
        _ = cancel.cancelled() => return (asm.finish(), Err(Stop::Cancelled)),
    };
    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => return (asm.finish(), Err(Stop::Failed(e))),
    };
    host.emit(EventKind::MessageStarted { message_id: asm.id.clone() }).await;

    let result = loop {
        let next = tokio::select! {
            n = tokio::time::timeout(cfg.idle_timeout, stream.next()) => n,
            _ = cancel.cancelled() => break Err(Stop::Cancelled),
        };
        let item = match next {
            Err(_) => break Err(Stop::Failed(CoreError::Stalled { seconds: cfg.idle_timeout.as_secs() as u32 })),
            Ok(None) => break Ok(()),
            Ok(Some(Err(e))) => break Err(Stop::Failed(e)),
            Ok(Some(Ok(ev))) => ev,
        };
        for kind in asm.apply(item) {
            host.emit(kind).await;
        }
        if asm.finished {
            break Ok(());
        }
    };
    let (msg, ready) = asm.finish_with_events();
    for kind in ready {
        host.emit(kind).await;
    }
    (msg, result)
}

/// Builds one assistant message from stream events, keeping parts in the
/// order they arrived.
struct Assembler {
    id: String,
    /// Stamped on thinking, so signed thinking goes back only where it came from.
    origin: Option<String>,
    parts: Vec<Part>,
    /// Provider index → (part position, argument text so far).
    calls: BTreeMap<u32, (usize, String)>,
    finished: bool,
}

impl Assembler {
    fn new(origin: String) -> Self {
        Self { id: new_id(), origin: Some(origin), parts: Vec::new(), calls: BTreeMap::new(), finished: false }
    }

    fn apply(&mut self, ev: StreamEvent) -> Vec<EventKind> {
        let id = self.id.clone();
        match ev {
            StreamEvent::Text(t) => {
                match self.parts.last_mut() {
                    Some(Part::Text { text }) => text.push_str(&t),
                    _ => self.parts.push(Part::Text { text: t.clone() }),
                }
                vec![EventKind::TextDelta { message_id: id, delta: t }]
            }
            StreamEvent::Thinking(t) => {
                match self.parts.last_mut() {
                    Some(Part::Thinking { text, signature: None, redacted: None, .. }) => text.push_str(&t),
                    _ => self.parts.push(Part::Thinking { text: t.clone(), signature: None, redacted: None, origin: self.origin.clone() }),
                }
                vec![EventKind::ThinkingDelta { message_id: id, delta: t }]
            }
            StreamEvent::ThinkingSignature(sig) => {
                match self.parts.last_mut() {
                    Some(Part::Thinking { signature, .. }) => *signature = Some(sig),
                    // A signature with no text before it (thinking not shown).
                    _ => self.parts.push(Part::Thinking { text: String::new(), signature: Some(sig), redacted: None, origin: self.origin.clone() }),
                }
                vec![]
            }
            StreamEvent::RedactedThinking(data) => {
                self.parts.push(Part::Thinking { text: String::new(), signature: None, redacted: Some(data), origin: self.origin.clone() });
                vec![]
            }
            StreamEvent::ToolCallSignature { index, signature: sig } => {
                if let Some((pos, _)) = self.calls.get(&index) {
                    if let Some(Part::ToolCall { signature, .. }) = self.parts.get_mut(*pos) {
                        *signature = Some(sig);
                    }
                }
                vec![]
            }
            StreamEvent::ToolCallStart { index, id: call_id, name } => {
                self.parts.push(Part::ToolCall { id: call_id.clone(), name: name.clone(), input_json: String::new(), title: None, signature: None });
                self.calls.insert(index, (self.parts.len() - 1, String::new()));
                vec![EventKind::ToolCallStarted { message_id: id, call_id, name }]
            }
            StreamEvent::ToolCallArgs { index, fragment } => {
                if let Some((_, buf)) = self.calls.get_mut(&index) {
                    buf.push_str(&fragment);
                }
                vec![]
            }
            StreamEvent::Finish(reason) => {
                self.finished = matches!(reason, FinishReason::Stop | FinishReason::ToolCalls | FinishReason::Length | FinishReason::Other);
                vec![]
            }
        }
    }

    /// Settle every tool call's arguments — the one place that happens,
    /// whichever way the stream ended.
    fn finish_with_events(mut self) -> (Message, Vec<EventKind>) {
        let mut events = Vec::new();
        for (pos, raw) in std::mem::take(&mut self.calls).into_values() {
            if let Some(Part::ToolCall { id, input_json, title, .. }) = self.parts.get_mut(pos) {
                *input_json = if raw.trim().is_empty() { "{}".to_string() } else { raw };
                *title = serde_json::from_str::<Value>(input_json)
                    .ok()
                    .and_then(|v| v.get(TITLE_ARG).and_then(Value::as_str).map(str::to_string));
                events.push(EventKind::ToolCallReady {
                    message_id: self.id.clone(),
                    call_id: id.clone(),
                    input_json: input_json.clone(),
                    title: title.clone(),
                });
            }
        }
        (Message { id: self.id, role: Role::Assistant, parts: self.parts, created_at: now_millis() }, events)
    }

    fn finish(self) -> Message {
        self.finish_with_events().0
    }
}

struct Call {
    id: String,
    name: String,
    input_raw: String,
}

fn tool_calls(m: &Message) -> Vec<Call> {
    m.parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall { id, name, input_json, .. } => {
                Some(Call { id: id.clone(), name: name.clone(), input_raw: input_json.clone() })
            }
            _ => None,
        })
        .collect()
}

/// Run a message's calls. Byte-identical calls (same tool, same arguments)
/// run once and share the result, with a note saying so: models do ask for
/// the same thing twice in one message, and running a download or a write
/// twice has no meaning. Anything that differs, even the title, runs.
async fn run_tools(
    host: &dyn TurnHost,
    tools: &Registry,
    sandbox: Arc<dyn Sandbox>,
    session_id: &str,
    calls: &[Call],
    cancel: &CancellationToken,
) -> Vec<(ToolOutput, u64)> {
    let mut first: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
    let mut unique: Vec<Call> = Vec::new();
    let mut source: Vec<usize> = Vec::with_capacity(calls.len());
    for c in calls {
        let idx = *first.entry((c.name.as_str(), c.input_raw.as_str())).or_insert_with(|| {
            unique.push(Call { id: c.id.clone(), name: c.name.clone(), input_raw: c.input_raw.clone() });
            unique.len() - 1
        });
        source.push(idx);
    }
    let results = run_unique(host, tools, sandbox, session_id, &unique, cancel).await;
    let mut seen = vec![false; unique.len()];
    source
        .into_iter()
        .map(|i| {
            let (out, ms) = results[i].clone();
            if std::mem::replace(&mut seen[i], true) {
                let note = "\n[note] This call was identical to another one in the same message, so it ran once and this is that result.";
                (ToolOutput { text: format!("{}{note}", out.text), ..out }, ms)
            } else {
                (out, ms)
            }
        })
        .collect()
}

async fn run_unique(
    host: &dyn TurnHost,
    tools: &Registry,
    sandbox: Arc<dyn Sandbox>,
    session_id: &str,
    calls: &[Call],
    cancel: &CancellationToken,
) -> Vec<(ToolOutput, u64)> {
    let all_parallel = calls.iter().all(|c| tools.get(&c.name).map(|t| t.spec().parallel).unwrap_or(true));
    let run_one = |c: &Call| {
        let sandbox = sandbox.clone();
        let cancel = cancel.clone();
        let tool = tools.get(&c.name);
        let name = c.name.clone();
        let raw = c.input_raw.clone();
        let call_id = c.id.clone();
        let session_id = session_id.to_string();
        async move {
            host.emit(EventKind::ToolRunning { call_id: call_id.clone() }).await;
            let started = Instant::now();
            let out = match (tool, serde_json::from_str::<Value>(&raw)) {
                (None, _) => ToolOutput::error(format!("There is no tool named `{name}`.")),
                (_, Err(e)) => ToolOutput::error(format!(
                    "The arguments were not valid JSON ({e}), so the tool did not run. They arrived as: {}",
                    raw.chars().take(2000).collect::<String>()
                )),
                (Some(tool), Ok(input)) => {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    let ctx = ToolContext {
                        session_id,
                        sandbox,
                        cancel,
                        on_output: Arc::new(move |chunk: &str| {
                            let _ = tx.send(chunk.to_string());
                        }),
                    };
                    let call = tool.call(&ctx, &input);
                    tokio::pin!(call);
                    loop {
                        tokio::select! {
                            out = &mut call => {
                                while let Ok(chunk) = rx.try_recv() {
                                    host.emit(EventKind::ToolOutputDelta { call_id: call_id.clone(), chunk }).await;
                                }
                                break out;
                            }
                            Some(chunk) = rx.recv() => {
                                host.emit(EventKind::ToolOutputDelta { call_id: call_id.clone(), chunk }).await;
                            }
                        }
                    }
                }
            };
            (out, started.elapsed().as_millis() as u64)
        }
    };
    if all_parallel {
        futures::future::join_all(calls.iter().map(run_one)).await
    } else {
        let mut out = Vec::new();
        for c in calls {
            out.push(run_one(c).await);
        }
        out
    }
}

/// Commit a partial assistant message; if it holds tool calls that will never
/// run, commit results saying so, so the transcript stays sendable.
async fn commit_with_seal(host: &dyn TurnHost, m: Message, why: &str) -> Result<(), CoreError> {
    let calls = tool_calls(&m);
    host.commit(m).await?;
    if calls.is_empty() {
        return Ok(());
    }
    host.commit(seal(&calls.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), why)).await
}

pub fn seal(call_ids: &[String], why: &str) -> Message {
    Message {
        id: new_id(),
        role: Role::Tool,
        created_at: now_millis(),
        parts: call_ids
            .iter()
            .map(|id| Part::ToolResult { call_id: id.clone(), output: why.to_string(), is_error: true, duration_ms: None })
            .collect(),
    }
}

fn has_answer(m: &Message) -> bool {
    m.parts.iter().any(|p| match p {
        Part::Text { text } => !text.trim().is_empty(),
        Part::ToolCall { .. } => true,
        _ => false,
    })
}

fn non_empty(m: Message) -> Option<Message> {
    (!m.parts.is_empty()).then_some(m)
}

fn fingerprint(name: &str, input: &str, output: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (name, input, output).hash(&mut h);
    h.finish()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}
