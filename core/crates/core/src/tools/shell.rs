//! `shell`: run a command in the sandbox.

use super::jobs::{Jobs, BACKGROUND_TIMEOUT};
use super::{clip, object_schema, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::sandbox::{ExecSpec, GUEST_WORKSPACE};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// The reference app's default: long enough for a package install.
const DEFAULT_TIMEOUT_SECS: u64 = 900;
const MAX_TIMEOUT_SECS: u64 = 4 * 3600;
/// What the model gets back at most; the middle of anything longer is cut.
pub const MAX_OUTPUT_CHARS: usize = 20_000;

/// Shares its job list with `shell_jobs`.
pub struct Shell(pub Arc<Jobs>);

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: "Run a shell command in the Linux sandbox on this device (Alpine Linux, busybox tools; \
                install more with `apk add`). The working directory is /solos/ws, the user's workspace, and files \
                there persist between calls. Returns the combined stdout and stderr and the exit code. For something long \
                (a big download, a build) that you want to check on while doing other things, pass background: true: \
                it returns a job id at once, and `shell_jobs` shows its output."
                .into(),
            schema: object_schema(
                json!({
                    "command": {"type": "string", "description": "The command or script to run with /bin/sh."},
                    "timeout": {"type": "integer", "description": "Seconds before the command is killed. Default 900 (4 hours in the background)."},
                    "background": {"type": "boolean", "description": "Start it and return a job id straight away instead of waiting. Default false."}
                }),
                &["command"],
            ),
            // Calls in one message run in the order the model wrote them:
            // it writes `create the file` then `read it` expecting exactly
            // that, and running them together reads a half-written file.
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let Some(command) = input.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) else {
            return ToolOutput::error("`command` is required.");
        };
        let background = input.get("background").and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true"))).unwrap_or(false);
        let given = input.get("timeout").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
        let timeout = match (given, background) {
            (Some(t), _) => t.clamp(1, MAX_TIMEOUT_SECS),
            (None, true) => BACKGROUND_TIMEOUT.as_secs(),
            (None, false) => DEFAULT_TIMEOUT_SECS,
        };
        let spec = ExecSpec {
            script: command.to_string(),
            cwd: GUEST_WORKSPACE.into(),
            // For `solos`: a call from the script belongs to this conversation.
            env: vec![("SOLOS_SESSION_ID".into(), ctx.session_id.clone())],
            timeout: Duration::from_secs(timeout),
        };
        if background {
            let id = self.0.start(ctx, command, spec);
            return ToolOutput::ok(format!(
                "Started job {id} in the background (killed after {timeout}s if still running). Check it with shell_jobs."
            ));
        }
        match ctx.sandbox.exec(spec, ctx.on_output.clone(), ctx.cancel.clone()).await {
            Err(e) => ToolOutput::error(format!("The command could not run: {e}")),
            Ok(r) => {
                let body = clip(r.output.trim_end(), MAX_OUTPUT_CHARS);
                let status = if r.timed_out {
                    format!("killed after {timeout}s (timeout)")
                } else if r.cancelled {
                    "stopped by the user".to_string()
                } else {
                    match r.exit_code {
                        Some(code) => format!("exit code {code}"),
                        None => "killed".to_string(),
                    }
                };
                let failed = r.timed_out || r.cancelled || r.exit_code != Some(0);
                let text = if body.is_empty() { format!("[{status}, no output]") } else { format!("{body}\n[{status}]") };
                ToolOutput { text, is_error: failed, ..Default::default() }
            }
        }
    }
}
