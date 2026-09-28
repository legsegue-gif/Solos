//! OpenAI Chat Completions — the protocol most relays and local servers speak.

use super::{Capture, ChatRequest, EventStream, FinishReason, Provider, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use solos_api::{CoreError, Message, ModelInfo, Part, Role};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const OFFICIAL_BASE: &str = "https://api.openai.com/v1";

pub struct OpenAi {
    client: reqwest::Client,
    base_url: String,
    key: String,
    capture: Capture,
}

impl OpenAi {
    pub fn new(base_url: &str, key: String, capture: Capture) -> Self {
        Self { client: super::http::client(), base_url: normalise_base(base_url), key, capture }
    }

    /// The models the endpoint says it serves.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, CoreError> {
        let v = super::http::get_json(self.client.get(format!("{}/models", self.base_url)).bearer_auth(&self.key)).await?;
        let mut out: Vec<ModelInfo> = Vec::new();
        for m in v.get("data").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = m.get("id").and_then(Value::as_str) else { continue };
            if out.iter().any(|o| o.id == id) {
                continue;
            }
            let num = |keys: &[&str]| keys.iter().find_map(|k| m.get(*k).and_then(Value::as_u64));
            out.push(ModelInfo {
                id: id.to_string(),
                display_name: m.get("name").and_then(Value::as_str).map(str::to_string),
                // Some relays report it under `info.meta` (Open WebUI style).
                context_window: num(&["context_length", "context_window", "max_context_length"])
                    .or_else(|| m.pointer("/info/meta/max_context_length").and_then(Value::as_u64)),
                max_output: num(&["max_output_tokens", "max_completion_tokens"]),
            });
        }
        Ok(out)
    }

    fn is_official(&self) -> bool {
        self.base_url.contains("api.openai.com")
    }

    pub fn build_body(&self, req: &ChatRequest) -> Value {
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        messages.extend(convert_messages(&req.messages, !self.is_official(), req.workspace.as_deref()));
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({"type": "function", "function": {
                            "name": t.name, "description": t.description, "parameters": t.schema
                        }})
                    })
                    .collect(),
            );
        }
        super::shaping::apply_thinking(&mut body, super::shaping::thinking_field(&self.base_url, &req.model), req.thinking);
        body
    }
}

fn normalise_base(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.is_empty() {
        OFFICIAL_BASE.to_string()
    } else {
        b.to_string()
    }
}

/// The provider-neutral transcript as Chat Completions messages.
///
/// `replay_reasoning` sends earlier thinking back as `reasoning_content`.
/// Reasoning models behind relays (Qwen, DeepSeek) expect it in tool rounds;
/// the official API does not take it.
pub fn convert_messages(messages: &[Message], replay_reasoning: bool, workspace: Option<&Path>) -> Vec<Value> {
    let inline = super::attachments::inline_set(messages);
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::User => out.push(json!({"role": "user", "content": user_content(m, &inline, workspace)})),
            Role::Assistant => {
                let text = m.text();
                let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::from(text) }});
                let calls: Vec<Value> = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolCall { id, name, input_json, .. } => Some(json!({
                            "id": id, "type": "function",
                            "function": {"name": name, "arguments": input_json}
                        })),
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                if replay_reasoning {
                    let thinking: String = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Thinking { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    if !thinking.is_empty() {
                        msg["reasoning_content"] = Value::from(thinking);
                    }
                }
                out.push(msg);
            }
            Role::Tool => {
                for p in &m.parts {
                    if let Part::ToolResult { call_id, output, .. } = p {
                        out.push(json!({"role": "tool", "tool_call_id": call_id, "content": output}));
                    }
                }
                // A tool message cannot hold an image in this protocol, so
                // the pictures follow as one user message, after every
                // result of the round (results must directly follow the
                // calls). It exists only in the request.
                let images: Vec<Value> = m
                    .parts
                    .iter()
                    .filter(|p| p.is_image())
                    .filter_map(|p| match p {
                        Part::Attachment { path, mime, .. } => {
                            let url = inline.contains(path).then(|| workspace.and_then(|ws| super::attachments::image_data_url(ws, path, mime))).flatten()?;
                            Some(json!({"type": "image_url", "image_url": {"url": url}}))
                        }
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    let mut content = vec![json!({"type": "text", "text": "[the image(s) returned by the tool call(s) above]"})];
                    content.extend(images);
                    out.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
    out
}

/// A user message's content: a plain string, or — when it carries images
/// that are sent — text and `image_url` parts in the order written.
fn user_content(m: &Message, inline: &std::collections::HashSet<String>, workspace: Option<&Path>) -> Value {
    use super::attachments::{files_block, image_data_url, omitted_note};
    let mut parts: Vec<Value> = Vec::new();
    let mut any_image = false;
    for p in &m.parts {
        match p {
            Part::Text { text } => parts.push(json!({"type": "text", "text": text})),
            Part::Attachment { path, mime, .. } if p.is_image() => {
                let url = inline.contains(path).then(|| workspace.and_then(|ws| image_data_url(ws, path, mime))).flatten();
                match url {
                    Some(url) => {
                        parts.push(json!({"type": "text", "text": format!("[attached image: {path}]")}));
                        parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                        any_image = true;
                    }
                    None => parts.push(json!({"type": "text", "text": omitted_note(p)})),
                }
            }
            _ => {}
        }
    }
    if let Some(block) = files_block(&m.parts) {
        parts.push(json!({"type": "text", "text": block}));
    }
    if any_image {
        return Value::Array(parts);
    }
    let text: Vec<&str> = parts.iter().filter_map(|p| p["text"].as_str()).collect();
    Value::from(text.join("\n\n"))
}

/// Turns streamed chunks into events. Relays differ: some send a tool call's
/// arguments in fragments, some in one piece, some as an object instead of a
/// string; some repeat the id on every fragment and some send it once.
#[derive(Default)]
pub struct ChunkParser {
    started: HashMap<u32, String>,
}

impl ChunkParser {
    pub fn parse(&mut self, data: &str) -> Result<Vec<StreamEvent>, CoreError> {
        let v: Value = serde_json::from_str(data)
            .map_err(|e| CoreError::Protocol { detail: format!("{e}: {}", data.chars().take(200).collect::<String>()) })?;
        if let Some(err) = v.get("error") {
            let detail = err.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string());
            return Err(CoreError::Protocol { detail });
        }
        let mut out = Vec::new();
        for choice in v.get("choices").and_then(Value::as_array).into_iter().flatten() {
            let delta = choice.get("delta").or_else(|| choice.get("message")).cloned().unwrap_or(Value::Null);
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    out.push(StreamEvent::Thinking(t.to_string()));
                    break;
                }
            }
            if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                out.push(StreamEvent::Text(t.to_string()));
            }
            for (pos, call) in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
                let index = call.get("index").and_then(Value::as_u64).map(|i| i as u32).unwrap_or(pos as u32);
                let func = call.get("function").cloned().unwrap_or(Value::Null);
                if !self.started.contains_key(&index) {
                    let name = func.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
                    self.started.insert(index, id.clone());
                    out.push(StreamEvent::ToolCallStart { index, id, name });
                }
                match func.get("arguments") {
                    Some(Value::String(s)) if !s.is_empty() => {
                        out.push(StreamEvent::ToolCallArgs { index, fragment: s.clone() })
                    }
                    Some(v @ Value::Object(_)) => out.push(StreamEvent::ToolCallArgs { index, fragment: v.to_string() }),
                    _ => {}
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                out.push(StreamEvent::Finish(match reason {
                    "stop" => FinishReason::Stop,
                    "tool_calls" | "function_call" => FinishReason::ToolCalls,
                    "length" => FinishReason::Length,
                    _ => FinishReason::Other,
                }));
            }
        }
        Ok(out)
    }
}

impl super::http::ChunkReader for ChunkParser {
    fn read(&mut self, data: &str) -> Option<Result<Vec<StreamEvent>, CoreError>> {
        if data.trim() == "[DONE]" {
            return None;
        }
        Some(self.parse(data))
    }
}

#[async_trait]
impl Provider for OpenAi {
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream, CoreError> {
        let body = self.build_body(&req);
        let request = self.client.post(format!("{}/chat/completions", self.base_url)).bearer_auth(&self.key);
        super::http::post_sse(request, &body, &self.capture, cancel, ChunkParser::default()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_tool_call_in_one_chunk_with_object_arguments() {
        let mut p = ChunkParser::default();
        let evs = p
            .parse(r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell","arguments":{"command":"ls"}}}]},"finish_reason":"tool_calls"}]}"#)
            .unwrap();
        assert_eq!(
            evs,
            vec![
                StreamEvent::ToolCallStart { index: 0, id: "c1".into(), name: "shell".into() },
                StreamEvent::ToolCallArgs { index: 0, fragment: r#"{"command":"ls"}"#.into() },
                StreamEvent::Finish(FinishReason::ToolCalls),
            ]
        );
    }

    #[test]
    fn fragments_after_the_first_do_not_restart_the_call() {
        let mut p = ChunkParser::default();
        p.parse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell","arguments":"{\"com"}}]}}]}"#).unwrap();
        let evs = p.parse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"mand\":\"ls\"}"}}]}}]}"#).unwrap();
        assert_eq!(evs, vec![StreamEvent::ToolCallArgs { index: 0, fragment: r#"mand":"ls"}"#.into() }]);
    }

    #[test]
    fn reasoning_and_text_are_told_apart() {
        let mut p = ChunkParser::default();
        let evs = p.parse(r#"{"choices":[{"delta":{"reasoning_content":"hmm","content":"hi"}}]}"#).unwrap();
        assert_eq!(evs, vec![StreamEvent::Thinking("hmm".into()), StreamEvent::Text("hi".into())]);
    }

    #[test]
    fn an_error_inside_the_stream_is_an_error() {
        assert!(ChunkParser::default().parse(r#"{"error":{"message":"overloaded"}}"#).is_err());
    }

    #[test]
    fn tool_results_become_tool_messages_after_the_calls() {
        let msgs = vec![
            Message { id: "a".into(), role: Role::Assistant, created_at: 0, parts: vec![
                Part::Thinking { text: "plan".into(), signature: None, redacted: None, origin: None },
                Part::ToolCall { id: "c1".into(), name: "shell".into(), input_json: r#"{"command":"ls"}"#.into(), title: None, signature: None },
            ]},
            Message { id: "t".into(), role: Role::Tool, created_at: 0, parts: vec![
                Part::ToolResult { call_id: "c1".into(), output: "x".into(), is_error: false, duration_ms: None },
            ]},
        ];
        let out = convert_messages(&msgs, true, None);
        assert_eq!(out[0]["content"], Value::Null);
        assert_eq!(out[0]["tool_calls"][0]["function"]["arguments"], r#"{"command":"ls"}"#);
        assert_eq!(out[0]["reasoning_content"], "plan");
        assert_eq!(out[1], json!({"role": "tool", "tool_call_id": "c1", "content": "x"}));
        assert!(convert_messages(&msgs, false, None)[0].get("reasoning_content").is_none());
    }

    /// A tool message cannot carry an image, and tool results must follow
    /// their calls directly: every result first, then one user message with
    /// the pictures.
    #[test]
    fn tool_images_follow_all_the_results_as_one_user_message() {
        use base64::Engine as _;
        let ws = std::env::temp_dir().join(format!("solos-oai-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.png"), base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=").unwrap()).unwrap();
        let msgs = vec![Message {
            id: "t".into(),
            role: Role::Tool,
            created_at: 0,
            parts: vec![
                Part::ToolResult { call_id: "c1".into(), output: "one".into(), is_error: false, duration_ms: None },
                Part::ToolResult { call_id: "c2".into(), output: "two".into(), is_error: false, duration_ms: None },
                Part::Attachment { path: "/solos/ws/a.png".into(), name: "a.png".into(), mime: "image/png".into(), size: 68 },
            ],
        }];
        let out = convert_messages(&msgs, false, Some(&ws));
        let roles: Vec<&str> = out.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, vec!["tool", "tool", "user"]);
        assert!(out[2]["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    }
}
