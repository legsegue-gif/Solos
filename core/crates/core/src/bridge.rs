//! The tools, served to the sandbox.
//!
//! A script in the guest cannot call the model's tools, yet it is often the
//! better caller: a loop over a hundred calendar events, or a notification
//! when a build ends, belongs in a script, and only its result belongs in the
//! conversation. This serves the same `Registry` the agent uses over HTTP on
//! 127.0.0.1, and `solos` (crates/guest) calls it. Nothing is reimplemented
//! here, so a tool behaves the same whichever of the two called it.
//!
//! iSH has no `AF_UNIX`, but a guest's `connect()` to 127.0.0.1 is the app's
//! own loopback, so TCP on loopback with a token per start is the reach.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::sandbox::Sandbox;
use crate::tools::{Registry, ToolContext};

/// The guest CLI, built by `ios/build-core.sh` for the sandbox's Linux and
/// carried in the core so the two never drift apart. Empty in a build that
/// did not cross-compile it (desktop test runs); nothing is installed then.
pub const GUEST_CLI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/solos-guest"));

/// Where the CLI is installed in the guest.
pub const GUEST_CLI_PATH: &str = "/usr/local/bin/solos";

/// The header that says which conversation a script runs in.
const SESSION_HEADER: &str = "x-solos-session";

struct Served {
    tools: Registry,
    sandbox: Arc<dyn Sandbox>,
    token: String,
}

/// Start serving; returns the environment a guest process needs to reach
/// it (`SOLOS_API_URL`, `SOLOS_API_TOKEN`). The token is new each start and
/// is handed to processes, never written into the guest's filesystem.
pub async fn serve(tools: Registry, sandbox: Arc<dyn Sandbox>, stop: CancellationToken) -> std::io::Result<Vec<(String, String)>> {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = listener.local_addr()?.port();
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let env = vec![
        ("SOLOS_API_URL".to_string(), format!("http://127.0.0.1:{port}")),
        ("SOLOS_API_TOKEN".to_string(), token.clone()),
    ];
    let app = Router::new()
        .route("/v1/tools", get(list_tools))
        .route("/v1/tools/{name}", post(call_tool))
        .route("/v1/files/url", get(file_url))
        .with_state(Arc::new(Served { tools, sandbox, token }));
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(async move { stop.cancelled().await }).await {
            tracing::warn!("the tool bridge stopped: {e}");
        }
    });
    Ok(env)
}

fn authorized(served: &Served, headers: &HeaderMap) -> bool {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    // Compared in full whatever the first difference, so the time taken says
    // nothing about how much of a guess was right.
    presented.len() == served.token.len()
        && presented.bytes().zip(served.token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn refused(tool: &str, status: StatusCode, error: impl Into<String>) -> Response {
    reply(status, json!({"ok": false, "tool": tool, "error": error.into()}))
}

async fn list_tools(State(served): State<Arc<Served>>, headers: HeaderMap) -> Response {
    if !authorized(&served, &headers) {
        return refused("tools", StatusCode::UNAUTHORIZED, "invalid or missing token");
    }
    let tools: Vec<Value> = served
        .tools
        .specs()
        .into_iter()
        .map(|s| json!({"name": s.name, "description": s.description, "input_schema": s.schema}))
        .collect();
    reply(StatusCode::OK, json!({"ok": true, "tool": "tools", "data": tools}))
}

async fn call_tool(State(served): State<Arc<Served>>, Path(name): Path<String>, headers: HeaderMap, body: Option<Json<Value>>) -> Response {
    if !authorized(&served, &headers) {
        return refused(&name, StatusCode::UNAUTHORIZED, "invalid or missing token");
    }
    let Some(tool) = served.tools.get(&name) else {
        return refused(&name, StatusCode::NOT_FOUND, format!("there is no tool named {name}; `solos tools` lists them"));
    };
    let input = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let ctx = ToolContext {
        session_id: headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string(),
        sandbox: served.sandbox.clone(),
        cancel: CancellationToken::new(),
        on_output: Arc::new(|_| {}),
    };
    let out = tool.call(&ctx, &input).await;
    // The call happened: 200 whatever it said, so a script can tell "the
    // tool said no" from "the call never ran".
    if out.is_error {
        return reply(StatusCode::OK, json!({"ok": false, "tool": name, "error": out.text}));
    }
    let mut body = match serde_json::from_str::<Value>(&out.text) {
        Ok(data) => json!({"ok": true, "tool": name, "data": data}),
        Err(_) => json!({"ok": true, "tool": name, "text": out.text}),
    };
    // Pictures the model would have been shown: a script gets the files.
    if !out.images.is_empty() {
        body["images"] = out.images.iter().map(|i| Value::from(i.path.clone())).collect();
    }
    reply(StatusCode::OK, body)
}

#[derive(Deserialize)]
struct FileUrlQuery {
    path: String,
}

async fn file_url(State(served): State<Arc<Served>>, headers: HeaderMap, Query(q): Query<FileUrlQuery>) -> Response {
    if !authorized(&served, &headers) {
        return refused("files", StatusCode::UNAUTHORIZED, "invalid or missing token");
    }
    match crate::files::url_for(&q.path) {
        Some(url) => reply(StatusCode::OK, json!({"ok": true, "tool": "files", "data": {"url": url, "path": q.path}})),
        None => refused(
            "files",
            StatusCode::BAD_REQUEST,
            format!("{} is not inside {}, so it has no solos:// address", q.path, crate::sandbox::GUEST_WORKSPACE),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use crate::tools::{object_schema, Tool, ToolOutput, ToolSpec};
    use std::io::{Read, Write};
    use std::sync::Mutex;

    /// Records what reached it and the session it was told.
    struct Echo(Mutex<Vec<(String, Value)>>);

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "echo".into(), description: "Echo.".into(), schema: object_schema(json!({}), &[]), parallel: true }
        }
        async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
            self.0.lock().unwrap().push((ctx.session_id.clone(), input.clone()));
            if input["fail"] == true {
                return ToolOutput::error("it said no");
            }
            ToolOutput::ok(json!({"got": input["name"]}).to_string())
        }
    }

    /// Spoken by hand, as the guest CLI speaks it, so a change to the wire
    /// format the CLI relies on fails here.
    fn send(env: &[(String, String)], request: &str, token: Option<&str>, body: Option<&str>) -> (u16, Value) {
        let addr = env[0].1.trim_start_matches("http://").to_string();
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        let mut req = format!("{request} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nX-Solos-Session: s-42\r\n");
        if let Some(t) = token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        req.push_str(body.unwrap_or(""));
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap())
    }

    // Two workers: the requests below block, and the server needs a worker
    // of its own to answer them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_script_reaches_the_same_tools_with_its_arguments_and_its_session() {
        let echo = Arc::new(Echo(Mutex::new(vec![])));
        let mut tools = Registry::new();
        tools.add(echo.clone());
        let dir = std::env::temp_dir().join(format!("solos-bridge-{}", uuid::Uuid::new_v4()));
        let stop = CancellationToken::new();
        let env = serve(tools, Arc::new(HostSandbox::new(dir)), stop.clone()).await.unwrap();
        assert_eq!(env[0].0, "SOLOS_API_URL");
        assert!(env[0].1.starts_with("http://127.0.0.1:"), "{}", env[0].1);
        let token = env[1].1.clone();

        let (env2, t) = (env.clone(), token.clone());
        let (status, body) = tokio::task::spawn_blocking(move || send(&env2, "POST /v1/tools/echo", Some(&t), Some(r#"{"name":"中文"}"#)))
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, json!({"ok": true, "tool": "echo", "data": {"got": "中文"}}));
        assert_eq!(echo.0.lock().unwrap()[0], ("s-42".to_string(), json!({"name": "中文"})));

        let (env2, t) = (env.clone(), token.clone());
        let (status, body) = tokio::task::spawn_blocking(move || send(&env2, "POST /v1/tools/echo", Some(&t), Some(r#"{"fail":true}"#)))
            .await
            .unwrap();
        assert_eq!((status, body["ok"].clone(), body["error"].clone()), (200, json!(false), json!("it said no")));

        let (env2, t) = (env.clone(), token.clone());
        let (status, _) = tokio::task::spawn_blocking(move || send(&env2, "POST /v1/tools/nonesuch", Some(&t), Some("{}"))).await.unwrap();
        assert_eq!(status, 404);

        let (env2, t) = (env.clone(), token.clone());
        let (status, body) =
            tokio::task::spawn_blocking(move || send(&env2, "GET /v1/files/url?path=/solos/ws/a%20b.png", Some(&t), None)).await.unwrap();
        assert_eq!((status, body["data"]["url"].clone()), (200, json!("solos://ws/a%20b.png")));

        let (env2, t) = (env.clone(), token.clone());
        let (status, body) = tokio::task::spawn_blocking(move || send(&env2, "GET /v1/tools", Some(&t), None)).await.unwrap();
        assert_eq!((status, body["data"][0]["name"].clone()), (200, json!("echo")));

        for bad in [Some("not-the-token"), None] {
            let env2 = env.clone();
            let bad = bad.map(str::to_string);
            let (status, _) =
                tokio::task::spawn_blocking(move || send(&env2, "POST /v1/tools/echo", bad.as_deref(), Some("{}"))).await.unwrap();
            assert_eq!(status, 401);
        }
        assert_eq!(echo.0.lock().unwrap().len(), 2, "a refused request never reaches the tool");
        stop.cancel();
    }
}
