//! Talking to models.
//!
//! One adapter per wire protocol turns a provider-neutral [`ChatRequest`] into
//! that protocol's request, and its streamed response into [`StreamEvent`]s.
//! Nothing above this module knows which protocol is in use.

pub mod anthropic;
pub mod attachments;
pub mod gemini;
pub mod http;
pub mod model_order;
pub mod openai;
pub mod shaping;
pub mod sse;

use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures::stream::BoxStream;
use solos_api::{CoreError, Message};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub thinking: bool,
    /// Where attached images are read from (the host folder behind
    /// `/solos/ws`); without it they are described, not sent.
    pub workspace: Option<PathBuf>,
}

/// One step of a streamed reply. Tool calls arrive as a start (with the
/// provider's index for the call) followed by argument fragments; the agent
/// assembles them.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    /// Closes the current thinking block with the provider's signature.
    ThinkingSignature(String),
    /// A thinking block the provider sent encrypted.
    RedactedThinking(String),
    ToolCallStart { index: u32, id: String, name: String },
    ToolCallArgs { index: u32, fragment: String },
    /// An opaque signature the provider attached to a call.
    ToolCallSignature { index: u32, signature: String },
    Finish(FinishReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

pub type EventStream = BoxStream<'static, Result<StreamEvent, CoreError>>;

#[async_trait]
pub trait Provider: Send + Sync {
    /// The wire protocol, for tagging what only this protocol can read back.
    fn protocol(&self) -> &'static str {
        "openai"
    }
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream, CoreError>;
}

/// The `origin` tag of signed thinking: the protocol and the model.
pub fn origin(protocol: &str, model: &str) -> String {
    format!("{protocol}:{model}")
}

/// Where to write request bodies and raw response streams, when capture is
/// on. Keys never appear in either: they travel in headers, which are not
/// written.
#[derive(Debug, Clone, Default)]
pub struct Capture {
    pub dir: Option<PathBuf>,
}

impl Capture {
    pub fn write(&self, name: &str, bytes: &[u8]) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(name), bytes);
        }
    }

    pub fn append(&self, name: &str, bytes: &[u8]) {
        use std::io::Write;
        if let Some(dir) = &self.dir {
            let _ = std::fs::create_dir_all(dir);
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(name)) {
                let _ = f.write_all(bytes);
            }
        }
    }
}

pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Turn an HTTP failure into the error kind a client can act on.
pub fn http_error(status: u16, body: &str) -> CoreError {
    let detail = body.chars().take(600).collect::<String>();
    let lower = body.to_ascii_lowercase();
    if status == 401 || status == 403 {
        return CoreError::Unauthorized { detail };
    }
    let too_long = lower.contains("context_length_exceeded")
        || lower.contains("maximum context length")
        || lower.contains("prompt is too long")
        || lower.contains("input token count exceeds");
    if too_long {
        return CoreError::ContextTooLong { detail };
    }
    CoreError::Http { status, detail }
}
