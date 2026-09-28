//! `browser` tool: drive a real web view on the device.
//!
//! Built and measured against the reference app.
//!
//! The platform supplies four primitives — navigate, evaluate JavaScript,
//! screenshot, manage tabs. Everything the model actually works with (the
//! page snapshot, clicking and typing by number, scrolling) is JavaScript
//! that lives here, so iOS and Android behave identically and a page that
//! misbehaves gets fixed in one place.
//!
//! Numbering is what makes the tool usable without vision: a snapshot hands
//! the model a numbered list of the interactive elements and keeps the nodes
//! behind those numbers in the page, so `click` takes `3` rather than a CSS
//! selector the model had to guess. The numbers die with the page, and a
//! stale one says so instead of clicking the wrong thing.
//!
//! Frames are out of scope for v1: only the top document is snapshotted.

use super::{object_schema, Tool, ToolContext, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

type ToolCtx = ToolContext;

/// This module's result, richer than the core's: pieces of content, and
/// the workspace files a call produced. Turned into the core's plain text
/// output at the end of each call.
#[derive(Default, Debug)]
struct ToolOutput {
    content: Vec<ToolResultContent>,
    is_error: bool,
    artifacts: Vec<String>,
    images: Vec<super::ToolImage>,
}

#[derive(Debug)]
enum ToolResultContent {
    Text { text: String },
}

impl ToolResultContent {
    fn text(text: impl Into<String>) -> Self {
        ToolResultContent::Text { text: text.into() }
    }
}

impl ToolOutput {
    fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ToolResultContent::text(text)], ..Default::default() }
    }
    fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ToolResultContent::text(text)], is_error: true, ..Default::default() }
    }
    fn with_artifact(mut self, url: String) -> Self {
        self.artifacts.push(url);
        self
    }
    fn plain(&self) -> String {
        self.content.iter().map(|ToolResultContent::Text { text }| text.as_str()).collect::<Vec<_>>().join("\n")
    }
    fn into_core(self) -> super::ToolOutput {
        super::ToolOutput { text: self.plain(), is_error: self.is_error, images: self.images }
    }
}

fn arg_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn arg_u64(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

fn arg_bool(input: &Value, key: &str) -> bool {
    input.get(key).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true"))).unwrap_or(false)
}

/// Text past `max` characters is written to a workspace file, and the
/// model is told where it is and how to read on.
struct Spilled {
    text: String,
    artifact: Option<String>,
}

/// Where long page text and screenshots are kept: in the workspace, so the
/// file tools reach them, under a folder the user rarely needs to see.
const KEEP_DIR: &str = ".solos";

/// The folders `temporary_dirs` names: what the browser tool saves as it
/// works, and nothing a person put there.
pub(crate) fn temporary_dirs(workspace: &std::path::Path) -> [std::path::PathBuf; 2] {
    [workspace.join(KEEP_DIR).join("pages"), workspace.join(KEEP_DIR).join("screenshots")]
}

/// What a platform must provide to expose a web view.
///
/// One entry point, like `DeviceBridge`, so the boundary stays JSON in and
/// JSON out. Operations:
///
/// - `navigate` `{tab, url}` → `{url, title}`; must wait for the load.
/// - `eval` `{tab, js}` → `{value}`; `js` is the body of an async function
///   that returns a string, so it may `await`.
/// - `screenshot` `{tab}` → `{base64, width, height}` of the visible viewport.
/// - `tabs` `{operation, tab, url}` → `{tabs: [...], active}`.
/// - `cancel` `{tab}` → anything: let go of whatever is in flight on that tab
///   and stop the page loading. It arrives on a different thread from the
///   call it is cancelling and must not wait for it.
///
/// Failures come back as `{"error": "..."}`. Calls arrive on a core thread,
/// so an implementation must hop to the main thread itself.
pub trait BrowserBridge: Send + Sync {
    fn call(&self, op: &str, input: Value) -> Value;
}

impl BrowserTool {
    pub fn new(bridge: Arc<dyn BrowserBridge>) -> Self {
        Self { bridge }
    }
}

fn schema(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    object_schema(Value::Object(props.into_iter().map(|(k, v)| (k.to_string(), v)).collect()), required)
}

tokio::task_local! {
    /// The cancel token of the call in flight.
    ///
    /// Ambient rather than threaded: the only place that needs it is the
    /// platform boundary at the bottom, and the twenty action methods in
    /// between have no business knowing that cancellation exists. Scoped in
    /// `call`, so it covers everything one tool call awaits and nothing else.
    static CANCEL: CancellationToken;
}

/// How much page text one result carries before it is spilled to a file.
const DEFAULT_TEXT_CHARS: usize = 5_000;
/// What `fetch` calls itself. Not `solos/x.y`: sites serve a different, often
/// worse page to something they do not recognise — Google answered the honest
/// name with a non-UTF-8 variant that arrived as mojibake.
const FETCH_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
/// How many interactive elements a snapshot lists.
const MAX_ELEMENTS: usize = 120;

pub struct BrowserTool {
    bridge: Arc<dyn BrowserBridge>,
}

#[async_trait]
impl Tool for BrowserTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".into(),
            description: "\
Control a web browser on the device, up to three tabs. To look at a web page — a link the user sent, or anything you need to read on the web — `navigate` to it, then `read` for its text or `snapshot` for everything on it and what you can click. The user sees the same page you do and can take over.

`fetch` also downloads: anything that is not text is written into /solos/ws and you get its `solos://` address back.

Actions:
- `navigate` — load `url` and return its title and text. Links, buttons and fields are not listed; take a `snapshot` when you need to click or type.
- `snapshot` — read the current page: its text plus a numbered list of the things you can click and type into.
- `read` — the article on this page, without the navigation, ads and footers. Use it once you are on something worth reading; it is far shorter and cleaner than `snapshot`.
- `find` — every element matching a CSS `selector`, with its text, link and whether it is on screen. For when you need one of many similar things.
- `hover` — put the pointer on `ref`, for menus that only appear under it.
- `collect` — scroll `amount` screens, gathering every `selector` match as you go. This is how you read a feed or a results list that loads as you scroll.
- `wait` — wait until the page stops changing, up to `timeout` ms. Useful after something that loads in the background.
- `click` — `ref` is a number from the last snapshot, or a CSS selector. Returns the page after the click.
- `type` — put `text` into `ref`; `submit: true` presses Enter afterwards.
- `scroll` — `to` is top, bottom, up, down, or a `ref` to bring into view.
- `screenshot` — a picture of the visible part of the page.
- `eval` — run `js` in the page and return its value. For anything the actions above do not cover.
- `fetch` — download `url` as a file, without opening it in a tab. This is for files — an image, a PDF, an archive — not for reading pages; a page is `navigate` + `read`. Text (JSON, plain files) comes back as text; anything else — an image, a PDF, an archive — is saved into /solos/ws and the reply gives you its `solos://` address. Pass `save_as` to choose the filename, or to save something that would otherwise be treated as text. It does not run JavaScript, so it is no use on a search engine or anything that draws itself.
- `tabs` — `operation` is list, open, close, or select. Three tabs at most: opening a fourth closes whichever has gone longest unused, and says which.
- `downloads` — `operation` is list or cancel (with `id`). Files a page downloads land in /solos/ws/downloads/ and can be read with the file tools once they finish.
- `user_agent` — `profile` is mobile (the default, iPhone Safari) or desktop (Mac Safari). Switch to desktop when a site serves a stripped-down mobile page.
- `viewport` — set the window the page thinks it is in: `width` × `height`, or `reset: true`. A layout that hides its navigation on a narrow screen hides it from you too.
- `cookies` — `operation` is get, set or clear, optionally limited to a `domain`. This is how a login the user made by hand carries over.

The user can watch and take over the browser while you are using it. If an action comes back saying the tab is being taken over, stop driving that tab and say so — the person is in the middle of something, usually logging in.

Element numbers come from the most recent snapshot of that page and stop being valid once the page changes; if a number is rejected, take a new snapshot."
                .into(),
            schema: schema(
                vec![
                    ("action", json!({
                        "type": "string",
                        "enum": ["navigate", "snapshot", "read", "find", "hover", "collect", "wait",
                                 "click", "type", "scroll", "screenshot", "eval", "fetch", "tabs",
                                 "downloads", "user_agent", "viewport", "cookies"]
                    })),
                    ("url", json!({"type": "string", "description": "For navigate and fetch, and for tabs open."})),
                    ("ref", json!({"type": "string", "description": "What to act on: an element number from the last snapshot, or a CSS selector."})),
                    ("text", json!({"type": "string", "description": "What to type, for type."})),
                    ("submit", json!({"type": "boolean", "description": "For type: press Enter afterwards."})),
                    ("to", json!({"type": "string", "description": "For scroll: top, bottom, up, down, or an element ref to bring into view. Default down."})),
                    ("amount", json!({"type": "integer", "description": "For scroll up/down: pixels. Default about one screen."})),
                    ("js", json!({"type": "string", "description": "For eval: JavaScript run in the page. It may await, and its value is returned. The last expression is not implicit; use return."})),
                    ("method", json!({"type": "string", "description": "For fetch: GET (default) or POST."})),
                    ("body", json!({"type": "string", "description": "For fetch: the request body."})),
                    ("headers", json!({"type": "object", "description": "For fetch: extra request headers."})),
                    ("operation", json!({"type": "string", "enum": ["list", "open", "close", "select", "cancel", "get", "set", "clear"], "description": "For tabs (list, open, close, select), downloads (list, cancel) and cookies (get, set, clear)."})),
                    ("id", json!({"type": "string", "description": "For downloads cancel: which download."})),
                    ("profile", json!({"type": "string", "enum": ["mobile", "desktop"], "description": "For user_agent."})),
                    ("width", json!({"type": "integer", "description": "For viewport, in points."})),
                    ("height", json!({"type": "integer", "description": "For viewport, in points."})),
                    ("reset", json!({"type": "boolean", "description": "For viewport: back to the window's own size."})),
                    ("domain", json!({"type": "string", "description": "For cookies: limit to this domain and its subdomains."})),
                    ("cookies", json!({"type": "array", "description": "For cookies set: [{name, value, domain, path, secure, http_only, expires}].", "items": {"type": "object"}})),
                    ("tab", json!({"type": "string", "description": "Which tab to act on. The active one by default."})),
                    ("selector", json!({"type": "string", "description": "CSS selector, for find and collect."})),
                    ("timeout", json!({"type": "integer", "description": "For wait: how long to allow, in ms. Default 5000."})),
                    ("full_page", json!({"type": "boolean", "description": "For screenshot: the whole scrollable page rather than what is on screen."})),
                    ("max_chars", json!({"type": "integer", "description": "Cap on returned page text (default 5000). Longer text is saved to a file you can read."})),
                    ("save_as", json!({"type": "string", "description": "For fetch: write the response to this filename in /solos/ws instead of returning it as text. Anything that is not text is saved automatically, named after the URL."})),
                ],
                &["action"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> super::ToolOutput {
        CANCEL.scope(ctx.cancel.clone(), self.dispatch(ctx, input.clone())).await.into_core()
    }
}

impl BrowserTool {
    async fn dispatch(&self, ctx: &ToolCtx, input: Value) -> ToolOutput {
        let Some(action) = arg_str(&input, "action") else {
            return ToolOutput::error("missing required argument: action");
        };
        let tab = arg_str(&input, "tab").unwrap_or("").to_string();
        let out = match action {
            "navigate" => self.navigate(ctx, &input, &tab).await,
            "snapshot" => self.snapshot(ctx, &input, &tab).await,
            "click" => self.click(ctx, &input, &tab).await,
            "type" => self.type_text(ctx, &input, &tab).await,
            "scroll" => self.scroll(ctx, &input, &tab).await,
            "screenshot" => self.screenshot(ctx, &input, &tab).await,
            "read" => self.read(ctx, &input, &tab).await,
            "find" => self.find(&input, &tab).await,
            "hover" => self.hover(&input, &tab).await,
            "collect" => self.collect(ctx, &input, &tab).await,
            "wait" => self.wait(&input, &tab).await,
            "eval" => self.eval_action(&input, &tab).await,
            "fetch" => self.fetch(ctx, &input).await,
            "tabs" => self.tabs(&input, &tab).await,
            "downloads" => self.downloads(&input).await,
            "user_agent" => self.user_agent(&input).await,
            "viewport" => self.viewport(&input, &tab).await,
            "cookies" => self.cookies(&input).await,
            other => Err(format!(
                "unknown action: {other}. Use navigate, snapshot, read, find, hover, collect, wait, click, type, scroll, screenshot, eval, fetch, tabs, downloads, user_agent, viewport or cookies."
            )),
        };
        match out {
            Ok(o) => o,
            Err(e) => ToolOutput::error(e),
        }
    }
}

impl BrowserTool {
    // -- the platform boundary ------------------------------------------------

    /// One call across to the platform, racing the user's Stop.
    ///
    /// Giving the turn three seconds to end and then abandoning whatever the
    /// browser was doing ends the turn but not the call: the web
    /// view kept loading, and a click already handed over still landed, up to
    /// forty-five seconds after Stop, quite possibly under the hands of
    /// someone who had just taken the tab over. So abandoning is not enough;
    /// the platform has to be told to let go, and that is what the `cancel`
    /// op is for.
    async fn bridge(&self, op: &str, input: Value) -> Result<Value, String> {
        let tab = input.get("tab").and_then(Value::as_str).unwrap_or("").to_string();
        let bridge = self.bridge.clone();
        let name = op.to_string();
        // The web view lives on the platform's main thread and a load can
        // take seconds, so this does not belong on a runtime thread.
        let call = tokio::task::spawn_blocking(move || bridge.call(&name, input));
        tokio::pin!(call);
        let joined = match CANCEL.try_with(|c| c.clone()) {
            Ok(cancel) => {
                tokio::select! {
                    joined = &mut call => joined,
                    _ = cancel.cancelled() => {
                        let bridge = self.bridge.clone();
                        // Fire and forget: the blocking thread this lands on
                        // returns as soon as the platform has let go, and by
                        // then nobody is waiting for this call's answer.
                        tokio::task::spawn_blocking(move || {
                            bridge.call("cancel", json!({"tab": tab}));
                        });
                        return Err("cancelled by user".into());
                    }
                }
            }
            // No scope: a direct caller (a test) rather than a tool call.
            Err(_) => call.await,
        };
        let value = joined.map_err(|e| format!("browser {op} failed to run: {e}"))?;
        if let Some(e) = value.get("error").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        Ok(value)
    }

    /// Wait for whatever the last action started to finish loading. A
    /// browser that cannot answer is not a reason to fail the action: the
    /// snapshot that follows says what the page actually is.
    ///
    /// Returns whatever the platform has to say about downloads that began or
    /// ended since it was last asked — a click on a download link produces no
    /// navigation and no page change, so without this it looks like nothing
    /// happened at all.
    async fn settle(&self, tab: &str) -> Option<String> {
        let mut note = None;
        match self.bridge("settle", json!({"tab": tab})).await {
            Ok(reply) => note = downloads_note(&reply),
            Err(e) => tracing::debug!("browser did not settle: {e}"),
        }
        // A finished load is not a finished page. A search page arrives empty
        // and fills itself in from script afterwards, and a click inside an
        // app is never a load at all — waiting on `isLoading` alone hands the
        // model the page as it was a moment before it became useful.
        if let Err(e) = self.eval(tab, stable_js(3_000)).await {
            tracing::debug!("the page never went quiet: {e}");
        }
        note
    }

    /// Run a snippet in the page. Every snippet this module sends returns a
    /// JSON string, so the platform only ever has to hand back a string.
    async fn eval(&self, tab: &str, js: String) -> Result<Value, String> {
        let reply = self.bridge("eval", json!({"tab": tab, "js": js})).await?;
        let raw = reply
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| "the browser returned no value".to_string())?;
        let parsed: Value = serde_json::from_str(raw)
            .map_err(|e| format!("the page returned malformed JSON: {e}"))?;
        if let Some(e) = parsed.get("__err").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        Ok(parsed)
    }

    // -- actions --------------------------------------------------------------

    async fn navigate(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let url = arg_str(input, "url").ok_or("navigate needs a url")?;
        let url = normalize_url(url);
        let landed = self.bridge("navigate", json!({"tab": tab, "url": url})).await?;
        let title = landed.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let _ = &title;
        // A URL that turns out to be a file never becomes a page: the web view
        // hands it to the download delegate and stays where it was.
        let mut notes: Vec<String> = downloads_note(&landed).into_iter().collect();
        // Some pages never stop loading (X holds connections open), so the
        // platform hands over what is on screen after a while instead of
        // failing with the post already drawn. Say so: what follows may be
        // missing whatever was still on its way.
        if landed.get("still_loading").and_then(Value::as_bool) == Some(true) {
            notes.push("The page was still loading after 30 seconds (some sites never stop). Below is what it shows so far, which may be incomplete; `wait` longer or `read` it.".into());
        }
        // The model almost always wants the page it just asked for, so hand
        // it over now rather than charge a second round trip for it.
        let page = self.eval(tab, snapshot_js()).await?;
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        let out = self.render_page(ctx, &page, max, false);
        Ok(with_note(out, (!notes.is_empty()).then(|| notes.join("\n"))))
    }

    async fn snapshot(
        &self,
        ctx: &ToolCtx,
        input: &Value,
        tab: &str,
    ) -> Result<ToolOutput, String> {
        let page = self.eval(tab, snapshot_js()).await?;
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        Ok(self.render_page(ctx, &page, max, true))
    }

    async fn click(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let target = arg_str(input, "ref").ok_or("click needs a ref: an element number or a CSS selector")?;
        let done = self.eval(tab, click_js(target)).await?;
        let what = done.get("clicked").and_then(Value::as_str).unwrap_or(target).to_string();
        // A click may navigate, in which case the page the model wants is not
        // the one that was there a moment ago. Only the web view knows when
        // the new one has arrived; a fixed wait either reads the old page or
        // wastes the difference.
        let note = self.settle(tab).await;
        let page = self.eval(tab, snapshot_js()).await?;
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        let mut out = self.render_page(ctx, &page, max, true);
        out.content.insert(0, ToolResultContent::text(format!("Clicked {what}.\n")));
        Ok(with_note(out, note))
    }

    async fn type_text(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let target = arg_str(input, "ref").ok_or("type needs a ref: an element number or a CSS selector")?;
        let text = arg_str(input, "text").unwrap_or("");
        let submit = arg_bool(input, "submit");
        let done = self.eval(tab, type_js(target, text, submit)).await?;
        let what = done.get("typed_into").and_then(Value::as_str).unwrap_or(target).to_string();
        let submitted = done.get("submitted").and_then(Value::as_bool).unwrap_or(false);
        if !submitted {
            return Ok(ToolOutput::text(format!("Typed into {what}."))
                );
        }
        let note = self.settle(tab).await;
        let page = self.eval(tab, snapshot_js()).await?;
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        Ok(with_note(self.render_page(ctx, &page, max, true), note))
    }

    async fn scroll(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let to = arg_str(input, "to").unwrap_or("down");
        let amount = arg_u64(input, "amount");
        self.eval(tab, scroll_js(to, amount)).await?;
        let page = self.eval(tab, snapshot_js()).await?;
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        Ok(self.render_page(ctx, &page, max, true))
    }

    /// The article, without the furniture around it. `snapshot` hands over
    /// everything on the page — navigation, ads, footer, cookie banner — and
    /// on a news site that is most of it. The text alone leaves the model room
    /// to say something about it.
    async fn read(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let page = self.eval(tab, readable_js()).await?;
        let title = page.get("title").and_then(Value::as_str).unwrap_or("");
        let url = page.get("url").and_then(Value::as_str).unwrap_or("");
        let body = page.get("text").and_then(Value::as_str).unwrap_or("");
        let source = page.get("source").and_then(Value::as_str).unwrap_or("");
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        let spilled = self.spill(ctx, body, max);
        let mut out = ToolOutput::text(format!("{url}\n{title}\n[{source}]\n\n{}", spilled.text.trim()));
        if let Some(a) = spilled.artifact {
            out.artifacts.push(a);
        }
        Ok(out)
    }

    /// Every match for a selector, so the model can pick one of many rather
    /// than guessing which `[3]` in a snapshot was the right row.
    async fn find(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let selector = arg_str(input, "selector").ok_or("find needs a selector")?;
        let found = self.eval(tab, find_js(selector)).await?;
        let rows = found.get("elements").and_then(Value::as_array).cloned().unwrap_or_default();
        let total = found.get("count").and_then(Value::as_u64).unwrap_or(rows.len() as u64);
        let mut s = format!("{total} match(es) for {selector}");
        if rows.len() < total as usize {
            s.push_str(&format!(", first {} shown", rows.len()));
        }
        s.push_str(":\n");
        for e in &rows {
            s.push_str(&element_line(e));
            if e.get("onscreen").and_then(Value::as_bool) == Some(false) {
                s.push_str(" [off screen]");
            }
            s.push('\n');
        }
        Ok(ToolOutput::text(s))
    }

    /// Menus that only exist while the pointer is on them.
    async fn hover(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let target = arg_str(input, "ref").ok_or("hover needs a ref")?;
        let done = self.eval(tab, hover_js(target)).await?;
        let what = done.get("hovered").and_then(Value::as_str).unwrap_or(target).to_string();
        Ok(ToolOutput::text(format!("Hovering {what}.")))
    }

    /// A feed is not a page: it is a page plus however much of it you have
    /// scrolled into existence. Without this the model reads the first screen
    /// and concludes that is all there is.
    async fn collect(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let selector = arg_str(input, "selector").ok_or("collect needs a selector")?;
        let rounds = arg_u64(input, "amount").unwrap_or(5).clamp(1, 30);
        let gathered = self.eval(tab, collect_js(selector, rounds)).await?;
        let items: Vec<String> = gathered
            .get("items")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let reached_end = gathered.get("reached_end").and_then(Value::as_bool).unwrap_or(false);
        let body = items
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        let spilled = self.spill(ctx, &body, max);
        let head = if reached_end {
            format!("{} item(s) for {selector}; reached the end of the page.\n\n", items.len())
        } else {
            format!("{} item(s) for {selector}; there is more below.\n\n", items.len())
        };
        let mut out = ToolOutput::text(format!("{head}{}", spilled.text));
        if let Some(a) = spilled.artifact {
            out.artifacts.push(a);
        }
        Ok(out)
    }

    async fn wait(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let limit = arg_u64(input, "timeout").unwrap_or(5_000).clamp(200, 60_000);
        let done = self.eval(tab, stable_js(limit)).await?;
        let ms = done.get("ms").and_then(Value::as_u64).unwrap_or(0);
        let changes = done.get("changes").and_then(Value::as_u64).unwrap_or(0);
        let settled = done.get("settled").and_then(Value::as_bool).unwrap_or(false);
        Ok(ToolOutput::text(if settled {
            format!("The page went quiet after {ms}ms ({changes} change(s)).")
        } else {
            format!("The page was still changing after {ms}ms ({changes} change(s)).")
        })
        )
    }

    async fn eval_action(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let js = arg_str(input, "js").ok_or("eval needs js")?;
        let value = self.eval(tab, eval_js(js)).await?;
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        Ok(ToolOutput::text(text))
    }

    /// `full_page` grows the web view to the document's own height before the
    /// picture is taken. It was advertised in the schema for a commit without
    /// being read here, so asking for the whole page quietly got one screen.
    async fn screenshot(&self, ctx: &ToolCtx, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let full = input.get("full_page").and_then(Value::as_bool).unwrap_or(false);
        let shot = self
            .bridge("screenshot", json!({"tab": tab, "full_page": full}))
            .await?;
        let b64 = shot
            .get("base64")
            .and_then(Value::as_str)
            .ok_or("the browser returned no image")?;
        let (w, h) = (
            shot.get("width").and_then(Value::as_u64).unwrap_or(0),
            shot.get("height").and_then(Value::as_u64).unwrap_or(0),
        );
        // Keep the picture on disk either way: the user can look at it from
        // the card, and a model without vision can still hand it to one that
        // has it.
        // On disk, where the user can open it from the reply and a script
        // can work on it, and shown to the model with the result.
        let saved = self.save_png(ctx, b64);
        let mut out = ToolOutput::text(match &saved {
            Some((url, _, _)) => format!("Screenshot ({w}×{h}{}) saved at {url}; the picture follows.", if full { ", whole page" } else { "" }),
            None => format!("Screenshot ({w}×{h}) could not be saved. Use snapshot or read to get the page as text."),
        });
        if let Some((url, guest, size)) = saved {
            out.artifacts.push(url);
            out.images.push(super::ToolImage { path: guest, mime: "image/png".into(), size });
        }
        Ok(out)
    }

    /// What a page is downloading, and a way to stop one. The files land in
    /// the workspace, so anything finished is readable with the file tools.
    async fn downloads(&self, input: &Value) -> Result<ToolOutput, String> {
        let operation = arg_str(input, "operation").unwrap_or("list");
        let id = arg_str(input, "id").unwrap_or("");
        let reply = self
            .bridge("downloads", json!({"operation": operation, "id": id}))
            .await?;
        let rows = reply.get("downloads").and_then(Value::as_array).cloned().unwrap_or_default();
        let text = if rows.is_empty() {
            "No downloads.".to_string()
        } else {
            rows.iter().map(download_line).collect::<Vec<_>>().join("\n")
        };
        Ok(ToolOutput::text(text))
    }

    /// What the site is told it is talking to. Applies to every open tab and
    /// survives the next launch, because a page loaded under the wrong agent
    /// has already chosen its markup.
    async fn user_agent(&self, input: &Value) -> Result<ToolOutput, String> {
        let profile = arg_str(input, "profile").unwrap_or("mobile");
        let reply = self.bridge("user_agent", json!({"profile": profile})).await?;
        let string = reply.get("user_agent").and_then(Value::as_str).unwrap_or("");
        Ok(ToolOutput::text(format!("User-Agent: {profile}\n{string}"))
            )
    }

    async fn viewport(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let reset = input.get("reset").and_then(Value::as_bool).unwrap_or(false);
        let width = input.get("width").and_then(Value::as_u64).unwrap_or(0);
        let height = input.get("height").and_then(Value::as_u64).unwrap_or(0);
        if !reset && (width == 0 || height == 0) {
            return Err("viewport needs width and height, or reset: true".into());
        }
        let reply = self
            .bridge("viewport", json!({"tab": tab, "reset": reset, "width": width, "height": height}))
            .await?;
        let (w, h) = (
            reply.get("width").and_then(Value::as_u64).unwrap_or(0),
            reply.get("height").and_then(Value::as_u64).unwrap_or(0),
        );
        Ok(ToolOutput::text(format!("Viewport is now {w}×{h}."))
            )
    }

    /// The seam between a login the person made by hand and the model
    /// carrying on in the same session. Values go to the model — it may have
    /// to carry one — but never into the card summary.
    async fn cookies(&self, input: &Value) -> Result<ToolOutput, String> {
        let operation = arg_str(input, "operation").unwrap_or("get");
        let domain = arg_str(input, "domain").unwrap_or("");
        let wanted = input.get("cookies").cloned().unwrap_or_else(|| json!([]));
        let reply = self
            .bridge(
                "cookies",
                json!({"operation": operation, "domain": domain, "cookies": wanted}),
            )
            .await?;
        match operation {
            "set" | "clear" => {
                let n = reply.get("changed").and_then(Value::as_u64).unwrap_or(0);
                let what = if operation == "set" { "Set" } else { "Cleared" };
                Ok(ToolOutput::text(format!("{what} {n} cookies.")))
            }
            _ => {
                let rows = reply.get("cookies").and_then(Value::as_array).cloned().unwrap_or_default();
                let text = serde_json::to_string_pretty(&rows).unwrap_or_else(|_| rows.len().to_string());
                Ok(ToolOutput::text(text)
                    )
            }
        }
    }

    async fn tabs(&self, input: &Value, tab: &str) -> Result<ToolOutput, String> {
        let operation = arg_str(input, "operation").unwrap_or("list");
        let url = arg_str(input, "url").map(normalize_url).unwrap_or_default();
        let reply = self
            .bridge("tabs", json!({"operation": operation, "tab": tab, "url": url}))
            .await?;
        let text = serde_json::to_string_pretty(&reply).unwrap_or_else(|_| reply.to_string());
        Ok(ToolOutput::text(text))
    }

    /// `fetch` does not go through the web view: no page to disturb, no
    /// layout to wait for, and the sandbox's `curl` is an interpreter away.
    async fn fetch(&self, ctx: &ToolCtx, input: &Value) -> Result<ToolOutput, String> {
        let url = normalize_url(arg_str(input, "url").ok_or("fetch needs a url")?);
        let method = arg_str(input, "method").unwrap_or("GET").to_ascii_uppercase();
        crate::providers::install_crypto_provider();
        let client = reqwest::Client::builder()
            .user_agent(FETCH_USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| format!("cannot build an HTTP client: {e}"))?;
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "GET" => client.get(&url),
            other => return Err(format!("fetch supports GET and POST, not {other}")),
        };
        if let Some(headers) = input.get("headers").and_then(Value::as_object) {
            for (k, v) in headers {
                if let Some(v) = v.as_str() {
                    req = req.header(k.as_str(), v);
                }
            }
        }
        if let Some(body) = arg_str(input, "body") {
            req = req.body(body.to_string());
        }
        let response = tokio::time::timeout(FETCH_STALL, req.send())
            .await
            .map_err(|_| format!("fetch failed: {url} sent no reply within {} s", FETCH_STALL.as_secs()))?
            .map_err(|e| format!("fetch failed: {e}"))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let final_url = response.url().to_string();
        let requested_name = arg_str(input, "save_as").map(str::to_string);
        let bytes = read_body(response, FETCH_STALL, FETCH_TOTAL).await?;

        // A file, not a page. The host's HTTP client is much faster than the
        // emulated guest's wget (a 12.8 MB image took the guest 63 s), so the
        // download happens here and the model is handed an solos:// URL —
        // the same address it would have written the file to, and the one
        // the chat can render.
        let binary = requested_name.is_some() || !looks_like_text(&content_type, &bytes);
        if binary && status.is_success() {
            let name = requested_name.unwrap_or_else(|| file_name_for(&final_url, &content_type));
            return Ok(match self.save_download(ctx, &name, &bytes) {
                Some(url) => ToolOutput::text(format!(
                    "{final_url}\n{} {content_type}\nSaved {} bytes to {url}",
                    status.as_u16(),
                    bytes.len()
                ))
                
                .with_artifact(url),
                None => ToolOutput::error(format!("could not write {name} into the workspace")),
            });
        }

        let body = String::from_utf8_lossy(&bytes).into_owned();
        let is_html = content_type.contains("html") || body.trim_start().starts_with("<!");
        let text = if is_html { html_to_text(&body) } else { body.clone() };
        let max = arg_u64(input, "max_chars").unwrap_or(DEFAULT_TEXT_CHARS as u64).clamp(200, 200_000) as usize;
        let spilled = self.spill(ctx, &text, max);
        let mut head = format!("{final_url}\n{} {}\n", status.as_u16(), content_type);
        if let Some(note) = js_shell_note(is_html, body.len(), text.chars().count()) {
            head.push_str(&note);
        }
        let mut out = if status.is_success() {
            ToolOutput::text(format!("{head}\n{}", spilled.text))
        } else {
            ToolOutput::error(format!("{head}\n{}", spilled.text))
        };
        if let Some(a) = spilled.artifact {
            out.artifacts.push(a);
        }
        Ok(out)
    }

    // -- rendering ------------------------------------------------------------

    /// `with_elements` is off for `navigate`: opening a page is almost always
    /// to read it, and a hundred numbered links under the text were most of
    /// what the model got back — on an X post, 4.5k of the 5k characters
    /// (measured 2026-09-25). `snapshot` still lists them for clicking.
    fn render_page(&self, ctx: &ToolCtx, page: &Value, max: usize, with_elements: bool) -> ToolOutput {
        let url = page.get("url").and_then(Value::as_str).unwrap_or("");
        let title = page.get("title").and_then(Value::as_str).unwrap_or("");
        let body = page.get("text").and_then(Value::as_str).unwrap_or("");
        let spilled = self.spill(ctx, body, max);

        let mut s = String::new();
        s.push_str(url);
        if !title.is_empty() {
            s.push_str(&format!("\n{title}"));
        }
        if let Some(scroll) = page.get("scroll") {
            let y = scroll.get("y").and_then(Value::as_u64).unwrap_or(0);
            let vh = scroll.get("viewport").and_then(Value::as_u64).unwrap_or(0);
            let h = scroll.get("height").and_then(Value::as_u64).unwrap_or(0);
            if h > vh + 8 {
                s.push_str(&format!("\nshowing {y}–{} of {h}px; scroll for more", y + vh));
            }
        }
        s.push_str("\n\n");
        s.push_str(spilled.text.trim());

        let elements = page.get("elements").and_then(Value::as_array).cloned().unwrap_or_default();
        if !with_elements {
            if !elements.is_empty() {
                s.push_str("\n\n(Links, buttons and fields are not listed here — `snapshot` lists them, numbered for click/type.)");
            }
        } else if elements.is_empty() {
            s.push_str("\n\nNothing on this page can be clicked or typed into.");
        } else {
            s.push_str("\n\nInteractive elements (use the number as `ref`):\n");
            for e in &elements {
                s.push_str(&element_line(e));
                s.push('\n');
            }
            if page.get("more_elements").and_then(Value::as_bool) == Some(true) {
                s.push_str(&format!("… only the first {MAX_ELEMENTS} are listed.\n"));
            }
        }

        let mut out = ToolOutput::text(s);
        if let Some(a) = spilled.artifact {
            out.artifacts.push(a);
        }
        out
    }

    /// The first `max` characters, and the whole text in a workspace file
    /// the model can page through with file_read.
    fn spill(&self, ctx: &ToolCtx, text: &str, max: usize) -> Spilled {
        let count = text.chars().count();
        if count <= max {
            return Spilled { text: text.to_string(), artifact: None };
        }
        let head: String = text.chars().take(max).collect();
        let name = format!("page-{}.txt", crate::agent::new_id());
        let dir = ctx.sandbox.workspace_dir().join(KEEP_DIR).join("pages");
        let saved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(&name), text)).is_ok();
        let guest = format!("{}/{KEEP_DIR}/pages/{name}", crate::sandbox::GUEST_WORKSPACE);
        let note = if saved {
            format!("\n\n[{} more characters. The whole text is in {guest}; read on with file_read (offset/lines).]", count - max)
        } else {
            format!("\n\n[{} more characters not shown.]", count - max)
        };
        Spilled { text: format!("{head}{note}"), artifact: saved.then(|| guest.replacen(crate::sandbox::GUEST_WORKSPACE, "solos://ws", 1)) }
    }

    /// Into the workspace, because that is where the user's files live and
    /// what `solos://ws/…` addresses.
    fn save_download(&self, ctx: &ToolCtx, name: &str, bytes: &[u8]) -> Option<String> {
        // One path segment, never an escape out of the workspace.
        let safe: String = name
            .rsplit('/')
            .next()
            .unwrap_or("download")
            .chars()
            .map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' })
            .collect();
        let safe = if safe.trim_matches('.').is_empty() { "download".to_string() } else { safe };
        let dir = ctx.sandbox.workspace_dir();
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(dir.join(&safe), bytes).ok()?;
        Some(format!("solos://ws/{safe}"))
    }

    /// The `solos://` address, guest path and size of the saved picture.
    fn save_png(&self, ctx: &ToolCtx, b64: &str) -> Option<(String, String, u64)> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        let dir = ctx.sandbox.workspace_dir().join(KEEP_DIR).join("screenshots");
        std::fs::create_dir_all(&dir).ok()?;
        let name = format!("screenshot-{}.png", crate::agent::new_id());
        std::fs::write(dir.join(&name), &bytes).ok()?;
        let rel = format!("{KEEP_DIR}/screenshots/{name}");
        Some((format!("solos://ws/{rel}"), format!("{}/{rel}", crate::sandbox::GUEST_WORKSPACE), bytes.len() as u64))
    }
}

/// Whether the bytes should be handed back as text at all. A declared text
/// type is taken at its word; anything else is judged by whether it decodes
/// as UTF-8, because plenty of servers label JSON as `application/octet-stream`.
/// A download is given up when nothing arrives for this long, not after a
/// fixed total: a large file on a slow line keeps going while bytes flow.
/// A fixed 60 s cut a 3 MB image mid-body and reported it as "error decoding
/// response body" (seen in a real turn).
const FETCH_STALL: Duration = Duration::from_secs(20);
/// The most a single download may take while still receiving data.
const FETCH_TOTAL: Duration = Duration::from_secs(300);

async fn read_body(mut response: reqwest::Response, stall: Duration, total: Duration) -> Result<Vec<u8>, String> {
    let started = std::time::Instant::now();
    let mut body = Vec::new();
    loop {
        if started.elapsed() > total {
            return Err(format!(
                "download stopped after {} s with {} bytes received; the server is too slow for this file",
                total.as_secs(),
                body.len()
            ));
        }
        match tokio::time::timeout(stall, response.chunk()).await {
            Err(_) => {
                return Err(format!(
                    "download stalled: nothing arrived for {} s after {} bytes",
                    stall.as_secs(),
                    body.len()
                ))
            }
            Ok(Err(e)) => return Err(format!("download broke off after {} bytes: {e}", body.len())),
            Ok(Ok(None)) => return Ok(body),
            Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
        }
    }
}

fn looks_like_text(content_type: &str, bytes: &[u8]) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("x-www-form-urlencoded")
    {
        return true;
    }
    if ct.starts_with("image/") || ct.starts_with("video/") || ct.starts_with("audio/")
        || ct.contains("pdf") || ct.contains("zip") || ct.contains("font")
    {
        return false;
    }
    // `application/octet-stream` deliberately falls through to the sniff
    // below: it is both the honest label for a real binary and what a server
    // reaches for when it cannot be bothered, and JSON arrives under it often
    // enough that trusting it would turn an API reply into a file on disk.
    // Nothing said. Judge the first kilobyte.
    let head = &bytes[..bytes.len().min(1024)];
    std::str::from_utf8(head).is_ok() || head.is_empty()
}

/// The last path segment, or something derived from the content type when the
/// URL has nothing usable in it.
fn file_name_for(url: &str, content_type: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    // Past the scheme and the host, or there is no path at all — otherwise
    // `https://x.org/` yields "x.org" and the download is named after the
    // site it came from.
    let after_scheme = no_query.split("://").nth(1).unwrap_or(no_query);
    let path = after_scheme.split_once('/').map(|(_, rest)| rest).unwrap_or("");
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    if last.contains('.') && last.len() <= 80 {
        return last.to_string();
    }
    let ext = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("bin")
        .trim();
    format!("download.{}", if ext.is_empty() { "bin" } else { ext })
}

/// `[3] link "Sign in" -> https://…`
/// Downloads that started, finished or failed since the browser was last
/// asked. The platform drains them as it reports them, so each is said once.
fn downloads_note(reply: &Value) -> Option<String> {
    let rows = reply.get("downloads")?.as_array()?;
    if rows.is_empty() {
        return None;
    }
    Some(rows.iter().map(download_line).collect::<Vec<_>>().join("\n"))
}

/// One download, as the model reads it. A finished one names the `solos://`
/// URL rather than the host path: that is the form the file tools take.
fn download_line(d: &Value) -> String {
    let get = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let bytes = d.get("bytes").and_then(Value::as_u64).unwrap_or(0);
    let (id, name) = (get("id"), get("name"));
    match d.get("state").and_then(Value::as_str).unwrap_or("running") {
        "finished" => format!("[{id}] {name} downloaded ({}) → {}", human_bytes(bytes), get("solos_url")),
        "failed" => {
            let why = d.get("error").and_then(Value::as_str).unwrap_or("no reason given");
            format!("[{id}] {name} failed: {why}")
        }
        "cancelled" => format!("[{id}] {name} cancelled"),
        _ => match d.get("total").and_then(Value::as_u64).unwrap_or(0) {
            0 => format!("[{id}] {name} downloading ({} so far)", human_bytes(bytes)),
            total => format!(
                "[{id}] {name} downloading ({} of {})",
                human_bytes(bytes),
                human_bytes(total)
            ),
        },
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", n as f64 / 1_073_741_824.0),
    }
}

/// Add something the model should know about that is not part of the page —
/// today only downloads. Kept out of the summary: the card says what the
/// action was, and a download is a side effect of it.
fn with_note(mut out: ToolOutput, note: Option<String>) -> ToolOutput {
    if let Some(note) = note {
        out.content.push(ToolResultContent::text(format!("\n{note}")));
    }
    out
}

fn element_line(e: &Value) -> String {
    let n = e.get("n").and_then(Value::as_u64).unwrap_or(0);
    let role = e.get("role").and_then(Value::as_str).unwrap_or("control");
    let label = e.get("label").and_then(Value::as_str).unwrap_or("");
    let mut line = format!("[{n}] {role}");
    if !label.is_empty() {
        line.push_str(&format!(" \"{label}\""));
    }
    if let Some(v) = e.get("value").and_then(Value::as_str) {
        if !v.is_empty() {
            line.push_str(&format!(" = \"{v}\""));
        }
    }
    if e.get("checked").and_then(Value::as_bool) == Some(true) {
        line.push_str(" [checked]");
    }
    if let Some(href) = e.get("href").and_then(Value::as_str) {
        line.push_str(&format!(" -> {href}"));
    }
    line
}

/// A page that is all script and no prose has not been read — it has been
/// downloaded. Say so, because the alternative reading is the one a model
/// actually reached: Google's JS shell yields the single sentence "if you
/// cannot reach Google Search, click here", from which it concluded the site
/// was blocked and spent four more rounds working around a problem that did
/// not exist.
fn js_shell_note(is_html: bool, html_bytes: usize, text_chars: usize) -> Option<String> {
    if !is_html || html_bytes < 10_000 || text_chars >= 500 {
        return None;
    }
    Some(format!(
        "NOTE: {html_bytes} bytes of HTML yielded only {text_chars} characters of text. \
This page builds itself with JavaScript, which fetch does not run, so what follows is \
not the page a browser would show. Use action \"navigate\" on this URL instead. Do not \
read this as the site being blocked or empty.\n"
    ))
}

fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.contains("://") || u.starts_with("about:") || u.starts_with("data:") {
        u.to_string()
    } else {
        format!("https://{u}")
    }
}


/// Enough of an HTML-to-text pass for `fetch`: drop the parts that are not
/// prose, unwrap the tags, and put the entities back.
///
/// Everything here walks **byte** offsets. It used to count characters while
/// slicing the string by bytes, which is the same thing only for ASCII: the
/// first Chinese character on the page put `i` inside a code point, the slice
/// panicked, and the whole tool task died. All the user saw was the words
/// "tool task failed" on a red card.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    // Only ASCII letters change, so this has the same byte length and the
    // same char boundaries as `html` — offsets are interchangeable.
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            // `i` is always on a boundary, so this char is whole.
            let ch = html[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        // Skip whole non-prose elements rather than just their tags.
        let mut skipped = false;
        for tag in ["script", "style", "noscript", "svg", "head"] {
            if !lower[i..].starts_with(&format!("<{tag}")) {
                continue;
            }
            i = match lower[i..].find(&format!("</{tag}")) {
                Some(end) => {
                    let close = i + end;
                    lower[close..].find('>').map(|g| close + g + 1).unwrap_or(bytes.len())
                }
                None => bytes.len(),
            };
            skipped = true;
            break;
        }
        if skipped {
            continue;
        }

        let block = [
            "<p", "<br", "<div", "<li", "<tr", "<h1", "<h2", "<h3", "<h4",
            "</p", "</div", "</li", "</tr", "</h",
        ]
        .iter()
        .any(|t| lower[i..].starts_with(t));
        i = lower[i..].find('>').map(|g| i + g + 1).unwrap_or(bytes.len());
        if block && !out.ends_with('\n') {
            out.push('\n');
        }
    }

    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // Collapse the blank lines the tag soup leaves behind.
    let mut text = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let l = line.trim();
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        text.push_str(l);
        text.push('\n');
    }
    text.trim().to_string()
}

// ---------------------------------------------------------------------------
// The page side
// ---------------------------------------------------------------------------

/// Shared helpers, prepended to every snippet. The snippet is the body of an
/// async function, so it may `await` and must `return` a string.
const PRELUDE: &str = r##"
const A = (window.__solos = window.__solos || { els: [] });
const S = (v) => { try { return JSON.stringify(v); } catch (e) { return JSON.stringify({ __err: 'not serialisable: ' + e }); } };
const err = (m) => JSON.stringify({ __err: m });
const vis = (el) => {
  const r = el.getBoundingClientRect();
  if (r.width < 2 || r.height < 2) return false;
  const s = getComputedStyle(el);
  return s.visibility !== 'hidden' && s.display !== 'none' && parseFloat(s.opacity || '1') > 0.05;
};
const clean = (s) => String(s == null ? '' : s).replace(/\s+/g, ' ').trim().slice(0, 100);
const label = (el) => clean(
  el.getAttribute('aria-label') ||
  el.getAttribute('placeholder') ||
  (el.labels && el.labels[0] && el.labels[0].innerText) ||
  el.innerText ||
  el.getAttribute('title') ||
  el.getAttribute('alt') ||
  el.getAttribute('name') || '');
const role = (el) => {
  const tag = el.tagName.toLowerCase();
  if (tag === 'a') return 'link';
  if (tag === 'button') return 'button';
  if (tag === 'select') return 'select';
  if (tag === 'textarea') return 'textbox';
  if (tag === 'summary') return 'disclosure';
  if (tag === 'input') {
    const t = (el.getAttribute('type') || 'text').toLowerCase();
    return ['checkbox', 'radio', 'submit', 'button', 'file', 'range'].indexOf(t) >= 0 ? t : 'textbox';
  }
  return el.getAttribute('role') || 'control';
};
const resolve = (ref) => {
  const s = String(ref == null ? '' : ref).trim();
  if (!s) return null;
  if (/^\d+$/.test(s)) return A.els[parseInt(s, 10) - 1] || null;
  try { return document.querySelector(s); } catch (e) { return null; }
};
const describe = (el) => el ? (role(el) + (label(el) ? ' "' + label(el) + '"' : '')) : 'nothing';
const stale = (ref) => err('no element matches ' + JSON.stringify(String(ref)) +
  '. Element numbers only last until the page changes, so take a snapshot and use a number from it.');
const where = () => ({ y: Math.round(window.scrollY), viewport: Math.round(window.innerHeight), height: Math.round(document.documentElement.scrollHeight) });
const settle = (ms) => new Promise((r) => setTimeout(r, ms));
"##;

const SELECTOR: &str = "a[href], button, input, select, textarea, summary, \
[role=button], [role=link], [role=checkbox], [role=radio], [role=tab], [role=menuitem], [role=switch], \
[onclick], [contenteditable=\"\"], [contenteditable=\"true\"]";

fn js(body: String) -> String {
    format!("{PRELUDE}\n{body}")
}

/// JSON so a value can be dropped into a snippet without quoting worries.
fn lit(v: impl serde::Serialize) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "null".into())
}

fn snapshot_js() -> String {
    js(format!(
        r##"
A.els = [];
const rows = [];
let more = false;
for (const el of document.querySelectorAll({sel})) {{
  if (el.disabled) continue;
  if (!vis(el)) continue;
  if (rows.length >= {max}) {{ more = true; break; }}
  const n = A.els.push(el);
  const row = {{ n: n, role: role(el), label: label(el) }};
  if (el.tagName === 'A' && el.href) row.href = String(el.href).slice(0, 200);
  if (typeof el.value === 'string' && el.value && el.type !== 'password') row.value = clean(el.value);
  if (el.checked === true) row.checked = true;
  rows.push(row);
}}
return S({{
  url: location.href,
  title: document.title,
  text: document.body ? document.body.innerText : '',
  elements: rows,
  more_elements: more,
  scroll: where(),
}});
"##,
        sel = lit(SELECTOR),
        max = MAX_ELEMENTS,
    ))
}

fn click_js(target: &str) -> String {
    js(format!(
        r##"
const ref = {target};
const el = resolve(ref);
if (!el) return stale(ref);
el.scrollIntoView({{ block: 'center', inline: 'center' }});
const what = describe(el);
if (typeof el.focus === 'function') el.focus({{ preventScroll: true }});
el.click();
return S({{ ok: true, clicked: what }});
"##,
        target = lit(target)
    ))
}

fn type_js(target: &str, text: &str, submit: bool) -> String {
    js(format!(
        r##"
const ref = {target};
const el = resolve(ref);
if (!el) return stale(ref);
el.scrollIntoView({{ block: 'center' }});
if (typeof el.focus === 'function') el.focus({{ preventScroll: true }});
const value = {text};
if (el.isContentEditable) {{
  el.textContent = value;
}} else {{
  // Go through the native setter so frameworks that watch the property
  // (React and friends) see the change instead of overwriting it.
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const d = Object.getOwnPropertyDescriptor(proto, 'value');
  if (d && d.set) d.set.call(el, value); else el.value = value;
}}
el.dispatchEvent(new Event('input', {{ bubbles: true }}));
el.dispatchEvent(new Event('change', {{ bubbles: true }}));
let submitted = false;
if ({submit}) {{
  const key = {{ key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true, cancelable: true }};
  const handled = !el.dispatchEvent(new KeyboardEvent('keydown', key));
  el.dispatchEvent(new KeyboardEvent('keyup', key));
  if (handled) {{
    submitted = true;
  }} else {{
    const form = el.form || (el.closest && el.closest('form'));
    if (form) {{ form.requestSubmit ? form.requestSubmit() : form.submit(); submitted = true; }}
  }}
}}
return S({{ ok: true, typed_into: describe(el), submitted: submitted }});
"##,
        target = lit(target),
        text = lit(text),
        submit = if submit { "true" } else { "false" }
    ))
}

fn scroll_js(to: &str, amount: Option<u64>) -> String {
    js(format!(
        r##"
const to = {to};
const px = {amount};
const step = px || Math.round(window.innerHeight * 0.8);
if (to === 'top') window.scrollTo({{ top: 0 }});
else if (to === 'bottom') window.scrollTo({{ top: document.documentElement.scrollHeight }});
else if (to === 'up') window.scrollBy({{ top: -step }});
else if (to === 'down' || !to) window.scrollBy({{ top: step }});
else {{
  const el = resolve(to);
  if (!el) return stale(to);
  el.scrollIntoView({{ block: 'center' }});
}}
await settle(150);
return S({{ ok: true, scroll: where() }});
"##,
        to = lit(to),
        amount = amount.map(|a| a.to_string()).unwrap_or_else(|| "null".into())
    ))
}

/// Wait until the DOM stops changing, or the limit runs out. A MutationObserver
/// rather than a fixed sleep: a search page is quiet for 800ms and then
/// suddenly is not, and a sleep long enough for that wastes it everywhere else.
fn stable_js(limit_ms: u64) -> String {
    js(format!(
        r##"
const limit = {limit};
const started = Date.now();
let last = Date.now();
let changes = 0;
const obs = new MutationObserver((records) => {{ changes += records.length; last = Date.now(); }});
obs.observe(document.documentElement, {{ childList: true, subtree: true, characterData: true, attributes: true }});
let settled = false;
while (Date.now() - started < limit) {{
  await settle(100);
  if (Date.now() - last > 400) {{ settled = true; break; }}
}}
obs.disconnect();
return S({{ ok: true, settled: settled, ms: Date.now() - started, changes: changes }});
"##,
        limit = limit_ms
    ))
}

/// The article on the page. The candidate list is the one every reader mode
/// starts from; the fallback scores blocks by how much of them is prose
/// rather than markup, which is what separates an article from a sidebar.
fn readable_js() -> String {
    js(r##"
const drop = ['script','style','noscript','nav','header','footer','aside','form','iframe','svg','button'];
const named = ['article','[role=main]','main','.post-content','.article-body','.entry-content','.article-content','#content','.content'];
let best = null, how = '';
for (const sel of named) {
  const el = document.querySelector(sel);
  if (el && vis(el) && (el.innerText || '').trim().length > 200) { best = el; how = sel; break; }
}
if (!best) {
  // Score every block: prose length against how much markup carries it.
  let top = 0;
  for (const el of document.querySelectorAll('div,section,td,li')) {
    const text = (el.innerText || '').trim();
    if (text.length < 200) continue;
    const density = text.length / Math.max(el.innerHTML.length, 1);
    const score = text.length * density;
    if (score > top) { top = score; best = el; how = 'densest block'; }
  }
}
if (!best) { best = document.body; how = 'whole page'; }
// Work on a copy so the page the user is looking at is not altered.
const copy = best.cloneNode(true);
for (const sel of drop) for (const el of copy.querySelectorAll(sel)) el.remove();
const text = (copy.innerText || '').replace(/\n{3,}/g, '\n\n').trim();
return S({
  url: location.href,
  title: document.title,
  source: how,
  text: text,
});
"##.to_string())
}

fn find_js(selector: &str) -> String {
    js(format!(
        r##"
let found;
try {{ found = document.querySelectorAll({sel}); }} catch (e) {{ return err('not a usable selector: ' + {sel}); }}
A.els = [];
const rows = [];
for (const el of found) {{
  if (rows.length >= 50) break;
  const n = A.els.push(el);
  const r = el.getBoundingClientRect();
  const row = {{ n: n, role: role(el), label: label(el) }};
  if (el.tagName === 'A' && el.href) row.href = String(el.href).slice(0, 200);
  if (typeof el.value === 'string' && el.value && el.type !== 'password') row.value = clean(el.value);
  row.onscreen = r.bottom > 0 && r.top < window.innerHeight && r.width > 0 && r.height > 0;
  rows.push(row);
}}
return S({{ count: found.length, elements: rows }});
"##,
        sel = lit(selector)
    ))
}

fn hover_js(target: &str) -> String {
    js(format!(
        r##"
const ref = {target};
const el = resolve(ref);
if (!el) return stale(ref);
el.scrollIntoView({{ block: 'center' }});
const r = el.getBoundingClientRect();
const at = {{ bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 }};
for (const kind of ['pointerover', 'mouseover', 'pointerenter', 'mouseenter', 'mousemove']) {{
  el.dispatchEvent(new MouseEvent(kind, at));
}}
if (typeof el.focus === 'function') el.focus({{ preventScroll: true }});
await settle(250);
return S({{ ok: true, hovered: describe(el) }});
"##,
        target = lit(target)
    ))
}

/// Scroll and gather. Stops early when the page stops growing, so a short
/// list does not cost the full budget.
fn collect_js(selector: &str, rounds: u64) -> String {
    js(format!(
        r##"
const sel = {sel};
const seen = new Set();
const items = [];
const take = () => {{
  let list;
  try {{ list = document.querySelectorAll(sel); }} catch (e) {{ return false; }}
  for (const el of list) {{
    const text = clean(el.innerText || el.textContent || '');
    if (!text) continue;
    const href = el.tagName === 'A' && el.href ? String(el.href) : (el.querySelector && el.querySelector('a[href]') ? el.querySelector('a[href]').href : '');
    const key = text + '|' + href;
    if (seen.has(key)) continue;
    seen.add(key);
    items.push(href ? text + ' -> ' + href : text);
  }}
  return true;
}};
if (!take()) return err('not a usable selector: ' + sel);
let reachedEnd = false;
for (let i = 0; i < {rounds}; i++) {{
  const before = document.documentElement.scrollHeight;
  const wasAt = window.scrollY;
  window.scrollBy({{ top: Math.round(window.innerHeight * 0.9) }});
  await settle(400);
  take();
  const atBottom = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 4;
  if (atBottom && document.documentElement.scrollHeight === before && window.scrollY === wasAt) {{ reachedEnd = true; break; }}
  if (atBottom) {{ await settle(600); take(); if (document.documentElement.scrollHeight === before) {{ reachedEnd = true; break; }} }}
}}
return S({{ items: items, reached_end: reachedEnd }});
"##,
        sel = lit(selector),
        rounds = rounds
    ))
}

fn eval_js(user: &str) -> String {
    js(format!(
        r##"
try {{
  const v = await (async () => {{
{user}
  }})();
  if (v === undefined) return S(null);
  const t = typeof v;
  if (v === null || t === 'string' || t === 'number' || t === 'boolean') return S(v);
  try {{ return S(JSON.parse(JSON.stringify(v))); }} catch (e) {{ return S(String(v)); }}
}} catch (e) {{
  return err(String((e && e.message) || e));
}}
"##,
        user = user
    ))
}

#[cfg(test)]
mod download_tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn test_ctx() -> ToolCtx {
        let dir = std::env::temp_dir().join(format!("solos-browser-{}", crate::agent::new_id()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        std::fs::create_dir_all(crate::sandbox::Sandbox::workspace_dir(&*sandbox)).unwrap();
        ToolCtx { session_id: "s".into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }
    }

    /// What comes back as text and what becomes a file. A server that labels
    /// JSON `application/octet-stream` is common enough that the content type
    /// alone cannot decide it.
    #[test]
    fn text_and_binary_are_told_apart() {
        assert!(looks_like_text("text/html; charset=utf-8", b"<html>"));
        assert!(looks_like_text("application/json", b"{}"));
        assert!(looks_like_text("application/octet-stream", b"{\"ok\":true}"));
        assert!(!looks_like_text("image/jpeg", b"\xff\xd8\xff\xe0"));
        assert!(!looks_like_text("application/pdf", b"%PDF-1.4"));
        assert!(!looks_like_text("", &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]));
    }

    #[tokio::test]
    async fn a_download_that_keeps_coming_is_not_cut_but_one_that_stops_is_named() {
        use std::io::{Read, Write};
        // Serves 8 bytes, pauses `gap`, serves 8 more (or never, if `stop`).
        fn serve(gap: Duration, stop: bool) -> u16 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut sock, _)) = listener.accept() {
                    let _ = sock.read(&mut [0u8; 1024]);
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n12345678");
                    let _ = sock.flush();
                    std::thread::sleep(gap);
                    if !stop {
                        let _ = sock.write_all(b"abcdefgh");
                    } else {
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            });
            port
        }
        crate::providers::install_crypto_provider();
        let client = reqwest::Client::new();

        // A pause shorter than the stall limit, a total longer than it: kept.
        let port = serve(Duration::from_millis(600), false);
        let r = client.get(format!("http://127.0.0.1:{port}/")).send().await.unwrap();
        let body = read_body(r, Duration::from_secs(1), Duration::from_secs(10)).await.unwrap();
        assert_eq!(body, b"12345678abcdefgh");

        // Nothing more after the first bytes: a stall, saying how much came.
        let port = serve(Duration::from_millis(0), true);
        let r = client.get(format!("http://127.0.0.1:{port}/")).send().await.unwrap();
        let err = read_body(r, Duration::from_secs(1), Duration::from_secs(10)).await.unwrap_err();
        assert!(err.contains("stalled") && err.contains("after 8 bytes"), "{err}");
    }

    /// The whole path, against a real socket: a server hands back a PNG,
    /// `fetch` writes it into the workspace and answers with its `solos://`
    /// address.
    #[tokio::test]
    async fn fetch_saves_a_binary_into_the_workspace() {
        use std::io::{Read, Write};
        let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = png.clone();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut scratch = [0u8; 1024];
                let _ = sock.read(&mut scratch);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(&body);
            }
        });

        let ctx = test_ctx();
        // `fetch` never reaches the platform — it is the host's own HTTP
        // client — so a bridge that refuses everything is enough here.
        struct NoBridge;
        impl BrowserBridge for NoBridge {
            fn call(&self, _op: &str, _input: Value) -> Value {
                json!({"error": "no browser in this test"})
            }
        }
        let tool = BrowserTool { bridge: Arc::new(NoBridge) };
        let out = tool
            .dispatch(&ctx,
                json!({"action": "fetch", "url": format!("http://127.0.0.1:{port}/pic/photo.png")}),
            )
            .await;
        assert!(!out.is_error, "{:?}", out.content);
        let said: String = out
            .content
            .iter()
            .filter_map(|c| match c {
                ToolResultContent::Text { text } => Some(text.as_str()),
            })
            .collect();
        assert!(said.contains("solos://ws/photo.png"), "no solos url: {said}");
        assert!(out.artifacts.iter().any(|a| a.contains("photo.png")), "not an artifact: {:?}", out.artifacts);

        let on_disk = crate::sandbox::Sandbox::workspace_dir(&*ctx.sandbox).join("photo.png");
        assert_eq!(std::fs::read(&on_disk).unwrap(), png, "the bytes changed on the way");
    }

    #[test]
    fn a_saved_file_gets_a_sensible_name() {
        assert_eq!(
            file_name_for("https://x.org/a/b/photo.jpg?w=320", "image/jpeg"),
            "photo.jpg"
        );
        // Nothing usable in the path: fall back to the type.
        assert_eq!(file_name_for("https://x.org/download?id=7", "image/png"), "download.png");
        assert_eq!(file_name_for("https://x.org/", ""), "download.bin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use std::sync::Mutex;

    /// A bridge that answers `eval` from a scripted queue and records what
    /// it was asked.
    struct FakeBrowser {
        calls: Mutex<Vec<(String, Value)>>,
        replies: Mutex<Vec<Value>>,
    }

    impl FakeBrowser {
        fn new(replies: Vec<Value>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(vec![]),
                replies: Mutex::new(replies.into_iter().rev().collect()),
            })
        }
        /// What the platform was handed for a given op — the only place a
        /// parameter that never left this module shows up as missing.
        fn sent(&self, op: &str) -> Vec<Value> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == op)
                .map(|(_, v)| v.clone())
                .collect()
        }
        fn js_sent(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(op, _)| op == "eval")
                .map(|(_, v)| v["js"].as_str().unwrap_or("").to_string())
                .collect()
        }
    }

    impl BrowserBridge for FakeBrowser {
        fn call(&self, op: &str, input: Value) -> Value {
            self.calls.lock().unwrap().push((op.into(), input));
            self.replies.lock().unwrap().pop().unwrap_or(json!({"error": "no scripted reply"}))
        }
    }

    /// `i` walked a `Vec<char>` while indexing a `String` by bytes. Any page
    /// with a non-ASCII character in it — every Chinese page — sliced the
    /// string off a char boundary and panicked, killing the whole tool task.
    /// What reached the user was the bare words "tool task failed".
    #[test]
    fn html_with_chinese_in_it_does_not_panic() {
        let text = html_to_text("<html><head><title>头</title></head><body><p>胡塞武装的新闻</p><div>中文</div></body></html>");
        assert!(text.contains("胡塞武装的新闻"), "{text}");
        assert!(!text.contains("<p"), "{text}");
        // A script full of Chinese is still skipped whole.
        let skipped = html_to_text("<p>前</p><script>var s = '中文脚本';</script><p>后</p>");
        assert!(skipped.contains("前") && skipped.contains("后"), "{skipped}");
        assert!(!skipped.contains("中文脚本"), "{skipped}");
        // Emoji and other multi-byte scalars too.
        assert!(html_to_text("<p>🚀 ok</p>").contains("🚀"));
    }

    /// The shape Google actually returns: ninety kilobytes of script and one
    /// sentence of prose, which reads like a refusal and is not one.
    #[test]
    fn a_javascript_shell_is_called_out_rather_than_read_as_an_empty_page() {
        let note = js_shell_note(true, 91_837, 34).expect("a 34-character page needs saying");
        assert!(note.contains("navigate"), "{note}");
        assert!(note.contains("not read this as the site being blocked"), "{note}");
        // A short page that is genuinely short is not worth a warning.
        assert!(js_shell_note(true, 800, 120).is_none());
        // Nor is a long article.
        assert!(js_shell_note(true, 91_837, 9_000).is_none());
        // Nor is JSON.
        assert!(js_shell_note(false, 60_000, 12).is_none());
    }

    /// A page snapshot as the page-side JS would return it.
    /// `read` hands back the article and says where it found it, instead of
    /// the whole page with its navigation and footer in the way.
    #[tokio::test]
    async fn read_returns_the_article_and_its_source() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({
            "url": "https://news.example/story",
            "title": "胡塞武装最新消息",
            "source": "article",
            "text": "第一段正文。\n\n第二段正文。",
        }).to_string()})]);
        let tool = BrowserTool { bridge: bridge.clone() };
        let out = tool.dispatch(&ctx(), json!({"action": "read"})).await;
        let text = text_of(&out);
        assert!(text.contains("第一段正文"), "{text}");
        assert!(text.contains("[article]"), "{text}");
    }

    /// `find` says how many matched even when it only lists some, and marks
    /// the ones that are not on screen.
    #[tokio::test]
    async fn find_reports_the_total_and_what_is_off_screen() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({
            "count": 42,
            "elements": [
                {"n": 1, "role": "link", "label": "第一条", "href": "https://a/1", "onscreen": true},
                {"n": 2, "role": "link", "label": "第二条", "href": "https://a/2", "onscreen": false},
            ],
        }).to_string()})]);
        let tool = BrowserTool { bridge: bridge.clone() };
        let out = tool.dispatch(&ctx(), json!({"action": "find", "selector": "a.item"})).await;
        let text = text_of(&out);
        assert!(text.contains("42 match(es)"), "{text}");
        assert!(text.contains("first 2 shown"), "{text}");
        assert!(text.contains("[off screen]"), "{text}");
    }

    /// `collect` says whether there is more below, because "17 items" means
    /// two different things depending on the answer.
    #[tokio::test]
    async fn collect_says_whether_it_reached_the_end() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({
            "items": ["一 -> https://a/1", "二 -> https://a/2"],
            "reached_end": false,
        }).to_string()})]);
        let tool = BrowserTool { bridge: bridge.clone() };
        let out = tool.dispatch(&ctx(), json!({"action": "collect", "selector": ".row"})).await;
        let text = text_of(&out);
        assert!(text.contains("2 item(s)"), "{text}");
        assert!(text.contains("there is more below"), "{text}");
        assert!(text.contains("1. 一 -> https://a/1"), "{text}");
    }

    /// The point of `wait` is to tell the difference between a page that
    /// settled and one that never did.
    #[tokio::test]
    async fn wait_distinguishes_quiet_from_still_changing() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({
            "ok": true, "settled": false, "ms": 5000, "changes": 312,
        }).to_string()})]);
        let tool = BrowserTool { bridge: bridge.clone() };
        let out = tool.dispatch(&ctx(), json!({"action": "wait"})).await;
        assert!(text_of(&out).contains("still changing"), "{}", text_of(&out));
    }

    /// Every action the schema advertises has to exist, or the model spends a
    /// round discovering it does not.
    #[tokio::test]
    async fn every_advertised_action_is_dispatched() {
        let tool = BrowserTool { bridge: FakeBrowser::new(vec![]) };
        let spec = tool.spec();
        let listed = spec.schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        for action in listed {
            let out = tool.dispatch(&ctx(), json!({"action": action})).await;
            let text = text_of(&out);
            assert!(!text.contains("unknown action"), "{action}: {text}");
        }
    }

    /// The companion to `every_advertised_action_is_dispatched`, and the hole
    /// it left: `full_page` sat in the schema for a commit while
    /// `screenshot()` never read it, so asking for the whole page quietly
    /// returned one screen, and `user_agent` / `viewport` / `cookies` existed
    /// only on the platform side. Dispatch cannot see either failure — only
    /// what the platform is actually handed can.
    #[tokio::test]
    async fn advertised_parameters_reach_the_platform() {
        let bridge = FakeBrowser::new(vec![
            json!({"base64": "iVBORw0KGgo=", "width": 390, "height": 3000}),
            json!({"width": 1024, "height": 768}),
            json!({"cookies": [{"name": "sid", "value": "x", "domain": ".example.com"}]}),
            json!({"user_agent": "Mozilla/5.0 (Macintosh)", "profile": "desktop"}),
        ]);
        let tool = tool(bridge.clone());
        let c = ctx();
        tool.dispatch(&c, json!({"action": "screenshot", "full_page": true})).await;
        tool.dispatch(&c, json!({"action": "viewport", "width": 1024, "height": 768})).await;
        tool.dispatch(&c, json!({"action": "cookies", "operation": "get", "domain": "example.com"})).await;
        tool.dispatch(&c, json!({"action": "user_agent", "profile": "desktop"})).await;

        assert_eq!(bridge.sent("screenshot")[0]["full_page"], json!(true));
        assert_eq!(bridge.sent("viewport")[0]["width"], json!(1024));
        assert_eq!(bridge.sent("viewport")[0]["height"], json!(768));
        assert_eq!(bridge.sent("cookies")[0]["domain"], json!("example.com"));
        assert_eq!(bridge.sent("user_agent")[0]["profile"], json!("desktop"));
    }

    #[tokio::test]
    async fn a_viewport_without_a_size_is_refused_before_the_browser_is_touched() {
        let bridge = FakeBrowser::new(vec![]);
        let out = tool(bridge.clone()).dispatch(&ctx(), json!({"action": "viewport"})).await;
        assert!(out.is_error);
        assert!(bridge.sent("viewport").is_empty());
    }

    /// Clicking a download link navigates nowhere and changes nothing on the
    /// page, so without this the model sees an unchanged snapshot and has no
    /// way to know a file is on its way.
    #[tokio::test]
    async fn a_download_a_click_started_is_reported_with_the_page() {
        let bridge = FakeBrowser::new(vec![
            json!({"value": json!({"ok": true, "clicked": "link \"annual report\""}).to_string()}),
            json!({"downloads": [
                {"id": "d1", "name": "report.pdf", "state": "finished", "bytes": 2_200_000,
                 "solos_url": "solos://ws/downloads/report.pdf"},
            ]}),
            json!({"value": json!({"ok": true, "settled": true}).to_string()}),
            page_reply("Reports"),
        ]);
        let out = tool(bridge).dispatch(&ctx(), json!({"action": "click", "ref": "1"})).await;
        let text = text_of(&out);
        assert!(text.contains("report.pdf downloaded (2.1 MB) → solos://ws/downloads/report.pdf"), "{text}");
        // The card still says what the action was; a download is a side effect.
    }

    /// Ending the turn but leaving the call running is not enough: the web view kept
    /// loading for up to forty-five seconds after Stop, and a click already
    /// handed over still landed — possibly under someone who had just taken
    /// the tab over. Stop has to reach the platform, not just the transcript.
    #[tokio::test]
    async fn stop_reaches_the_platform_instead_of_abandoning_the_call() {
        /// A bridge that answers nothing, the way a page that will not load
        /// looks from here.
        struct Stuck {
            ops: Mutex<Vec<String>>,
        }
        impl BrowserBridge for Stuck {
            fn call(&self, op: &str, _input: Value) -> Value {
                self.ops.lock().unwrap().push(op.to_string());
                if op == "cancel" {
                    return json!({"cancelled": true});
                }
                // Long enough to outlast the cancel, short enough that the
                // suite does not wait for it at shutdown.
                std::thread::sleep(Duration::from_secs(2));
                json!({"value": "{}"})
            }
        }

        let bridge = Arc::new(Stuck { ops: Mutex::new(vec![]) });
        let tool = BrowserTool { bridge: bridge.clone() };
        let mut ctx = ctx();
        ctx.cancel = CancellationToken::new();
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });

        let began = std::time::Instant::now();
        let out = CANCEL
            .scope(ctx.cancel.clone(), tool.dispatch(&ctx, json!({"action": "navigate", "url": "https://example.com"})))
            .await;
        let waited = began.elapsed();

        assert!(out.is_error, "a cancelled call is not a success");
        assert!(waited < Duration::from_secs(5), "cancel did not cut the wait: {waited:?}");
        // The platform has to be told, or the page keeps loading regardless.
        for _ in 0..50 {
            if bridge.ops.lock().unwrap().iter().any(|o| o == "cancel") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the platform was never told to let go: {:?}", bridge.ops.lock().unwrap());
    }

    #[tokio::test]
    async fn downloads_lists_what_is_still_running() {
        let bridge = FakeBrowser::new(vec![json!({"downloads": [
            {"id": "d1", "name": "big.zip", "state": "running", "bytes": 1024, "total": 8192},
            {"id": "d2", "name": "gone.pdf", "state": "failed", "error": "连接中断"},
        ]})]);
        let out = tool(bridge).dispatch(&ctx(), json!({"action": "downloads"})).await;
        let text = text_of(&out);
        assert!(text.contains("big.zip downloading (1.0 KB of 8.0 KB)"), "{text}");
        assert!(text.contains("gone.pdf failed: 连接中断"), "{text}");
    }

    fn page_reply(text: &str) -> Value {
        json!({"value": json!({
            "url": "https://example.com/",
            "title": "Example Domain",
            "text": text,
            "elements": [
                {"n": 1, "role": "link", "label": "More information...", "href": "https://iana.org/"},
                {"n": 2, "role": "textbox", "label": "Search", "value": "cats"},
            ],
            "more_elements": false,
            "scroll": {"y": 0, "viewport": 800, "height": 2400},
        }).to_string()})
    }

    fn ctx() -> ToolCtx {
        let dir = std::env::temp_dir().join(format!("solos-browser-{}", crate::agent::new_id()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        std::fs::create_dir_all(crate::sandbox::Sandbox::workspace_dir(&*sandbox)).unwrap();
        ToolCtx { session_id: "s".into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }
    }

    fn text_of(out: &ToolOutput) -> String {
        out.content
            .iter()
            .filter_map(|c| match c {
                ToolResultContent::Text { text } => Some(text.clone()),
            })
            .collect()
    }

    fn tool(bridge: Arc<FakeBrowser>) -> BrowserTool {
        BrowserTool { bridge }
    }

    #[tokio::test]
    async fn navigate_returns_the_page_it_landed_on() {
        let bridge = FakeBrowser::new(vec![
            json!({"url": "https://example.com/", "title": "Example Domain"}),
            page_reply("Example Domain\nThis domain is for use in examples."),
        ]);
        let out = tool(bridge.clone())
            .dispatch(&ctx(), json!({"card_title": "打开示例站", "action": "navigate", "url": "example.com"}))
            .await;
        assert!(!out.is_error);
        let text = text_of(&out);
        assert!(text.contains("https://example.com/"), "{text}");
        assert!(text.contains("This domain is for use in examples."), "{text}");
        // Opening a page is for reading it: the numbered controls stay out,
        // and the model is told where they are.
        assert!(!text.contains("[1] link"), "{text}");
        assert!(text.contains("`snapshot` lists them"), "{text}");
        assert!(text.contains("scroll for more"), "{text}");
        // A bare host is a URL the user means, not a relative path.
        assert_eq!(bridge.calls.lock().unwrap()[0].1["url"], "https://example.com");
    }

    /// A page that never stops loading is handed over as it stands, and the
    /// model is told so, rather than the call failing with the content
    /// already on screen (2026-09-25: every tweet on the phone).
    #[tokio::test]
    async fn a_page_still_loading_is_read_as_it_stands_and_says_so() {
        let bridge = FakeBrowser::new(vec![
            json!({"url": "https://x.com/a/status/1", "title": "a on X", "still_loading": true}),
            page_reply("a on X\nthe post itself"),
        ]);
        let out = tool(bridge)
            .dispatch(&ctx(), json!({"card_title": "打开推文", "action": "navigate", "url": "https://x.com/a/status/1"}))
            .await;
        assert!(!out.is_error);
        let text = text_of(&out);
        assert!(text.contains("the post itself"), "{text}");
        assert!(text.contains("still loading"), "{text}");
    }

    #[tokio::test]
    async fn a_click_waits_for_the_page_it_opened() {
        let bridge = FakeBrowser::new(vec![
            json!({"value": json!({"ok": true, "clicked": "link \"More information...\""}).to_string()}),
            json!({"url": "https://iana.org/", "title": "IANA"}),
            json!({"value": json!({"ok": true, "settled": true, "ms": 300, "changes": 4}).to_string()}),
            page_reply("Example Domains"),
        ]);
        let out = tool(bridge.clone())
            .dispatch(&ctx(), json!({"card_title": "点开链接", "action": "click", "ref": "1"}))
            .await;
        assert!(!out.is_error, "{}", text_of(&out));
        let ops: Vec<String> = bridge.calls.lock().unwrap().iter().map(|(op, _)| op.clone()).collect();
        // eval(click) → settle(load) → eval(wait for the DOM to go quiet) →
        // eval(snapshot). The third one is the difference between reading the
        // page and reading the page as it was a moment before it filled in.
        assert_eq!(
            ops,
            vec!["eval", "settle", "eval", "eval"],
            "the page must be read after it has loaded and stopped changing, not before"
        );
    }

    #[tokio::test]
    async fn a_stale_element_number_says_so_instead_of_clicking() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({"__err": "no element matches \"7\". Element numbers only last until the page changes, so take a snapshot and use a number from it."}).to_string()})]);
        let out = tool(bridge)
            .dispatch(&ctx(), json!({"card_title": "点击", "action": "click", "ref": "7"}))
            .await;
        assert!(out.is_error);
        assert!(text_of(&out).contains("take a snapshot"), "{}", text_of(&out));
    }

    #[tokio::test]
    async fn typing_reports_the_field_and_only_re_reads_on_submit() {
        let bridge = FakeBrowser::new(vec![json!({"value": json!({
            "ok": true, "typed_into": "textbox \"Search\"", "submitted": false
        }).to_string()})]);
        let out = tool(bridge.clone())
            .dispatch(&ctx(), json!({"card_title": "输入", "action": "type", "ref": "2", "text": "hello"}))
            .await;
        assert!(!out.is_error, "{}", text_of(&out));
        assert!(text_of(&out).contains("Typed into textbox \"Search\""));
        assert_eq!(bridge.js_sent().len(), 1, "no snapshot when nothing was submitted");
        let sent = &bridge.js_sent()[0];
        assert!(sent.contains(r#"const value = "hello""#), "{sent}");
        assert!(sent.contains("if (false)"), "submit must be off: {sent}");
    }

    #[tokio::test]
    async fn eval_returns_the_value_the_page_produced() {
        let bridge = FakeBrowser::new(vec![json!({"value": "{\"count\":3}"})]);
        let out = tool(bridge.clone())
            .dispatch(&ctx(), json!({"card_title": "数一数", "action": "eval", "js": "return {count: document.links.length}"}))
            .await;
        assert!(!out.is_error);
        assert!(text_of(&out).contains("\"count\": 3"));
        assert!(bridge.js_sent()[0].contains("return {count: document.links.length}"));
    }

    #[tokio::test]
    async fn a_long_page_is_spilled_to_a_file() {
        let long = "paragraph\n".repeat(2_000);
        let bridge = FakeBrowser::new(vec![page_reply(&long)]);
        let out = tool(bridge)
            .dispatch(&ctx(), json!({"card_title": "读页面", "action": "snapshot", "max_chars": 500}))
            .await;
        assert!(!out.is_error);
        assert_eq!(out.artifacts.len(), 1, "the full text must stay reachable");
        assert!(out.artifacts[0].starts_with("solos://ws/.solos/pages/page-"), "{:?}", out.artifacts);
        // The element list survives the cap: it is what the model acts on.
        assert!(text_of(&out).contains("[1] link"));
    }

    #[tokio::test]
    async fn a_screenshot_is_saved_into_the_workspace() {
        // 1×1 transparent PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let bridge = FakeBrowser::new(vec![json!({"base64": png, "width": 390, "height": 844})]);
        let out = tool(bridge).dispatch(&ctx(), json!({"card_title": "截图", "action": "screenshot"})).await;
        assert!(!out.is_error, "{}", text_of(&out));
        assert!(text_of(&out).contains("saved at solos://ws/.solos/screenshots/"), "{}", text_of(&out));
        assert_eq!(out.images.len(), 1, "the model is shown the picture");
        assert_eq!(out.artifacts.len(), 1, "the picture is kept for the user either way");
    }

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><title>x</title><style>p{color:red}</style></head>\
<body><h1>Hi</h1><script>var a = '<b>no</b>';</script><p>One &amp; two</p><p>Three</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hi"));
        assert!(text.contains("One & two"));
        assert!(text.contains("Three"));
        assert!(!text.contains("color:red"), "{text}");
        assert!(!text.contains("var a"), "{text}");
    }

    #[test]
    fn injected_arguments_cannot_break_out_of_the_snippet() {
        let nasty = "\"); alert('x'); //";
        let snippet = click_js(nasty);
        assert!(snippet.contains(r#"const ref = "\"); alert('x'); //""#), "{snippet}");
    }
}
