//! Keeping a conversation inside the model's window.
//!
//! Two steps, with the reference app's thresholds (by window size):
//!
//! 1. **Clearing old tool output**, in the request only: the transcript is
//!    untouched and the UI shows everything. Oldest results go first; the
//!    recent turns keep theirs.
//! 2. **Summarising**, only when the user agrees: near the limit the turn
//!    stops and asks. A summary is a mark in the store, not a deletion, and
//!    it can be undone.
//!
//! When the window is unknown nothing is guessed; the endpoint's own
//! "too long" answer is the signal instead.

use solos_api::{Compaction, Message, Part, Role};

/// Tokens a request may use before step 1 and step 2, for a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Above this, old tool output is cleared (0: never).
    pub clear_above: u64,
    /// Clear down to this.
    pub clear_to: u64,
    /// Above this, stop and ask to summarise (0: the window is too small to
    /// summarise into; ask to start a new chat instead, at 90%).
    pub ask_above: u64,
    pub full_above: u64,
}

impl Policy {
    pub fn for_window(window: u64) -> Self {
        match window {
            w if w < 32_000 => Policy { clear_above: 0, clear_to: 0, ask_above: 0, full_above: w * 9 / 10 },
            w if w < 64_000 => Policy { clear_above: w - 10_000, clear_to: w - 15_000, ask_above: 0, full_above: w - 10_000 },
            w if w < 128_000 => Policy { clear_above: w - 20_000, clear_to: w - 30_000, ask_above: w - 10_000, full_above: w },
            w => Policy { clear_above: w - 40_000, clear_to: w - 60_000, ask_above: w - 20_000, full_above: w },
        }
    }
}

/// User turns kept whole when older ones are summarised.
pub const KEEP_RECENT_USER_TURNS: usize = 3;
/// Tokens an attached image is counted as.
const IMAGE_TOKENS: u64 = 1_500;

/// A rough token count: four ASCII characters or one other character per
/// token. It only has to be good enough to act before the endpoint refuses.
pub fn estimate_text(s: &str) -> u64 {
    let (ascii, other) = s.chars().fold((0u64, 0u64), |(a, o), c| if c.is_ascii() { (a + 1, o) } else { (a, o + 1) });
    ascii / 4 + other
}

pub fn estimate_message(m: &Message) -> u64 {
    m.parts
        .iter()
        .map(|p| match p {
            Part::Text { text } | Part::Thinking { text, .. } => estimate_text(text),
            Part::ToolCall { input_json, .. } => estimate_text(input_json) + 10,
            Part::ToolResult { output, .. } => estimate_text(output) + 10,
            Part::Attachment { .. } if p.is_image() => IMAGE_TOKENS,
            Part::Attachment { path, .. } => estimate_text(path) + 20,
        })
        .sum::<u64>()
        + 4
}

pub fn estimate(system: &str, tools_json: &str, messages: &[Message]) -> u64 {
    estimate_text(system) + estimate_text(tools_json) + messages.iter().map(estimate_message).sum::<u64>()
}

/// The transcript the model sees: after the newest summary in force, with
/// the summary in front as a user message.
pub fn visible(messages: &[Message], compactions: &[Compaction]) -> Vec<Message> {
    let Some(c) = compactions.iter().rev().find(|c| !c.undone) else { return messages.to_vec() };
    let Some(cut) = messages.iter().position(|m| m.id == c.through_message_id) else { return messages.to_vec() };
    let mut out = vec![Message {
        id: format!("summary-{}", c.id),
        role: Role::User,
        parts: vec![Part::Text { text: summary_message(&c.summary) }],
        created_at: c.created_at,
    }];
    out.extend_from_slice(&messages[cut + 1..]);
    out
}

fn summary_message(summary: &str) -> String {
    format!("[Summary of the earlier part of this conversation, which was compacted to save context]\n\n{summary}")
}

/// Clear old tool output until the estimate is at or below `target`,
/// oldest first, leaving the last `KEEP_RECENT_USER_TURNS` user turns alone.
/// Returns how many results were cleared.
pub fn clear_old_tool_output(messages: &mut [Message], mut estimate: u64, target: u64) -> usize {
    let protect_from = recent_start(messages);
    let mut cleared = 0;
    for m in messages[..protect_from].iter_mut().filter(|m| m.role == Role::Tool) {
        if estimate <= target {
            break;
        }
        for p in &mut m.parts {
            if let Part::ToolResult { output, .. } = p {
                let before = estimate_text(output);
                if before < 50 {
                    continue;
                }
                *output = format!(
                    "[output cleared to save context: {} characters. Run the command again if it is needed.]",
                    output.chars().count()
                );
                estimate = estimate.saturating_sub(before).saturating_add(estimate_text(output));
                cleared += 1;
            }
        }
    }
    cleared
}

/// Index of the first message of the recent user turns that stay whole.
pub fn recent_start(messages: &[Message]) -> usize {
    let users: Vec<usize> = messages.iter().enumerate().filter(|(_, m)| m.role == Role::User).map(|(i, _)| i).collect();
    if users.len() <= KEEP_RECENT_USER_TURNS {
        return 0;
    }
    users[users.len() - KEEP_RECENT_USER_TURNS]
}

/// The summariser's instructions. What a summary must keep follows from what
/// it replaces: the model reads it as the conversation so far, then answers
/// the user's next message, so anything it drops is gone for the model, and
/// anything phrased as a task may be taken up again unasked.
pub const SUMMARY_SYSTEM: &str = "Condense the conversation you are given. The condensed text will stand in for it: \
an assistant will read it in place of the original messages and then answer the user's next message, \
so whatever you leave out is lost to that assistant.

Write it in the language the user wrote in, and in the past tense. Keep, word for word: file paths, URLs, \
names, ids, numbers, commands, code and error messages. Record what the user asked for, what was done about it \
and how each step turned out, what was decided and why, errors and how they were dealt with, and anything the \
user said about how they want things done. Say where things were left: which files exist and what they hold, \
what worked, and what was not finished, stated as a fact rather than as a task to resume.

If a summary of an even earlier part is included, fold it in so that one text covers everything. Give more room \
to recent events than to old ones. Reply with the condensed text only.";

/// Characters of one tool result kept when a transcript is written out to
/// be summarised.
const TOOL_OUTPUT_IN_SUMMARY: usize = 2_000;

/// A stretch of transcript as plain text for the summariser.
pub fn transcript_text(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        for p in &m.parts {
            let line = match (m.role, p) {
                (Role::User, Part::Text { text }) => format!("User: {text}"),
                (Role::User, Part::Attachment { path, .. }) => format!("User attached: {path}"),
                (Role::Assistant, Part::Text { text }) if !text.trim().is_empty() => format!("Assistant: {text}"),
                (Role::Assistant, Part::ToolCall { name, input_json, .. }) => format!("Assistant called {name}: {input_json}"),
                (Role::Tool, Part::ToolResult { output, is_error, .. }) => {
                    let body = crate::tools::clip(output, TOOL_OUTPUT_IN_SUMMARY);
                    format!("{}: {body}", if *is_error { "Tool error" } else { "Tool result" })
                }
                _ => continue,
            };
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }
    out
}

/// What to summarise now: the messages after the summary in force (whose
/// text is carried in) up to the recent turns, and the id of the last one.
/// `None` when there is nothing older than the recent turns.
pub fn compaction_span<'a>(messages: &'a [Message], compactions: &[Compaction]) -> Option<(Option<String>, &'a [Message])> {
    let current = compactions.iter().rev().find(|c| !c.undone);
    let start = current
        .and_then(|c| messages.iter().position(|m| m.id == c.through_message_id))
        .map(|i| i + 1)
        .unwrap_or(0);
    let rest = &messages[start..];
    let end = recent_start(rest);
    (end > 0).then(|| (current.map(|c| c.summary.clone()), &rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, role: Role, text: &str) -> Message {
        let parts = match role {
            Role::Tool => vec![Part::ToolResult { call_id: "c".into(), output: text.into(), is_error: false, duration_ms: None }],
            _ => vec![Part::Text { text: text.into() }],
        };
        Message { id: id.into(), role, parts, created_at: 0 }
    }

    fn turns(n: usize) -> Vec<Message> {
        (0..n)
            .flat_map(|i| {
                vec![
                    msg(&format!("u{i}"), Role::User, &format!("question {i}")),
                    msg(&format!("a{i}"), Role::Assistant, "calling"),
                    msg(&format!("t{i}"), Role::Tool, &"x".repeat(4_000)),
                    msg(&format!("r{i}"), Role::Assistant, "answer"),
                ]
            })
            .collect()
    }

    #[test]
    fn thresholds_follow_the_window() {
        assert_eq!(Policy::for_window(200_000).ask_above, 180_000);
        assert_eq!(Policy::for_window(200_000).clear_above, 160_000);
        assert_eq!(Policy::for_window(100_000).ask_above, 90_000);
        assert_eq!(Policy::for_window(40_000).ask_above, 0, "too small to summarise into");
    }

    #[test]
    fn estimates_count_cjk_per_character() {
        assert_eq!(estimate_text("abcdefgh"), 2);
        assert_eq!(estimate_text("你好"), 2);
    }

    #[test]
    fn old_tool_output_is_cleared_oldest_first_and_recent_turns_keep_theirs() {
        let mut m = turns(5);
        let est = estimate("", "", &m);
        let cleared = clear_old_tool_output(&mut m, est, est - 1_500);
        assert_eq!(cleared, 2);
        let out = |i: usize| match &m[i * 4 + 2].parts[0] {
            Part::ToolResult { output, .. } => output.clone(),
            _ => unreachable!(),
        };
        assert!(out(0).starts_with("[output cleared"));
        assert!(out(1).starts_with("[output cleared"));
        assert_eq!(out(2).len(), 4_000, "within the last three user turns");
    }

    #[test]
    fn a_summary_in_force_replaces_what_it_covers_and_undo_restores_it() {
        let m = turns(4);
        let mut c = vec![Compaction { id: "c1".into(), through_message_id: "r0".into(), summary: "S".into(), created_at: 0, undone: false }];
        let v = visible(&m, &c);
        assert_eq!(v.len(), 1 + 12);
        assert!(v[0].text().ends_with("S"));
        assert_eq!(v[1].id, "u1");
        c[0].undone = true;
        assert_eq!(visible(&m, &c).len(), 16);
    }

    #[test]
    fn the_span_stops_before_the_recent_turns_and_carries_the_old_summary() {
        let m = turns(5);
        let (prev, span) = compaction_span(&m, &[]).unwrap();
        assert!(prev.is_none());
        assert_eq!(span.last().unwrap().id, "r1");
        let c = vec![Compaction { id: "c1".into(), through_message_id: "r1".into(), summary: "S".into(), created_at: 0, undone: false }];
        assert!(compaction_span(&m, &c).is_none(), "only the recent turns are left");
        assert!(compaction_span(&turns(3), &[]).is_none());
    }
}
