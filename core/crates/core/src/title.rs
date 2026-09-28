//! Automatic session titles.
//!
//! After the first reply, the session's own model is asked — thinking off,
//! no tools — for a title of a few words, from the start of the
//! conversation. If that fails or comes back empty, the first user message,
//! shortened, is the title, so a session never stays untitled.

use crate::providers::{ChatRequest, Provider, StreamEvent};
use futures::StreamExt;
use solos_api::{CoreError, Message, Role};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const SYSTEM: &str = "You write short titles for conversations. Answer with the title only: \
at most six words, no quotes, no punctuation at the end, in the language the user writes in.";

/// Characters of each message the title is written from.
const EXCERPT: usize = 200;
/// Length of the fallback title, in characters.
const FALLBACK_LEN: usize = 30;

/// The request for a title, or `None` when there is nothing to title yet
/// (no user text, or no reply with text or a tool call).
pub fn request(model: &str, transcript: &[Message]) -> Option<ChatRequest> {
    let user = transcript.iter().find(|m| m.role == Role::User)?.text();
    if user.trim().is_empty() {
        return None;
    }
    let reply = transcript
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .map(Message::text)
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default();
    let mut conversation = format!("User: {}", excerpt(&user));
    if !reply.is_empty() {
        conversation.push_str(&format!("\n\nAssistant: {}", excerpt(&reply)));
    }
    Some(ChatRequest {
        model: model.to_string(),
        system: SYSTEM.to_string(),
        messages: vec![Message {
            id: String::new(),
            role: Role::User,
            parts: vec![solos_api::Part::Text { text: format!("Title this conversation.\n\n{conversation}") }],
            created_at: 0,
        }],
        tools: vec![],
        thinking: false,
        workspace: None,
    })
}

fn excerpt(s: &str) -> String {
    s.chars().take(EXCERPT).collect()
}

/// Ask for a title; the model's answer cleaned up, or an error.
pub async fn generate(provider: &dyn Provider, req: ChatRequest, timeout: Duration) -> Result<String, CoreError> {
    let text = collect_text(provider, req, timeout).await?;
    clean(&text).ok_or(CoreError::EmptyResponse)
}

/// The text of a one-off reply (no tools), within `timeout`.
pub async fn collect_text(provider: &dyn Provider, req: ChatRequest, timeout: Duration) -> Result<String, CoreError> {
    let collect = async {
        let mut stream = provider.stream(req, CancellationToken::new()).await?;
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Text(t) = ev? {
                text.push_str(&t);
            }
        }
        Ok::<_, CoreError>(text)
    };
    tokio::time::timeout(timeout, collect).await.map_err(|_| CoreError::Stalled { seconds: timeout.as_secs() as u32 })?
}

/// The first non-empty line, without quotes, markdown emphasis or a
/// trailing full stop.
pub fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_start_matches('#').trim();
    let strip: &[char] = &['"', '\'', '“', '”', '「', '」', '*', '`'];
    let line = line.trim_end_matches(['.', '。']).trim().trim_matches(strip).trim();
    let line = line.trim_end_matches(['.', '。']).trim();
    (!line.is_empty()).then(|| line.chars().take(80).collect())
}

/// The first user message, whitespace collapsed and shortened.
pub fn fallback(transcript: &[Message]) -> Option<String> {
    let user = transcript.iter().find(|m| m.role == Role::User)?.text();
    let flat = user.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() > FALLBACK_LEN {
        Some(format!("{}…", flat.chars().take(FALLBACK_LEN).collect::<String>().trim_end()))
    } else {
        Some(flat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solos_api::Part;

    fn msg(role: Role, text: &str) -> Message {
        Message { id: "x".into(), role, parts: vec![Part::Text { text: text.into() }], created_at: 0 }
    }

    #[test]
    fn cleans_what_models_wrap_titles_in() {
        assert_eq!(clean("\"Alpine version check\"").as_deref(), Some("Alpine version check"));
        assert_eq!(clean("\n**查看系统版本**。\n").as_deref(), Some("查看系统版本"));
        assert_eq!(clean("# Title.\nmore").as_deref(), Some("Title"));
        assert_eq!(clean("  \n "), None);
    }

    #[test]
    fn fallback_is_the_first_user_message_shortened() {
        let t = vec![msg(Role::User, "  look at\n the   logs and tell me what failed last night please ")];
        assert_eq!(fallback(&t).as_deref(), Some("look at the logs and tell me w…"));
        assert_eq!(fallback(&[msg(Role::User, "hi")]).as_deref(), Some("hi"));
    }

    #[test]
    fn the_request_has_no_tools_and_thinking_off() {
        let t = vec![msg(Role::User, "hello"), msg(Role::Assistant, "hi there")];
        let r = request("m", &t).unwrap();
        assert!(r.tools.is_empty());
        assert!(!r.thinking);
        assert!(r.messages[0].text().contains("User: hello\n\nAssistant: hi there"));
        assert!(request("m", &[]).is_none());
    }
}
