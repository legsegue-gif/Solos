//! The Solos client API: every type that crosses from the core to a client.
//!
//! Clients see sessions through two things only: a [`Snapshot`], the full
//! state of a session, and a stream of [`Event`]s that change it. Events carry
//! a per-session `seq`; a client that sees a gap asks for a snapshot and
//! starts over from it. That is the whole recovery story.
//!
//! Nothing here is user-facing prose. Errors are kinds with parameters, and
//! the client decides how to say them in the user's language.

uniffi::setup_scaffolding!();

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
pub type Millis = i64;

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The wire protocol an endpoint speaks.
#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// OpenAI Chat Completions, which most relays and local servers speak.
    OpenAi,
    Anthropic,
    Gemini,
}

/// A place models are served from, with its own key.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    /// Empty means the protocol's official address.
    pub base_url: String,
    /// Where the client keeps the key; resolved through the client's secret
    /// resolver when a request needs it, so a key changed in settings applies
    /// to the next request.
    pub secret_ref: String,
}

/// A model on a particular endpoint.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelChoice {
    pub endpoint_id: String,
    pub model: String,
}

#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    pub endpoints: Vec<Endpoint>,
    pub default_model: Option<ModelChoice>,
    pub thinking: bool,
}

/// Which package manager a mirror serves.
#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MirrorKind {
    Alpine,
    Pip,
    Npm,
}

/// A place the Linux system gets packages from.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PackageMirror {
    pub kind: MirrorKind,
    /// Stable within its kind: `official`, `tuna`, …
    pub id: String,
    /// The reference app's name for it (a proper name, not translated).
    pub name: String,
    pub url: String,
    /// Where it is: `Global`, `China`, `Europe` or `Asia`.
    pub region: String,
}

/// How long a source took to answer; `None` when it failed or took too long.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MirrorSpeed {
    pub kind: MirrorKind,
    pub id: String,
    pub millis: Option<u64>,
}

/// What an endpoint said about one of its models. Anything it did not say is
/// `None`; nothing is guessed.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
}

/// A model's context window: what its endpoint reported, and what the user
/// set, kept apart so fetching the list again never loses the user's value.
/// The user's wins when both are there.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelWindow {
    pub reported: Option<u64>,
    pub custom: Option<u64>,
}

/// Whether the sandbox runs, and how much room it and the conversations
/// take on the device.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SandboxStatus {
    /// Why it could not start; `None` while it runs.
    pub error: Option<String>,
    /// The guest's own system, without the workspace; `None` for a sandbox
    /// that has none of its own (the host shell).
    pub system_bytes: Option<u64>,
    pub workspace_bytes: u64,
    /// Conversations, settings and their journal.
    pub database_bytes: u64,
    /// Page text and screenshots the browser tool saved for the model:
    /// made as it works, safe to throw away.
    pub temporary_bytes: u64,
}

// ---------------------------------------------------------------------------
// Sessions and messages
// ---------------------------------------------------------------------------

#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<ModelChoice>,
    /// Thinking for this session; `None` follows the settings.
    pub thinking: Option<bool>,
    pub created_at: Millis,
    pub updated_at: Millis,
    /// The start of the last reply, for the session list.
    pub preview: Option<String>,
    /// When it was pinned to the top of the list; `None` when it is not.
    pub pinned_at: Option<Millis>,
}

#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// Results of the tool calls in the assistant message before it.
    Tool,
}

#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        /// The provider's proof that the thinking is its own (Anthropic's
        /// `signature`), sent back with it unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Thinking the provider returned encrypted; `text` is empty.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<String>,
        /// `protocol:model` that produced it: signed thinking is replayed
        /// only to that model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        /// The arguments as the model sent them, as JSON text.
        input_json: String,
        /// The short label the model gave the call, for its row in the UI.
        title: Option<String>,
        /// Gemini's `thoughtSignature` on the call, which it needs back.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
        duration_ms: Option<u64>,
    },
    /// A file the user attached, copied into the workspace.
    Attachment {
        /// Guest path, under `/solos/ws/attachments/`.
        path: String,
        /// The name the user's file had.
        name: String,
        mime: String,
        size: u64,
    },
}

/// A file the user picked, as the app hands it over: the core copies it into
/// the workspace, so the app may delete its own copy once `send` returns.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AttachmentSource {
    pub name: String,
    pub host_path: String,
    pub mime: String,
}

#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub parts: Vec<Part>,
    pub created_at: Millis,
}

/// A summary standing in for the start of a conversation when the model
/// is asked. The messages stay in the store and on screen; undoing the
/// summary sends them to the model again.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub id: String,
    /// The last message the summary covers.
    pub through_message_id: String,
    pub summary: String,
    pub created_at: Millis,
    pub undone: bool,
}

/// A turn that is running right now.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RunningTurn {
    pub turn_id: String,
    /// The assistant message being streamed, as far as it has got.
    pub streaming: Option<Message>,
    /// Tool calls that are executing.
    pub running_tools: Vec<String>,
}

/// Everything a client needs to draw a session.
#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub session: SessionInfo,
    pub messages: Vec<Message>,
    pub turn: Option<RunningTurn>,
    /// Messages sent while a turn was running, waiting their turn.
    pub queued: Vec<String>,
    pub compactions: Vec<Compaction>,
    /// Why the last turn failed, if it did, so a chat opened later can
    /// offer the same way on as the one that watched it fail.
    pub last_error: Option<CoreError>,
    /// The `seq` of the last event already reflected here; the next event for
    /// this session will carry `seq + 1`.
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(uniffi::Record, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub session_id: String,
    pub seq: u64,
    pub kind: EventKind,
}

#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    TurnStarted { turn_id: String },
    /// A message the core has written to the store. For an assistant message
    /// this replaces whatever was streamed for it.
    MessageCommitted { message: Message },
    /// An assistant message has begun streaming.
    MessageStarted { message_id: String },
    TextDelta { message_id: String, delta: String },
    ThinkingDelta { message_id: String, delta: String },
    ToolCallStarted { message_id: String, call_id: String, name: String },
    ToolCallReady { message_id: String, call_id: String, input_json: String, title: Option<String> },
    ToolRunning { call_id: String },
    ToolOutputDelta { call_id: String, chunk: String },
    Queued { queued: Vec<String> },
    Retrying { attempt: u32, reason: CoreError },
    TurnFinished { turn_id: String, outcome: TurnOutcome },
    SessionUpdated { info: SessionInfo },
    /// Messages after `after` were deleted (all of them when `None`): the
    /// session was cleared, or a message retried or edited.
    Truncated { after: Option<String> },
    CompactionsChanged { compactions: Vec<Compaction> },
}

#[derive(uniffi::Enum, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed { error: CoreError },
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every way a call or a turn can fail, as something a client can act on and
/// put into words. `detail` is diagnostic text (a server's own message, an OS
/// error) shown as-is, never the whole explanation.
#[derive(uniffi::Error, thiserror::Error, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreError {
    #[error("no model is configured")]
    NoModel,
    #[error("endpoint {endpoint_id} not found")]
    UnknownEndpoint { endpoint_id: String },
    #[error("no key for endpoint {endpoint_name}")]
    MissingKey { endpoint_name: String },
    #[error("network: {detail}")]
    Network { detail: String },
    #[error("HTTP {status}: {detail}")]
    Http { status: u16, detail: String },
    #[error("authentication rejected: {detail}")]
    Unauthorized { detail: String },
    #[error("the request is longer than the model accepts: {detail}")]
    ContextTooLong { detail: String },
    /// The conversation is close to the model's window; the turn stopped
    /// so the user can choose to summarise it (when `can_summarize`) or
    /// start a new chat.
    #[error("the conversation uses about {used} of {window} tokens")]
    ContextNearlyFull { used: u64, window: u64, can_summarize: bool },
    #[error("nothing is old enough to summarise")]
    NothingToCompact,
    #[error("the model sent nothing for {seconds}s")]
    Stalled { seconds: u32 },
    #[error("the model answered with nothing")]
    EmptyResponse,
    #[error("stopped after {rounds} rounds of tool calls")]
    TooManyRounds { rounds: u32 },
    #[error("the same tool call kept returning the same result: {tool}")]
    Loop { tool: String },
    #[error("a turn is already running")]
    Busy,
    #[error("there is no message to answer again")]
    NothingToRetry,
    #[error("the last turn finished; there is nothing to carry on")]
    NothingToResume,
    #[error("session {session_id} not found")]
    SessionNotFound { session_id: String },
    #[error("terminal {terminal_id} is not open")]
    NoSuchTerminal { terminal_id: String },
    #[error("sandbox: {detail}")]
    Sandbox { detail: String },
    #[error("storage: {detail}")]
    Storage { detail: String },
    #[error("malformed response: {detail}")]
    Protocol { detail: String },
    #[error("internal: {detail}")]
    Internal { detail: String },
}

impl CoreError {
    /// Worth trying the same request again without the user changing anything.
    pub fn is_transient(&self) -> bool {
        match self {
            CoreError::Network { .. } | CoreError::Stalled { .. } => true,
            CoreError::Http { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

impl Part {
    pub fn is_image(&self) -> bool {
        matches!(self, Part::Attachment { mime, .. } if mime.starts_with("image/"))
    }
}

impl Message {
    /// The text parts, joined; what a person would call "what it said".
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}
