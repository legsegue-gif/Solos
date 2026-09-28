//! Request shaping: which fields a given (endpoint, model) gets to switch
//! thinking on or off.
//!
//! This is a table, not scattered string checks. The first rule whose
//! endpoint and model patterns both match decides; each row has a test. Rows
//! follow the reference app's rules; a model no row names gets no thinking
//! field at all, which leaves the endpoint's own default in force.

use serde_json::{json, Value};

/// How thinking is switched on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingField {
    /// Send nothing: the model decides for itself, or rejects the field.
    Omit,
    /// Root `enable_thinking: bool` (Qwen served by anything but DashScope;
    /// relays reject `thinking_budget` and `extra_body` with a 400).
    QwenRoot,
    /// `enable_thinking` at the root and inside `extra_body` (DashScope).
    QwenDual,
    /// Root `reasoning_effort`; `off` is the value to send when thinking is
    /// off, or `None` to leave the field out.
    ReasoningEffort { on: &'static str, off: Option<&'static str> },
    /// `reasoning: {effort}` (OpenRouter); left out when off, since
    /// forced-reasoning models reject `effort: "none"`.
    ReasoningNested { on: &'static str },
    /// DeepSeek V4: `thinking: {type}` with `reasoning_effort` beside it.
    DeepSeekSibling,
}

/// Where a pattern must match: `*` alone matches anything, a leading or
/// trailing `*` matches a suffix or prefix, `*x*` a substring.
struct Rule {
    endpoint: &'static str,
    model: &'static str,
    field: ThinkingField,
}

/// Endpoints are matched on the lower-cased base URL; models on the
/// lower-cased id.
const RULES: &[Rule] = &[
    // Mistral rejects unknown fields with a 422.
    Rule { endpoint: "*mistral.ai*", model: "*", field: ThinkingField::Omit },
    Rule { endpoint: "*openrouter.ai*", model: "*", field: ThinkingField::ReasoningNested { on: "medium" } },
    // gpt-5 has a documented off tier; the o-series takes only
    // low / medium / high, and "none" there is a 400.
    Rule { endpoint: "*api.openai.com*", model: "gpt-5*", field: ThinkingField::ReasoningEffort { on: "medium", off: Some("none") } },
    Rule { endpoint: "*", model: "o1*", field: ThinkingField::ReasoningEffort { on: "medium", off: None } },
    Rule { endpoint: "*", model: "o3*", field: ThinkingField::ReasoningEffort { on: "medium", off: None } },
    Rule { endpoint: "*", model: "o4*", field: ThinkingField::ReasoningEffort { on: "medium", off: None } },
    Rule { endpoint: "*", model: "gpt-5*", field: ThinkingField::ReasoningEffort { on: "medium", off: None } },
    Rule { endpoint: "*dashscope*", model: "*qwen*", field: ThinkingField::QwenDual },
    Rule { endpoint: "*", model: "*qwen*", field: ThinkingField::QwenRoot },
    Rule { endpoint: "*", model: "*deepseek-v4*", field: ThinkingField::DeepSeekSibling },
];

fn matches(pattern: &str, s: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        _ if pattern == "*" => true,
        (Some(rest), _) if rest.ends_with('*') => s.contains(&rest[..rest.len() - 1]),
        (Some(suffix), _) => s.ends_with(suffix),
        (None, Some(prefix)) => s.starts_with(prefix),
        (None, None) => s == pattern,
    }
}

pub fn thinking_field(base_url: &str, model: &str) -> ThinkingField {
    let base = base_url.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    RULES
        .iter()
        .find(|r| matches(r.endpoint, &base) && matches(r.model, &model))
        .map(|r| r.field)
        .unwrap_or(ThinkingField::Omit)
}

/// Write the thinking switch into a Chat Completions body.
pub fn apply_thinking(body: &mut Value, field: ThinkingField, on: bool) {
    match field {
        ThinkingField::Omit => {}
        ThinkingField::QwenRoot => body["enable_thinking"] = json!(on),
        ThinkingField::QwenDual => {
            body["enable_thinking"] = json!(on);
            body["extra_body"] = json!({"enable_thinking": on});
        }
        ThinkingField::ReasoningEffort { on: effort, off } => match (on, off) {
            (true, _) => body["reasoning_effort"] = json!(effort),
            (false, Some(off)) => body["reasoning_effort"] = json!(off),
            (false, None) => {}
        },
        ThinkingField::ReasoningNested { on: effort } => {
            if on {
                body["reasoning"] = json!({"effort": effort});
            }
        }
        ThinkingField::DeepSeekSibling => {
            if on {
                body["thinking"] = json!({"type": "enabled"});
                body["reasoning_effort"] = json!("high");
            } else {
                body["thinking"] = json!({"type": "disabled"});
            }
        }
    }
}

/// How an Anthropic model family is switched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinking {
    /// Thinking cannot be turned off (Fable, Mythos, Opus 5.5): `adaptive`
    /// when on, nothing when off; the app hides it.
    AlwaysOn,
    /// Thinks unless told not to (Opus 5, Sonnet 5): off is `disabled`.
    OnByDefault,
    /// Thinks only when asked (the 4.6–4.8 family): off sends nothing.
    OffByDefault,
    /// Older models: a fixed `budget_tokens` when on.
    Budget,
    /// Not a model any row names: nothing is sent either way.
    Unknown,
}

const ANTHROPIC_RULES: &[(&str, AnthropicThinking)] = &[
    ("*fable*", AnthropicThinking::AlwaysOn),
    ("*mythos*", AnthropicThinking::AlwaysOn),
    ("*opus-5-5*", AnthropicThinking::AlwaysOn),
    ("*opus-5*", AnthropicThinking::OnByDefault),
    ("*sonnet-5*", AnthropicThinking::OnByDefault),
    ("*-4-8*", AnthropicThinking::OffByDefault),
    ("*-4-7*", AnthropicThinking::OffByDefault),
    ("*-4-6*", AnthropicThinking::OffByDefault),
    ("*-4-5*", AnthropicThinking::Budget),
    ("*-4-1*", AnthropicThinking::Budget),
    ("*opus-4*", AnthropicThinking::Budget),
    ("*sonnet-4*", AnthropicThinking::Budget),
    ("*3-7-sonnet*", AnthropicThinking::Budget),
];

/// Thinking budget for the older models, below the request's `max_tokens`.
const ANTHROPIC_BUDGET: u32 = 10_000;

pub fn anthropic_family(model: &str) -> AnthropicThinking {
    let model = model.to_ascii_lowercase();
    ANTHROPIC_RULES.iter().find(|(p, _)| matches(p, &model)).map(|(_, f)| *f).unwrap_or(AnthropicThinking::Unknown)
}

/// The `thinking` field for an Anthropic request, or `None` to leave it out.
/// Summaries are asked for, since the raw reasoning is never returned and
/// the default display is empty.
pub fn anthropic_thinking(model: &str, on: bool) -> Option<Value> {
    use AnthropicThinking::*;
    let adaptive = json!({"type": "adaptive", "display": "summarized"});
    match (anthropic_family(model), on) {
        (AlwaysOn | OnByDefault | OffByDefault, true) => Some(adaptive),
        (OnByDefault, false) => Some(json!({"type": "disabled"})),
        (Budget, true) => Some(json!({"type": "enabled", "budget_tokens": ANTHROPIC_BUDGET})),
        _ => None,
    }
}

/// Gemini's `thinkingConfig`, or `None` to leave it out. On, thoughts are
/// asked for and the model picks the depth. Off, each family goes to its
/// floor: 3.x Flash to "minimal" (3.7 onwards rejects it, and Pro never had
/// it: "low"), 2.5 Pro to a budget of 128 (it cannot turn thinking off),
/// other 2.5 models to 0. Speech, image and embedding models take no
/// thinking config at all.
pub fn gemini_thinking(model: &str, on: bool) -> Option<Value> {
    let id = model.to_ascii_lowercase();
    if ["-tts", "-image", "-embedding", "-vision"].iter().any(|s| id.ends_with(s) || id.contains(&format!("{s}-"))) {
        return None;
    }
    if on {
        return Some(json!({"includeThoughts": true}));
    }
    if id.contains("gemini-3") {
        let minor = id
            .split("gemini-3.")
            .nth(1)
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|d| d.parse::<u32>().ok());
        let minimal = id.contains("flash") && minor.is_none_or(|m| m < 7);
        return Some(json!({"thinkingLevel": if minimal { "minimal" } else { "low" }}));
    }
    if id.contains("2.5-pro") {
        return Some(json!({"thinkingBudget": 128}));
    }
    if id.contains("2.5-flash-lite") {
        return None;
    }
    if id.contains("gemini-2.5") {
        return Some(json!({"thinkingBudget": 0}));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ThinkingField::*;

    fn body(base: &str, model: &str, on: bool) -> Value {
        let mut b = json!({});
        apply_thinking(&mut b, thinking_field(base, model), on);
        b
    }

    #[test]
    fn qwen_on_a_relay_gets_only_the_root_switch() {
        assert_eq!(thinking_field("http://localhost:8000/v1", "qwen3.8-max-thinking"), QwenRoot);
        assert_eq!(body("http://localhost:8000/v1", "qwen3.8-max", false), json!({"enable_thinking": false}));
        assert_eq!(body("https://relay.example/v1", "Qwen3.8-Max", true), json!({"enable_thinking": true}));
    }

    #[test]
    fn qwen_on_dashscope_also_gets_extra_body() {
        assert_eq!(
            body("https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen3-max", true),
            json!({"enable_thinking": true, "extra_body": {"enable_thinking": true}})
        );
    }

    #[test]
    fn openai_official_sends_none_when_off_and_relays_leave_it_out() {
        assert_eq!(body("https://api.openai.com/v1", "gpt-5.1", false), json!({"reasoning_effort": "none"}));
        assert_eq!(body("https://api.openai.com/v1", "o3-mini", true), json!({"reasoning_effort": "medium"}));
        assert_eq!(body("https://api.openai.com/v1", "o3", false), json!({}), "o-series has no off tier");
        assert_eq!(body("https://relay.example/v1", "gpt-5", false), json!({}));
        assert_eq!(body("https://relay.example/v1", "gpt-5", true), json!({"reasoning_effort": "medium"}));
    }

    #[test]
    fn openrouter_nests_the_effort_and_leaves_it_out_when_off() {
        assert_eq!(body("https://openrouter.ai/api/v1", "qwen/qwen3-max", true), json!({"reasoning": {"effort": "medium"}}));
        assert_eq!(body("https://openrouter.ai/api/v1", "deepseek/deepseek-r1", false), json!({}));
    }

    #[test]
    fn deepseek_v4_switch_and_tier_are_siblings() {
        assert_eq!(
            body("https://api.deepseek.com/v1", "deepseek-v4", true),
            json!({"thinking": {"type": "enabled"}, "reasoning_effort": "high"})
        );
        assert_eq!(body("https://api.deepseek.com/v1", "deepseek-v4-flash", false), json!({"thinking": {"type": "disabled"}}));
    }

    #[test]
    fn models_no_rule_names_get_nothing() {
        assert_eq!(thinking_field("https://api.deepseek.com/v1", "deepseek-chat"), Omit);
        assert_eq!(body("https://relay.example/v1", "glm-4.6", true), json!({}));
        assert_eq!(body("https://api.openai.com/v1", "gpt-4o", false), json!({}));
        assert_eq!(body("https://api.mistral.ai/v1", "magistral-medium", false), json!({}));
        assert_eq!(body("https://api.mistral.ai/v1", "qwen-on-mistral", true), json!({}));
    }

    #[test]
    fn anthropic_families_each_get_their_own_switch() {
        let t = |m: &str, on: bool| anthropic_thinking(m, on);
        let adaptive = Some(json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(t("claude-fable-5-1", true), adaptive);
        assert_eq!(t("claude-fable-5-1", false), None, "cannot be disabled: a 400");
        assert_eq!(t("claude-opus-5-5", false), None);
        assert_eq!(t("claude-opus-5", true), adaptive);
        assert_eq!(t("claude-opus-5", false), Some(json!({"type": "disabled"})), "thinks by default");
        assert_eq!(t("claude-sonnet-5", false), Some(json!({"type": "disabled"})));
        assert_eq!(t("claude-opus-4-8", true), adaptive);
        assert_eq!(t("claude-sonnet-4-6", false), None, "off unless asked");
        assert_eq!(t("claude-haiku-4-5", true), Some(json!({"type": "enabled", "budget_tokens": 10000})));
        assert_eq!(t("claude-haiku-4-5", false), None);
        assert_eq!(t("some-relay-model", true), None);
    }

    #[test]
    fn gemini_families_go_to_their_own_floor_when_off() {
        let off = |m: &str| gemini_thinking(m, false);
        assert_eq!(off("gemini-2.5-flash"), Some(json!({"thinkingBudget": 0})));
        assert_eq!(off("gemini-2.5-pro"), Some(json!({"thinkingBudget": 128})));
        assert_eq!(off("gemini-2.5-flash-lite"), None);
        assert_eq!(off("gemini-3-flash-preview"), Some(json!({"thinkingLevel": "minimal"})));
        assert_eq!(off("gemini-3.7-flash"), Some(json!({"thinkingLevel": "low"})));
        assert_eq!(off("gemini-3.1-pro-preview"), Some(json!({"thinkingLevel": "low"})));
        assert_eq!(gemini_thinking("gemini-3.1-flash-tts-preview", true), None);
        assert_eq!(gemini_thinking("gemini-3-pro", true), Some(json!({"includeThoughts": true})));
        assert_eq!(off("some-relay-model"), None);
    }
}
