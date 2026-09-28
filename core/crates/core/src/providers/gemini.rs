//! Google Gemini API (`generateContent`), over raw HTTP.
//!
//! Function calls arrive whole, each possibly carrying a `thoughtSignature`
//! that Gemini needs back on the same part in the next request; thoughts
//! are replayed only to the model that produced them. Tool results travel
//! as `functionResponse` parts named after their call.

use super::http::{get_json, post_sse, ChunkReader};
use super::shaping::gemini_thinking;
use super::{attachments, origin, Capture, ChatRequest, EventStream, FinishReason, Provider, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use solos_api::{CoreError, Message, ModelInfo, Part, Role};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub const PROTOCOL: &str = "gemini";
const OFFICIAL_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Calls Gemini sent without an id get one of ours, marked so it is never
/// sent back as if Gemini had issued it.
const LOCAL_ID: &str = "local_";

pub struct Gemini {
    client: reqwest::Client,
    base_url: String,
    key: String,
    capture: Capture,
}

impl Gemini {
    pub fn new(base_url: &str, key: String, capture: Capture) -> Self {
        let b = base_url.trim().trim_end_matches('/');
        let base_url = if b.is_empty() { OFFICIAL_BASE.to_string() } else { b.to_string() };
        Self { client: super::http::client(), base_url, key, capture }
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, CoreError> {
        let v = get_json(self.client.get(format!("{}/models?pageSize=1000", self.base_url)).header("x-goog-api-key", &self.key)).await?;
        Ok(v.get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| {
                m.get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_none_or(|ms| ms.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| {
                let name = m.get("name")?.as_str()?;
                Some(ModelInfo {
                    id: name.strip_prefix("models/").unwrap_or(name).to_string(),
                    display_name: m.get("displayName").and_then(Value::as_str).map(str::to_string),
                    context_window: m.get("inputTokenLimit").and_then(Value::as_u64),
                    max_output: m.get("outputTokenLimit").and_then(Value::as_u64),
                })
            })
            .collect())
    }

    pub fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": req.system}]},
            "contents": convert_messages(&req.messages, &origin(PROTOCOL, &req.model), req.workspace.as_deref()),
        });
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "parameters": strip_unsupported_schema(&t.schema)}))
                .collect();
            body["tools"] = json!([{"functionDeclarations": decls}]);
        }
        if let Some(cfg) = gemini_thinking(&req.model, req.thinking) {
            body["generationConfig"] = json!({"thinkingConfig": cfg});
        }
        body
    }
}

/// Gemini implements a subset of JSON Schema and rejects the rest.
fn strip_unsupported_schema(v: &Value) -> Value {
    const DROP: [&str; 6] = ["additionalProperties", "$schema", "exclusiveMinimum", "exclusiveMaximum", "const", "examples"];
    match v {
        Value::Object(map) => Value::Object(
            map.iter().filter(|(k, _)| !DROP.contains(&k.as_str())).map(|(k, v)| (k.clone(), strip_unsupported_schema(v))).collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_unsupported_schema).collect()),
        other => other.clone(),
    }
}

/// The transcript as `contents`. Roles must alternate, so turns of one role
/// in a row are merged; tool results go in a user turn.
pub fn convert_messages(messages: &[Message], origin: &str, workspace: Option<&Path>) -> Vec<Value> {
    let inline = attachments::inline_set(messages);
    // Call id → tool name: a `functionResponse` is matched by name.
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in messages {
        let mut parts = Vec::new();
        let role = match m.role {
            Role::User => {
                for p in &m.parts {
                    match p {
                        Part::Text { text } if !text.is_empty() => parts.push(json!({"text": text})),
                        Part::Attachment { path, mime, .. } if p.is_image() => {
                            match inline.contains(path).then(|| workspace.and_then(|ws| attachments::image_base64(ws, path))).flatten() {
                                Some(data) => {
                                    parts.push(json!({"text": format!("[attached image: {path}]")}));
                                    parts.push(json!({"inlineData": {"mimeType": mime, "data": data}}));
                                }
                                None => parts.push(json!({"text": attachments::omitted_note(p)})),
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(files) = attachments::files_block(&m.parts) {
                    parts.push(json!({"text": files}));
                }
                "user"
            }
            Role::Assistant => {
                for p in &m.parts {
                    match p {
                        Part::Thinking { text, signature, origin: from, .. } if from.as_deref() == Some(origin) => {
                            let mut part = json!({"text": text, "thought": true});
                            if let Some(sig) = signature {
                                part["thoughtSignature"] = json!(sig);
                            }
                            parts.push(part);
                        }
                        Part::Text { text } if !text.is_empty() => parts.push(json!({"text": text})),
                        Part::ToolCall { id, name, input_json, signature, .. } => {
                            names.insert(id, name);
                            let args = serde_json::from_str::<Value>(input_json).ok().filter(Value::is_object).unwrap_or(json!({}));
                            let mut part = json!({"functionCall": {"name": name, "args": args}});
                            if !id.starts_with(LOCAL_ID) {
                                part["functionCall"]["id"] = json!(id);
                            }
                            if let Some(sig) = signature {
                                part["thoughtSignature"] = json!(sig);
                            }
                            parts.push(part);
                        }
                        _ => {}
                    }
                }
                "model"
            }
            Role::Tool => {
                for p in &m.parts {
                    if let Part::ToolResult { call_id, output, is_error, .. } = p {
                        let key = if *is_error { "error" } else { "output" };
                        let mut part = json!({"functionResponse": {
                            "name": names.get(call_id.as_str()).copied().unwrap_or(""),
                            "response": {key: output},
                        }});
                        if !call_id.starts_with(LOCAL_ID) {
                            part["functionResponse"]["id"] = json!(call_id);
                        }
                        parts.push(part);
                    }
                }
                // Then the images the results returned.
                for p in m.parts.iter().filter(|p| p.is_image()) {
                    if let Part::Attachment { path, mime, .. } = p {
                        if let Some(data) = inline.contains(path).then(|| workspace.and_then(|ws| attachments::image_base64(ws, path))).flatten() {
                            parts.push(json!({"inlineData": {"mimeType": mime, "data": data}}));
                        }
                    }
                }
                "user"
            }
        };
        if parts.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((r, p)) if *r == role => p.extend(parts),
            _ => turns.push((role, parts)),
        }
    }
    turns.into_iter().map(|(role, parts)| json!({"role": role, "parts": parts})).collect()
}

/// Reads `streamGenerateContent` chunks.
#[derive(Default)]
pub struct ChunkReaderImpl {
    calls: u32,
}

impl ChunkReader for ChunkReaderImpl {
    fn read(&mut self, data: &str) -> Option<Result<Vec<StreamEvent>, CoreError>> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => return Some(Err(CoreError::Protocol { detail: format!("{e}: {}", data.chars().take(200).collect::<String>()) })),
        };
        if let Some(err) = v.get("error") {
            let detail = err.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            return Some(Err(match err.get("code").and_then(Value::as_u64) {
                Some(code) => super::http_error(code as u16, &detail),
                None => CoreError::Protocol { detail },
            }));
        }
        if let Some(reason) = v.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
            return Some(Err(CoreError::Protocol { detail: format!("prompt blocked: {reason}") }));
        }
        let mut out = Vec::new();
        let Some(cand) = v.get("candidates").and_then(|c| c.get(0)) else { return Some(Ok(out)) };
        for part in cand.pointer("/content/parts").and_then(Value::as_array).into_iter().flatten() {
            let sig = part.get("thoughtSignature").and_then(Value::as_str).map(str::to_string);
            if let Some(call) = part.get("functionCall") {
                let index = self.calls;
                self.calls += 1;
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{LOCAL_ID}{}", uuid::Uuid::new_v4().simple()));
                out.push(StreamEvent::ToolCallStart { index, id, name: call["name"].as_str().unwrap_or("").to_string() });
                out.push(StreamEvent::ToolCallArgs { index, fragment: call.get("args").cloned().unwrap_or(json!({})).to_string() });
                if let Some(signature) = sig {
                    out.push(StreamEvent::ToolCallSignature { index, signature });
                }
            } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    if !text.is_empty() {
                        out.push(StreamEvent::Thinking(text.to_string()));
                    }
                    if let Some(s) = sig {
                        out.push(StreamEvent::ThinkingSignature(s));
                    }
                } else if !text.is_empty() {
                    out.push(StreamEvent::Text(text.to_string()));
                }
            }
        }
        if let Some(reason) = cand.get("finishReason").and_then(Value::as_str) {
            out.push(StreamEvent::Finish(match reason {
                "STOP" if self.calls > 0 => FinishReason::ToolCalls,
                "STOP" => FinishReason::Stop,
                "MAX_TOKENS" => FinishReason::Length,
                _ => FinishReason::Other,
            }));
        }
        Some(Ok(out))
    }
}

#[async_trait]
impl Provider for Gemini {
    fn protocol(&self) -> &'static str {
        PROTOCOL
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream, CoreError> {
        let body = self.build_body(&req);
        let url = format!("{}/models/{}:streamGenerateContent?alt=sse", self.base_url, req.model);
        let request = self.client.post(url).header("x-goog-api-key", &self.key);
        post_sse(request, &body, &self.capture, cancel, ChunkReaderImpl::default()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolSpec;

    #[test]
    fn chunks_with_thoughts_text_and_a_signed_call() {
        let mut r = ChunkReaderImpl::default();
        let a = r.read(r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hmm","thought":true,"thoughtSignature":"T1"}]}}]}"#).unwrap().unwrap();
        assert_eq!(a, vec![StreamEvent::Thinking("hmm".into()), StreamEvent::ThinkingSignature("T1".into())]);
        let b = r
            .read(r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Checking."},{"functionCall":{"name":"shell","args":{"command":"ls"}},"thoughtSignature":"S1"}]},"finishReason":"STOP"}]}"#)
            .unwrap()
            .unwrap();
        assert_eq!(b[0], StreamEvent::Text("Checking.".into()));
        assert!(matches!(&b[1], StreamEvent::ToolCallStart { index: 0, name, .. } if name == "shell"));
        assert_eq!(b[2], StreamEvent::ToolCallArgs { index: 0, fragment: r#"{"command":"ls"}"#.into() });
        assert_eq!(b[3], StreamEvent::ToolCallSignature { index: 0, signature: "S1".into() });
        assert_eq!(b[4], StreamEvent::Finish(FinishReason::ToolCalls));
    }

    #[test]
    fn errors_in_the_stream_keep_their_status() {
        let e = ChunkReaderImpl::default().read(r#"{"error":{"code":503,"message":"overloaded","status":"UNAVAILABLE"}}"#).unwrap().unwrap_err();
        assert!(e.is_transient(), "{e:?}");
    }

    #[test]
    fn calls_and_results_round_trip_with_names_and_signatures() {
        let msgs = vec![
            Message { id: "u".into(), role: Role::User, parts: vec![Part::Text { text: "go".into() }], created_at: 0 },
            Message { id: "a".into(), role: Role::Assistant, created_at: 0, parts: vec![
                Part::Thinking { text: "plan".into(), signature: Some("T".into()), redacted: None, origin: Some("gemini:gemini-3-pro".into()) },
                Part::ToolCall { id: "c1".into(), name: "shell".into(), input_json: r#"{"command":"ls"}"#.into(), title: None, signature: Some("S".into()) },
            ]},
            Message { id: "t".into(), role: Role::Tool, created_at: 0, parts: vec![
                Part::ToolResult { call_id: "c1".into(), output: "a.txt".into(), is_error: false, duration_ms: None },
            ]},
        ];
        let c = convert_messages(&msgs, "gemini:gemini-3-pro", None);
        assert_eq!(c[1]["role"], "model");
        assert_eq!(c[1]["parts"][0], json!({"text": "plan", "thought": true, "thoughtSignature": "T"}));
        assert_eq!(c[1]["parts"][1]["functionCall"]["args"], json!({"command": "ls"}));
        assert_eq!(c[1]["parts"][1]["thoughtSignature"], "S");
        assert_eq!(c[2]["parts"][0]["functionResponse"], json!({"name": "shell", "id": "c1", "response": {"output": "a.txt"}}));
        let mut local = msgs.clone();
        if let Part::ToolCall { id, .. } = &mut local[1].parts[1] {
            *id = "local_1".into();
        }
        if let Part::ToolResult { call_id, .. } = &mut local[2].parts[0] {
            *call_id = "local_1".into();
        }
        let l = convert_messages(&local, "gemini:gemini-3-pro", None);
        assert!(l[1]["parts"][1]["functionCall"].get("id").is_none(), "our own ids stay ours");
        assert_eq!(l[2]["parts"][0]["functionResponse"]["name"], "shell");
        let other = convert_messages(&msgs, "gemini:gemini-2.5-flash", None);
        assert!(other[1]["parts"][0].get("functionCall").is_some(), "no thoughts for another model");
    }

    #[test]
    fn schemas_lose_what_gemini_rejects() {
        let g = Gemini::new("", "k".into(), Capture::default());
        let body = g.build_body(&ChatRequest {
            model: "gemini-2.5-flash".into(),
            system: "s".into(),
            messages: vec![],
            tools: vec![ToolSpec { name: "t".into(), description: "d".into(), parallel: false,
                schema: json!({"type": "object", "additionalProperties": false, "properties": {"c": {"type": "string"}}}) }],
            thinking: false,
            workspace: None,
        });
        let params = &body["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(params.get("additionalProperties").is_none());
        assert_eq!(params["properties"]["c"]["type"], "string");
        assert_eq!(body["generationConfig"]["thinkingConfig"], json!({"thinkingBudget": 0}));
    }
}
