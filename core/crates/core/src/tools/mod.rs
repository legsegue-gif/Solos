//! Tools the model can call.
//!
//! A tool is a name, a description, a JSON schema, and a function. The
//! description and the shape of the result are as much a part of the tool as
//! the function: they decide whether the model calls it at the right moment
//! and what it can do with the answer, so they are written for the model and
//! checked against one (see `tools/evals` in the repository).

pub mod browser;
pub mod device;
pub mod files;
pub mod image;
pub mod jobs;
pub mod shell;

use crate::sandbox::Sandbox;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Every schema asks for this: a few words shown on the tool's row in the UI.
pub const TITLE_ARG: &str = "title";

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub schema: Value,
    /// May run at the same time as other calls from the same message.
    pub parallel: bool,
}

/// What a tool hands back to the model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
    /// Workspace images the model is shown with the result (a picture it
    /// asked to look at, a screenshot).
    pub images: Vec<ToolImage>,
}

/// An image file in the workspace, by guest path.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolImage {
    pub path: String,
    pub mime: String,
    pub size: u64,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true, ..Default::default() }
    }
}

/// What a running tool can reach.
pub struct ToolContext {
    pub session_id: String,
    pub sandbox: Arc<dyn Sandbox>,
    pub cancel: CancellationToken,
    /// Live output for the UI while the tool runs.
    pub on_output: Arc<dyn Fn(&str) + Send + Sync>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput;
}

#[derive(Clone, Default)]
pub struct Registry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tools every platform has.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        let jobs = Arc::new(jobs::Jobs::default());
        r.add(Arc::new(shell::Shell(jobs.clone())));
        r.add(Arc::new(jobs::ShellJobs(jobs)));
        r.add(Arc::new(image::ReadImage));
        for t in files::tools() {
            r.add(t);
        }
        r
    }

    pub fn add(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }
}

/// Build an object schema that always includes the row title.
pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    let mut props = properties.as_object().cloned().unwrap_or_default();
    // A tool's own `title` would be replaced by the label and its value lost:
    // the model fills one field, and the call is shown by it too.
    assert!(!props.contains_key(TITLE_ARG), "`{TITLE_ARG}` is the call's label; name the tool's own field otherwise");
    props.insert(
        TITLE_ARG.into(),
        serde_json::json!({
            "type": "string",
            "description": "A few words saying what this call does, in the user's language. Shown to the user as the label of the call."
        }),
    );
    let mut req: Vec<Value> = vec![Value::from(TITLE_ARG)];
    req.extend(required.iter().map(|r| Value::from(*r)));
    serde_json::json!({ "type": "object", "properties": props, "required": req })
}

/// Keep the head and the tail of a long text, and say what was cut. The
/// middle of a long log is the least likely part to matter; the end usually
/// holds the error and the start the command's own banner.
pub fn clip(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head = max_chars * 2 / 5;
    let tail = max_chars - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}\n… [{} characters omitted] …\n{end}", count - head - tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_both_ends_and_says_how_much_went() {
        let text: String = (0..1000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let out = clip(&text, 100);
        assert!(out.starts_with(&text[..40]));
        assert!(out.ends_with(&text[940..]));
        assert!(out.contains("[900 characters omitted]"));
        assert_eq!(clip("short", 100), "short");
    }

    #[test]
    fn every_schema_asks_for_a_title() {
        let s = object_schema(serde_json::json!({"x": {"type": "string"}}), &["x"]);
        assert_eq!(s["required"], serde_json::json!(["title", "x"]));
    }
}
