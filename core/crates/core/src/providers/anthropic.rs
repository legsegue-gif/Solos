//! Anthropic Messages API, over raw HTTP (there is no official Rust SDK).
//!
//! Thinking comes back as `thinking` blocks with a `signature` (or as
//! `redacted_thinking`); both are sent back unchanged, and only to the model
//! that produced them. Cache breakpoints sit on the last tool, the system
//! prompt and the last message block, so each request reuses the one before.

use super::http::{get_json, post_sse, ChunkReader};
use super::shaping::anthropic_thinking;
use super::{attachments, origin, Capture, ChatRequest, EventStream, FinishReason, Provider, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use solos_api::{CoreError, Message, ModelInfo, Part, Role};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub const PROTOCOL: &str = "anthropic";
const OFFICIAL_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// Output ceiling per request. Every current model takes at least this; a
/// reply that reaches it ends with `Length`, which the turn reports.
const MAX_TOKENS: u32 = 32_000;

pub struct Anthropic {
    client: reqwest::Client,
    base_url: String,
    key: String,
    capture: Capture,
}

impl Anthropic {
    pub fn new(base_url: &str, key: String, capture: Capture) -> Self {
        Self { client: super::http::client(), base_url: normalise_base(base_url), key, capture }
    }

    fn is_official(&self) -> bool {
        self.base_url.contains("api.anthropic.com")
    }

    fn request(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let builder = builder.header("x-api-key", &self.key).header("anthropic-version", API_VERSION);
        // Relays that speak this protocol often authenticate with a bearer
        // token instead; the official API reads that header as OAuth.
        if self.is_official() {
            builder
        } else {
            builder.bearer_auth(&self.key)
        }
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, CoreError> {
        let v = get_json(self.request(self.client.get(format!("{}/v1/models?limit=1000", self.base_url)))).await?;
        Ok(v.get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some(ModelInfo {
                    id: m.get("id")?.as_str()?.to_string(),
                    display_name: m.get("display_name").and_then(Value::as_str).map(str::to_string),
                    context_window: m.get("max_input_tokens").and_then(Value::as_u64),
                    max_output: m.get("max_tokens").and_then(Value::as_u64),
                })
            })
            .collect())
    }

    pub fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
            "messages": convert_messages(&req.messages, &origin(PROTOCOL, &req.model), req.workspace.as_deref()),
        });
        if !req.tools.is_empty() {
            let eager = self.is_official();
            let mut tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut tool = json!({"name": t.name, "description": t.description, "input_schema": t.schema});
                    // Large arguments stream as written; relays may reject the field.
                    if eager {
                        tool["eager_input_streaming"] = json!(true);
                    }
                    tool
                })
                .collect();
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            body["tools"] = Value::Array(tools);
        }
        if let Some(thinking) = anthropic_thinking(&req.model, req.thinking) {
            body["thinking"] = thinking;
        }
        body
    }
}

fn normalise_base(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let b = b.strip_suffix("/v1").unwrap_or(b);
    if b.is_empty() {
        OFFICIAL_BASE.to_string()
    } else {
        b.to_string()
    }
}

/// The transcript as Messages API turns. Tool results travel in a user turn;
/// turns of one role in a row are merged, since roles must alternate.
pub fn convert_messages(messages: &[Message], origin: &str, workspace: Option<&Path>) -> Vec<Value> {
    let inline = attachments::inline_set(messages);
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in messages {
        let (role, blocks) = match m.role {
            Role::User => ("user", user_blocks(m, &inline, workspace)),
            Role::Assistant => ("assistant", assistant_blocks(m, origin)),
            // Results first (the protocol wants them leading the turn), then
            // the images they returned.
            Role::Tool => (
                "user",
                m.parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolResult { call_id, output, is_error, .. } => Some(json!({
                            "type": "tool_result", "tool_use_id": call_id, "content": output, "is_error": is_error
                        })),
                        _ => None,
                    })
                    .chain(m.parts.iter().filter(|p| p.is_image()).filter_map(|p| match p {
                        Part::Attachment { path, mime, .. } => {
                            let data = inline.contains(path).then(|| workspace.and_then(|ws| attachments::image_base64(ws, path))).flatten()?;
                            Some(json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}))
                        }
                        _ => None,
                    }))
                    .collect(),
            ),
        };
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((r, b)) if *r == role => b.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }
    if let Some(block) = turns.last_mut().and_then(|(_, b)| b.last_mut()) {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    turns.into_iter().map(|(role, content)| json!({"role": role, "content": content})).collect()
}

fn user_blocks(m: &Message, inline: &std::collections::HashSet<String>, workspace: Option<&Path>) -> Vec<Value> {
    let mut blocks = Vec::new();
    for p in &m.parts {
        match p {
            Part::Text { text } if !text.is_empty() => blocks.push(json!({"type": "text", "text": text})),
            Part::Attachment { path, mime, .. } if p.is_image() => {
                let data = inline.contains(path).then(|| workspace.and_then(|ws| attachments::image_base64(ws, path))).flatten();
                match data {
                    Some(data) => {
                        blocks.push(json!({"type": "text", "text": format!("[attached image: {path}]")}));
                        blocks.push(json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}));
                    }
                    None => blocks.push(json!({"type": "text", "text": attachments::omitted_note(p)})),
                }
            }
            _ => {}
        }
    }
    if let Some(files) = attachments::files_block(&m.parts) {
        blocks.push(json!({"type": "text", "text": files}));
    }
    blocks
}

fn assistant_blocks(m: &Message, origin: &str) -> Vec<Value> {
    let mut blocks = Vec::new();
    for p in &m.parts {
        match p {
            // Signed thinking goes back unchanged to the model that wrote
            // it; any other model would reject or ignore it.
            Part::Thinking { text, signature, redacted, origin: from } if from.as_deref() == Some(origin) => {
                if let Some(data) = redacted {
                    blocks.push(json!({"type": "redacted_thinking", "data": data}));
                } else if let Some(sig) = signature {
                    blocks.push(json!({"type": "thinking", "thinking": text, "signature": sig}));
                }
            }
            Part::Text { text } if !text.is_empty() => blocks.push(json!({"type": "text", "text": text})),
            Part::ToolCall { id, name, input_json, .. } => {
                // The wire needs an object. Arguments that did not parse were
                // already reported to the model in the call's result.
                let input = serde_json::from_str::<Value>(input_json).ok().filter(Value::is_object).unwrap_or(json!({}));
                blocks.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
            }
            _ => {}
        }
    }
    blocks
}

/// Reads the Messages API event stream.
#[derive(Default)]
pub struct EventReader {
    /// Block index → kind, for deltas that only name the index.
    blocks: HashMap<u64, &'static str>,
}

impl ChunkReader for EventReader {
    fn read(&mut self, data: &str) -> Option<Result<Vec<StreamEvent>, CoreError>> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                return Some(Err(CoreError::Protocol { detail: format!("{e}: {}", data.chars().take(200).collect::<String>()) }))
            }
        };
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
        let out = match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_stop" => return None,
            "error" => return Some(Err(stream_error(&v["error"]))),
            "content_block_start" => {
                let block = &v["content_block"];
                match block.get("type").and_then(Value::as_str).unwrap_or("") {
                    "tool_use" => {
                        self.blocks.insert(index, "tool_use");
                        vec![StreamEvent::ToolCallStart {
                            index: index as u32,
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                        }]
                    }
                    "redacted_thinking" => vec![StreamEvent::RedactedThinking(block["data"].as_str().unwrap_or("").to_string())],
                    "thinking" => {
                        self.blocks.insert(index, "thinking");
                        vec![]
                    }
                    _ => vec![],
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => vec![StreamEvent::Text(s("text"))],
                    "thinking_delta" => vec![StreamEvent::Thinking(s("thinking"))],
                    "signature_delta" => vec![StreamEvent::ThinkingSignature(s("signature"))],
                    "input_json_delta" => vec![StreamEvent::ToolCallArgs { index: index as u32, fragment: s("partial_json") }],
                    _ => vec![],
                }
                .into_iter()
                .filter(|e| !matches!(e, StreamEvent::Text(t) | StreamEvent::Thinking(t) if t.is_empty()))
                .collect()
            }
            "message_delta" => match v["delta"].get("stop_reason").and_then(Value::as_str) {
                Some("end_turn" | "stop_sequence") => vec![StreamEvent::Finish(FinishReason::Stop)],
                Some("tool_use") => vec![StreamEvent::Finish(FinishReason::ToolCalls)],
                Some("max_tokens") => vec![StreamEvent::Finish(FinishReason::Length)],
                Some(_) => vec![StreamEvent::Finish(FinishReason::Other)],
                None => vec![],
            },
            _ => vec![],
        };
        Some(Ok(out))
    }
}

/// An error sent inside a 200 stream, as the kind a client can act on:
/// `overloaded_error` is the API's 529 and worth retrying.
fn stream_error(e: &Value) -> CoreError {
    let detail = e.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    match e.get("type").and_then(Value::as_str) {
        Some("overloaded_error") => CoreError::Http { status: 529, detail },
        Some("rate_limit_error") => CoreError::Http { status: 429, detail },
        Some("api_error") => CoreError::Http { status: 500, detail },
        _ => CoreError::Protocol { detail },
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn protocol(&self) -> &'static str {
        PROTOCOL
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream, CoreError> {
        let body = self.build_body(&req);
        let request = self.request(self.client.post(format!("{}/v1/messages", self.base_url)));
        post_sse(request, &body, &self.capture, cancel, EventReader::default()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolSpec;

    fn read_all(lines: &[&str]) -> Vec<StreamEvent> {
        let mut r = EventReader::default();
        lines.iter().filter_map(|l| r.read(l)).flat_map(|x| x.unwrap()).collect()
    }

    #[test]
    fn a_stream_with_signed_thinking_text_and_a_tool_call() {
        let evs = read_all(&[
            r#"{"type":"message_start","message":{"id":"m"}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Let me look."}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"shell","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
        ]);
        assert_eq!(
            evs,
            vec![
                StreamEvent::Thinking("plan".into()),
                StreamEvent::ThinkingSignature("SIG".into()),
                StreamEvent::Text("Let me look.".into()),
                StreamEvent::ToolCallStart { index: 2, id: "toolu_1".into(), name: "shell".into() },
                StreamEvent::ToolCallArgs { index: 2, fragment: "{\"command\":".into() },
                StreamEvent::ToolCallArgs { index: 2, fragment: "\"ls\"}".into() },
                StreamEvent::Finish(FinishReason::ToolCalls),
            ]
        );
        assert!(EventReader::default().read(r#"{"type":"message_stop"}"#).is_none());
    }

    #[test]
    fn an_overload_inside_the_stream_is_retryable() {
        let e = EventReader::default().read(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#).unwrap().unwrap_err();
        assert!(e.is_transient(), "{e:?}");
    }

    fn assistant(parts: Vec<Part>) -> Message {
        Message { id: "a".into(), role: Role::Assistant, parts, created_at: 0 }
    }

    #[test]
    fn thinking_goes_back_signed_and_only_to_its_own_model() {
        let msgs = vec![
            Message { id: "u".into(), role: Role::User, parts: vec![Part::Text { text: "go".into() }], created_at: 0 },
            assistant(vec![
                Part::Thinking { text: "plan".into(), signature: Some("SIG".into()), redacted: None, origin: Some("anthropic:claude-opus-5".into()) },
                Part::Thinking { text: String::new(), signature: None, redacted: Some("ENC".into()), origin: Some("anthropic:claude-opus-5".into()) },
                Part::ToolCall { id: "t1".into(), name: "shell".into(), input_json: r#"{"command":"ls"}"#.into(), title: None, signature: None },
            ]),
            Message { id: "r".into(), role: Role::Tool, parts: vec![Part::ToolResult { call_id: "t1".into(), output: "x".into(), is_error: false, duration_ms: None }], created_at: 0 },
            Message { id: "u2".into(), role: Role::User, parts: vec![Part::Text { text: "and?".into() }], created_at: 0 },
        ];
        let same = convert_messages(&msgs, "anthropic:claude-opus-5", None);
        assert_eq!(same[1]["content"][0], json!({"type": "thinking", "thinking": "plan", "signature": "SIG"}));
        assert_eq!(same[1]["content"][1], json!({"type": "redacted_thinking", "data": "ENC"}));
        assert_eq!(same[1]["content"][2]["input"], json!({"command": "ls"}));
        // The tool result and the next user message share one user turn.
        assert_eq!(same.len(), 3);
        assert_eq!(same[2]["content"][0]["type"], "tool_result");
        assert_eq!(same[2]["content"][1]["text"], "and?");
        assert_eq!(same[2]["content"][1]["cache_control"], json!({"type": "ephemeral"}), "the tail is cached");
        let other = convert_messages(&msgs, "anthropic:claude-sonnet-5", None);
        assert_eq!(other[1]["content"][0]["type"], "tool_use", "no thinking for another model");
    }

    #[test]
    fn the_body_caches_the_prefix_and_shapes_thinking_per_model() {
        let p = Anthropic::new("", "k".into(), Capture::default());
        let req = |model: &str, thinking: bool| ChatRequest {
            model: model.into(),
            system: "sys".into(),
            messages: vec![Message { id: "u".into(), role: Role::User, parts: vec![Part::Text { text: "hi".into() }], created_at: 0 }],
            tools: vec![ToolSpec { name: "shell".into(), description: "d".into(), schema: json!({"type": "object"}), parallel: false }],
            thinking,
            workspace: None,
        };
        let b = p.build_body(&req("claude-opus-5", true));
        assert_eq!(b["system"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(b["tools"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(p.build_body(&req("claude-opus-5", false))["thinking"], json!({"type": "disabled"}));
        let relay = Anthropic::new("https://relay.example/v1/", "k".into(), Capture::default());
        assert_eq!(relay.base_url, "https://relay.example");
        assert!(relay.build_body(&req("claude-opus-5", true))["tools"][0].get("eager_input_streaming").is_none());
    }
}
