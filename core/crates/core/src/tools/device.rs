//! Device capabilities as first-class tools.
//!
//! The system's own permission prompts stand in front of every framework. On
//! top of them, the tools that read a person's own data (calendar, reminders,
//! contacts, location, photos, the clipboard, health) ask once whether the
//! assistant may, because what it reads goes to the model service the person
//! chose (`consent`). The reference app asks nothing; this is the App Store's
//! rule about sharing personal data with a third-party AI, not a measurement.
//!
//! The schemas live here so both platforms present the model an identical
//! surface, and so the harness can gate, render and audit each action by its
//! typed arguments. The platform supplies only the implementation, through
//! `DeviceBridge`.
//!
//! A capability the device does not have is never registered, rather than
//! registered and always failing: a tool the model can see is a tool it will
//! try.

use super::{object_schema, Registry, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::sandbox::GUEST_WORKSPACE;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

type ToolCtx = ToolContext;

fn schema(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    object_schema(Value::Object(props.into_iter().map(|(k, v)| (k.to_string(), v)).collect()), required)
}

/// What a platform must provide to expose its device capabilities.
///
/// Calls arrive on a core thread. Implementations that need the main thread
/// (most permission prompts do) must hop there themselves.
pub trait DeviceBridge: Send + Sync {
    /// Capability names this device actually supports. Anything not listed
    /// is not registered as a tool.
    fn capabilities(&self) -> Vec<String>;

    /// Run one call. `input` is the tool's arguments; the return value is
    /// rendered to the model as JSON. Failures come back as
    /// `{"error": "..."}`.
    ///
    /// A capability that produces files (a photo export, say) finds the host
    /// directory to write them into under `_dir` in its input, and names them
    /// back in `files`; the core turns those into `solos://` URLs the model
    /// and the sandbox can both use.
    fn call(&self, capability: &str, input: Value) -> Value;

    /// Ask the person whether the assistant may read this kind of data,
    /// naming the tool that wants to (`device_calendar`). Returns the answer,
    /// or `None` when the person cannot be asked just now (the app is not on
    /// screen). Called on a core thread that may block while the question is
    /// on screen.
    fn consent(&self, kind: solos_api::ConsentKind, capability: &str) -> Option<bool>;
}

/// Every capability the core knows how to describe. A platform picks the
/// subset it can serve.
pub fn all_specs() -> Vec<ToolSpec> {
    vec![
        calendar(),
        reminders(),
        contacts(),
        location(),
        photos(),
        clipboard(),
        notify(),
        alarm(),
        media(),
        health(),
        weather(),
        maps(),
        vision(),
        nlp(),
        open(),
        device_info(),
    ]
}

/// Register the tools this bridge can actually serve.
pub fn register(registry: &mut Registry, bridge: Arc<dyn DeviceBridge>) {
    let available = bridge.capabilities();
    for spec in all_specs() {
        if available.iter().any(|c| c == &spec.name) {
            registry.add(Arc::new(DeviceTool {
                spec,
                bridge: bridge.clone(),
                consents: registry.consents(),
            }));
        }
    }
}

struct DeviceTool {
    spec: ToolSpec,
    bridge: Arc<dyn DeviceBridge>,
    consents: Arc<crate::consent::Consents>,
}

#[async_trait]
impl Tool for DeviceTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn call(&self, ctx: &ToolCtx, input: &Value) -> ToolOutput {
        let bridge = self.bridge.clone();
        let name = self.spec.name.clone();
        if let Some(kind) = crate::consent::kind_of_tool(&name) {
            let (asker, tool) = (bridge.clone(), name.clone());
            match self.consents.allowed(kind, move || asker.consent(kind, &tool)).await {
                Some(true) => {}
                Some(false) => return ToolOutput::error(crate::consent::refusal(kind)),
                None => return ToolOutput::error(crate::consent::unasked(kind)),
            }
        }
        // Somewhere to put anything this call produces (an exported photo):
        // the workspace's attachments folder, which the guest sees as
        // /solos/ws/attachments, so a script can read it at once.
        let host_dir = ctx.sandbox.workspace_dir().join("attachments");
        let _ = std::fs::create_dir_all(&host_dir);
        let mut input = input.clone();
        // The file argument as the model wrote it, and where it is on the device.
        let mut file: Option<(Value, String)> = None;
        if let Some(object) = input.as_object_mut() {
            object.insert("_dir".into(), json!(host_dir.display().to_string()));
            // A file the call reads (`device_vision`'s image) is named the way
            // the model sees it; the platform side gets the file on the device.
            if let Some(path) = object.get("path").and_then(Value::as_str) {
                let Some(host) = crate::files::resolve_argument(path, &ctx.sandbox.workspace_dir()) else {
                    return ToolOutput::error(format!(
                        "{path} is not inside the workspace ({GUEST_WORKSPACE}). Copy it there with `shell` first."
                    ));
                };
                let host = host.display().to_string();
                file = Some((json!(path), host.clone()));
                object.insert("path".into(), json!(host));
            }
        }
        // The platform side talks to system frameworks and may wait on a
        // permission prompt, so it does not belong on a runtime thread. A
        // stopped turn stops waiting for it at once; the platform's own wait
        // ends later, and its answer is dropped.
        let call = tokio::task::spawn_blocking(move || bridge.call(&name, input));
        let value = tokio::select! {
            joined = call => match joined {
                Ok(v) => v,
                Err(e) => return ToolOutput::error(format!("{} failed to run: {e}", self.spec.name)),
            },
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Stopped by the user."),
        };
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return ToolOutput::error(crate::consent::system_refusal(error).unwrap_or_else(|| error.to_string()));
        }
        // Files the call wrote (an exported photo), by the address the user
        // can open and a script can pass on, inside the result itself so it
        // stays one JSON document.
        let mut value = value;
        // A result that names the file names it as the model did, not by
        // where it is on the device.
        if let (Some((given, host)), Some(object)) = (&file, value.as_object_mut()) {
            if object.get("path").and_then(Value::as_str) == Some(host.as_str()) {
                object.insert("path".into(), given.clone());
            }
        }
        let saved: Vec<Value> = value
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|name| crate::files::url_for(&format!("{GUEST_WORKSPACE}/attachments/{name}")))
            .map(Value::from)
            .collect();
        if let (false, Some(object)) = (saved.is_empty(), value.as_object_mut()) {
            object.insert("saved".into(), Value::from(saved));
        }
        ToolOutput::ok(serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn spec(name: &str, description: &str, schema: Value, parallel: bool, _confirm: Permission) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.into(), schema, parallel }
}

/// Kept on each spec to record which calls would warrant a confirmation;
/// nothing asks yet (see the module comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Permission {
    Allow,
    Ask,
}

/// Dates are ISO 8601, or one of a few words the user would actually say.
const WHEN: &str = "ISO 8601 (2026-09-17T14:00:00+08:00), or a plainer form: today, tomorrow, `tomorrow 09:00`, `today 2pm`, +3d, +2h.";

fn calendar() -> ToolSpec {
    spec(
        "device_calendar",
        "Read and write the user's calendar. `list` returns events in a window; `create` adds one; `delete` removes one by id. Times are in the device's timezone unless an offset is given.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["list", "create", "delete"]})),
                ("from", json!({"type": "string", "description": format!("Start of the window for list. {WHEN}")})),
                ("to", json!({"type": "string", "description": format!("End of the window for list. {WHEN}")})),
                ("name", json!({"type": "string", "description": "What the event is called, for create."})),
                ("start", json!({"type": "string", "description": format!("Event start, for create. {WHEN}")})),
                ("end", json!({"type": "string", "description": format!("Event end, for create. Defaults to one hour after start. {WHEN}")})),
                ("location", json!({"type": "string"})),
                ("notes", json!({"type": "string"})),
                ("all_day", json!({"type": "boolean"})),
                ("id", json!({"type": "string", "description": "Event identifier, for delete."})),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

fn reminders() -> ToolSpec {
    spec(
        "device_reminders",
        "Read and write the user's reminders. `list` returns open items (or completed ones with completed: true); `create` adds one with an optional due date; `complete` marks one done by id.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["list", "create", "complete"]})),
                ("name", json!({"type": "string", "description": "What to do, for create."})),
                ("due", json!({"type": "string", "description": format!("When it is due, for create. {WHEN}")})),
                ("notes", json!({"type": "string"})),
                ("completed", json!({"type": "boolean", "description": "For list: return completed items instead of open ones."})),
                ("id", json!({"type": "string", "description": "Reminder identifier, for complete."})),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

fn contacts() -> ToolSpec {
    spec(
        "device_contacts",
        "Search the user's contacts by name. Returns names with phone numbers and email addresses. Read-only.",
        schema(
            vec![
                ("query", json!({"type": "string", "description": "Part of a name to match."})),
                ("limit", json!({"type": "integer", "description": "Maximum results (default 10)."})),
            ],
            &["query"],
        ),
        true,
        Permission::Allow,
    )
}

fn location() -> ToolSpec {
    spec(
        "device_location",
        "The device's current location: latitude, longitude, accuracy, and a placemark when one can be resolved. Asks the user the first time.",
        schema(vec![], &[]),
        true,
        Permission::Ask,
    )
}

fn photos() -> ToolSpec {
    spec(
        "device_photos",
        "The user's photo library. `list_recent` returns the newest photos with their ids, dates and sizes; `export` writes chosen ones into this session's files so a script can work on them, and returns the paths. Read-only: nothing is added to or removed from the library.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["list_recent", "export"]})),
                ("limit", json!({"type": "integer", "description": "How many to list (default 20, max 200)."})),
                ("since", json!({"type": "string", "description": format!("Only photos taken after this. {WHEN}")})),
                ("ids", json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Which photos to export, by the id `list_recent` gave."
                })),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

fn clipboard() -> ToolSpec {
    spec(
        "device_clipboard",
        "Read or write the system clipboard. `read` returns its text; `write` replaces it.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["read", "write"]})),
                ("text", json!({"type": "string", "description": "What to put on the clipboard, for write."})),
            ],
            &["action"],
        ),
        true,
        Permission::Allow,
    )
}

/// Alarms and countdown timers, through AlarmKit (iOS 26+).
///
/// Two things about this are not obvious, and both cost the reference
/// implementation a screen to learn:
///
/// - **An AlarmKit alarm does not appear in Apple's Clock app.** Only the app
///   that scheduled it can show it. So the answer to "where did my alarm go"
///   has to be a screen in *this* app — which is why the session list grows an
///   alarm button while any are pending, and why the description below tells
///   the model to say so.
/// - It is not a notification. `device_notify` posts a banner that a silenced
///   phone will not sound for; an alarm rings through the silent switch and
///   takes over the screen. Asking for the wrong one is the difference between
///   waking up and not.
fn alarm() -> ToolSpec {
    spec(
        "device_alarm",
        "Set alarms and countdown timers that ring even when the phone is silenced. `set` takes a time of day, `timer` takes a duration, `list` shows what is pending, `cancel` removes one by id or all of them. These do NOT appear in the Clock app — they are only visible in this app, on the alarm button that appears in the session list — so when you set one, tell the user that is where to find it. For a quiet reminder that does not ring, use `device_reminders`; for a note that something finished, `device_notify`.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["set", "timer", "list", "cancel"]})),
                ("time", json!({"type": "string", "description": format!("When it should ring, for set. `07:30` means the next time it is 07:30. {WHEN}")})),
                ("duration", json!({"type": "string", "description": "How long to count down, for timer: seconds, or a shorthand like `5m`, `1h30m`."})),
                ("label", json!({"type": "string", "description": "What it is for. This is what the alert says when it rings, so write it for someone half asleep."})),
                ("repeat", json!({"type": "string", "enum": ["none", "daily", "weekdays"], "description": "For set. Default none."})),
                ("id", json!({"type": "string", "description": "Which one to cancel."})),
                ("all", json!({"type": "boolean", "description": "For cancel: remove every pending alarm and timer."})),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

/// The phone's own music, through MediaPlayer.
///
/// No screen of ours is needed: what this changes is already visible in
/// Control Centre and on the Lock Screen, which is where a person looks for
/// "what is playing" anyway: the result already has a place to be seen
/// without us building anything.
///
/// Deliberately not here: playing a *file* from the workspace. A model that
/// wants the user to hear something writes `![clip](solos://ws/clip.m4a)` and
/// it plays inline in the conversation — in the place the request
/// was made, with no session id to keep track of.
fn media() -> ToolSpec {
    spec(
        "device_media",
        "Control what the phone is playing: `now_playing` says what is on, `play` / `pause` / `toggle` / `next` / `previous` move it, `volume` reads or sets the level, `search` looks through the music library, `play_search` plays the first match. This drives the system player, so the result shows up in Control Centre and on the Lock Screen. To play a file from the workspace instead, put it in your reply as `![name](solos://ws/file.m4a)` and it plays there.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["now_playing", "play", "pause", "toggle", "next", "previous", "volume", "search", "play_search"]})),
                ("query", json!({"type": "string", "description": "What to look for, for search / play_search."})),
                ("type", json!({"type": "string", "enum": ["song", "album", "artist", "playlist"], "description": "What kind of thing to search for. Default song."})),
                ("limit", json!({"type": "number", "description": "How many results at most. Default 20."})),
                ("level", json!({"type": "number", "description": "For volume: 0.0–1.0. Omit to read the current level instead of setting it."})),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

/// The Health store, through HealthKit.
///
/// The reference implementation exposes one subcommand per metric — 27 of
/// them. That does not fit a typed schema, and it does not need to: two of its
/// own subcommands, `types` and `batch`, already say how to collapse it.
/// `list_types` is discovery and `read` takes several types at once, so the
/// model finds the right identifier and then asks for it, instead of us
/// enumerating HealthKit in an enum that goes stale every September.
fn health() -> ToolSpec {
    spec(
        "device_health",
        "Read and write the user's Health data. Start with `list_types` to find the identifier for what is being asked about — the names are HealthKit's own and guessing them wastes a call. `read` takes one or more of those types over a date range, bucketed by day when it makes sense. `log` writes a sample. `delete` removes only samples this app wrote.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["list_types", "read", "log", "delete"]})),
                ("types", json!({"type": "array", "items": {"type": "string"}, "description": "HealthKit identifiers, e.g. `stepCount`, `heartRate`, `sleepAnalysis`, `bodyMass`. From list_types."})),
                ("from", json!({"type": "string", "description": format!("Start of the range. {WHEN}")})),
                ("to", json!({"type": "string", "description": format!("End of the range. Now if omitted. {WHEN}")})),
                ("bucket", json!({"type": "string", "enum": ["day", "none"], "description": "`day` sums or averages per day (what a week of steps should be); `none` returns raw samples. Default day for cumulative types."})),
                ("value", json!({"type": "number", "description": "What to write, for log."})),
                ("unit", json!({"type": "string", "description": "Unit of `value`, e.g. `kg`, `count`, `ml`. list_types names the one each type expects."})),
                ("at", json!({"type": "string", "description": format!("When the logged sample happened. Now if omitted. {WHEN}")})),
                ("id", json!({"type": "string", "description": "Sample to delete."})),
            ],
            &["action"],
        ),
        true,
        Permission::Ask,
    )
}

/// Weather, through WeatherKit.
///
/// Shaped after the reference implementation's `apple-weather`, which a
/// side-by-side test showed answering "what's the weather" far better with
/// the same model: with nothing specified it reports everything for
/// where the device is, so the plain question gets the forecast too, and the
/// field notes from its help text sit where the model reads them. Each note
/// is there because it was got wrong once: a probability read as an amount,
/// and an absent minute-by-minute forecast read as "no rain".
fn weather() -> ToolSpec {
    spec(
        "device_weather",
        "Weather through WeatherKit. With no arguments it returns `report` for where the device is: current conditions, the hourly and daily forecast, and severe-weather alerts in one call — which is what a plain \"what's the weather\" wants, so narrow `action` only when the question is narrower. `current` is now, `hourly` and `daily` are forecasts, `minute` is minute-by-minute precipitation for the next hour (not part of `report`), `alerts` is warnings only. Leave `lat`/`lon` out for the device's own location (the result then carries `place`); give both for anywhere else, e.g. from `device_maps` search. Field notes: `precip_chance` is a PROBABILITY (0-1), not an amount — how much rain is `precip_amount_mm`, per forecast interval. `wind_gust_kmh` is the peak gust and is absent when WeatherKit has no gust data. `minute` and `alerts` come with `minute_availability` / `alerts_availability`, WeatherKit's own verdict for this location (`available`, `unsupported`, `temporarily_unavailable`, `unknown`). A null `minute` or `alerts` is not \"no rain\" or \"no warnings\" — say what the verdict says. An empty `alerts` list with `available` does mean no warnings. Times are ISO 8601 in the device's time zone.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["report", "current", "hourly", "daily", "minute", "alerts"], "description": "Default `report`."})),
                ("lat", json!({"type": "number", "description": "Latitude. Leave out, together with `lon`, for the device's location."})),
                ("lon", json!({"type": "number", "description": "Longitude."})),
                ("hours", json!({"type": "number", "description": "How many hours of hourly forecast. Default 12, at most 48."})),
                ("days", json!({"type": "number", "description": "How many days of daily forecast. Default 7, at most 10."})),
            ],
            &[],
        ),
        true,
        Permission::Allow,
    )
}

/// Places, routes and travel time, through MapKit.
///
/// A pure question: the answer is the whole of it, so there is nothing to
/// show on a screen afterwards and nothing to undo.
fn maps() -> ToolSpec {
    spec(
        "device_maps",
        "Search for places, get directions, or estimate travel time. `search` without a centre searches the whole world, which is what a named landmark wants; give `lat`/`lon` only when the question is \"near me\", and get them from `device_location`. `route` returns the steps; `eta` returns only the duration and is much cheaper, so prefer it when the question is \"how long does it take\".",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["search", "route", "eta"]})),
                ("query", json!({"type": "string", "description": "What to look for, for search."})),
                ("lat", json!({"type": "number", "description": "Optional centre latitude for search; leave both out to search globally."})),
                ("lon", json!({"type": "number", "description": "Centre longitude for search."})),
                ("radius", json!({"type": "number", "description": "Search radius in metres. Default 1000."})),
                ("limit", json!({"type": "number", "description": "How many results at most. Default 10."})),
                ("from", json!({"type": "string", "description": "Origin for route/eta: an address, or `lat,lon`."})),
                ("to", json!({"type": "string", "description": "Destination for route/eta: an address, or `lat,lon`."})),
                ("mode", json!({"type": "string", "enum": ["driving", "walking", "transit"], "description": "Default driving."})),
            ],
            &["action"],
        ),
        true,
        Permission::Allow,
    )
}

/// What is in a picture, through the Vision framework.
///
/// Runs on the device and costs no tokens, which is the point: reading a
/// screenshot with `ocr` is far cheaper than sending the image to a model, and
/// works on a model that cannot see images at all.
fn vision() -> ToolSpec {
    spec(
        "device_vision",
        "Read and analyse an image on the device. `ocr` pulls out text, `barcode` reads QR and bar codes, `classify` says what the picture is of, `faces` finds faces, `analyze` does the first three at once. This runs locally and costs no tokens, so prefer it over looking at the image yourself when the question is about text or codes in the picture.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["ocr", "barcode", "classify", "faces", "analyze"]})),
                ("path", json!({"type": "string", "description": "The image: an absolute guest path, or the `solos://` URL a tool reported."})),
                ("lang", json!({"type": "string", "description": "OCR languages, comma separated, e.g. `zh-Hans,en`. Guessed when omitted."})),
                ("level", json!({"type": "string", "enum": ["fast", "accurate"], "description": "OCR effort. Default accurate."})),
                ("limit", json!({"type": "number", "description": "How many classifications at most."})),
            ],
            &["action", "path"],
        ),
        true,
        Permission::Allow,
    )
}

/// Language work that does not need a model, through NaturalLanguage.
fn nlp() -> ToolSpec {
    spec(
        "device_nlp",
        "Analyse text on the device: `language` detects which language it is, `tokenize` splits it into words or sentences, `ner` pulls out names, places and organisations, `sentiment` scores how positive it is, `analyze` runs them all. Local and free — use it for bulk or mechanical text work instead of spending a model turn on it.",
        schema(
            vec![
                ("action", json!({"type": "string", "enum": ["language", "tokenize", "ner", "sentiment", "analyze"]})),
                ("text", json!({"type": "string", "description": "The text to look at."})),
                ("unit", json!({"type": "string", "enum": ["word", "sentence", "paragraph"], "description": "For tokenize. Default word."})),
                ("per_sentence", json!({"type": "boolean", "description": "For sentiment: score each sentence separately instead of the whole."})),
            ],
            &["action", "text"],
        ),
        true,
        Permission::Allow,
    )
}

fn notify() -> ToolSpec {
    spec(
        "device_notify",
        "Post a local notification, now or at a time. Use it to tell the user something finished while they were elsewhere, not to narrate progress.",
        schema(
            vec![
                ("heading", json!({"type": "string", "description": "The notification's first line."})),
                ("body", json!({"type": "string"})),
                ("at", json!({"type": "string", "description": format!("When to deliver it. Immediate if omitted. {WHEN}")})),
            ],
            &["heading"],
        ),
        true,
        Permission::Allow,
    )
}

fn open() -> ToolSpec {
    spec(
        "device_open",
        "Open a URL in whichever app handles it, leaving Solos in the background. Hands control to the user, so only do it when they asked.",
        schema(
            vec![("url", json!({"type": "string", "description": "http(s):// or an app scheme."}))],
            &["url"],
        ),
        false,
        Permission::Ask,
    )
}

fn device_info() -> ToolSpec {
    spec(
        "device_info",
        "Facts about the device and its situation: model, system version, battery level and charging state, network reachability, locale, timezone and the current local time. Use it instead of guessing what `date` in the sandbox reports.",
        schema(vec![], &[]),
        true,
        Permission::Allow,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct FakeBridge {
        available: Vec<String>,
        calls: Mutex<Vec<(String, Value)>>,
        reply: Value,
        /// What the person answers when asked (`None`: could not be asked), and what was asked.
        answer: Option<bool>,
        asked: Mutex<Vec<(solos_api::ConsentKind, String)>>,
    }

    impl DeviceBridge for FakeBridge {
        fn capabilities(&self) -> Vec<String> {
            self.available.clone()
        }
        fn call(&self, capability: &str, input: Value) -> Value {
            self.calls.lock().unwrap().push((capability.into(), input));
            self.reply.clone()
        }
        fn consent(&self, kind: solos_api::ConsentKind, capability: &str) -> Option<bool> {
            self.asked.lock().unwrap().push((kind, capability.into()));
            self.answer
        }
    }

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("solos-device-{}", uuid::Uuid::new_v4()));
        let sandbox = Arc::new(HostSandbox::new(dir));
        std::fs::create_dir_all(crate::sandbox::Sandbox::workspace_dir(&*sandbox)).unwrap();
        ToolContext { session_id: "s".into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }
    }

    fn bridge(available: &[&str], reply: Value) -> Arc<FakeBridge> {
        Arc::new(FakeBridge {
            available: available.iter().map(|s| s.to_string()).collect(),
            calls: Mutex::new(vec![]),
            reply,
            answer: Some(true),
            asked: Mutex::new(vec![]),
        })
    }

    fn bridge_saying(answer: Option<bool>, available: &[&str]) -> Arc<FakeBridge> {
        Arc::new(FakeBridge {
            available: available.iter().map(|s| s.to_string()).collect(),
            calls: Mutex::new(vec![]),
            reply: json!({"ok": true}),
            answer,
            asked: Mutex::new(vec![]),
        })
    }

    #[test]
    fn only_supported_capabilities_are_registered() {
        let mut registry = Registry::new();
        register(&mut registry, bridge(&["device_clipboard", "device_info"], json!({"ok": true})));
        assert_eq!(registry.specs().len(), 2);
        assert!(registry.get("device_clipboard").is_some());
        assert!(registry.get("device_calendar").is_none(), "a device without it must not advertise it");
    }

    #[test]
    fn every_capability_asks_for_a_title_and_weather_for_nothing_else() {
        for spec in all_specs() {
            assert!(spec.schema["properties"]["title"].is_object(), "{}", spec.name);
        }
        // "What's the weather" is one call.
        let weather = all_specs().into_iter().find(|s| s.name == "device_weather").unwrap();
        assert_eq!(weather.schema["required"], json!(["title"]));
        // Opening a URL hands the screen to another app.
        assert!(!all_specs().into_iter().find(|s| s.name == "device_open").unwrap().parallel);
    }

    #[tokio::test]
    async fn exported_files_are_named_by_their_workspace_address() {
        let b = bridge(&["device_photos"], json!({"files": ["IMG_0001.jpg"], "items": [{}]}));
        let mut registry = Registry::new();
        register(&mut registry, b.clone());
        let out = registry.get("device_photos").unwrap().call(&ctx(), &json!({"action": "export", "ids": ["a"]})).await;
        assert!(!out.is_error, "{}", out.text);
        let result: Value = serde_json::from_str(&out.text).expect("the result stays one JSON document");
        assert_eq!(result["saved"], json!(["solos://ws/attachments/IMG_0001.jpg"]));
        let sent = &b.calls.lock().unwrap()[0].1;
        assert!(sent["_dir"].as_str().unwrap().ends_with("attachments"), "the platform is told where to write");
    }

    #[tokio::test]
    async fn a_platform_error_is_an_error() {
        let b = bridge(&["device_calendar"], json!({"error": "calendar access denied"}));
        let mut registry = Registry::new();
        register(&mut registry, b);
        let out = registry.get("device_calendar").unwrap().call(&ctx(), &json!({"action": "list"})).await;
        assert!(out.is_error && out.text == "calendar access denied", "a failure that is not a refusal keeps its words");
    }

    #[tokio::test]
    async fn a_refusal_from_the_system_is_told_to_the_model_as_final() {
        let b = bridge(&["device_calendar"], json!({"error": "Access to calendar is denied; it can be allowed in Settings."}));
        let mut registry = Registry::new();
        register(&mut registry, b);
        let out = registry.get("device_calendar").unwrap().call(&ctx(), &json!({"action": "list"})).await;
        assert!(out.is_error && out.text.contains("Do not try again") && out.text.contains("Settings > Solos"), "{}", out.text);
    }

    #[tokio::test]
    async fn a_tool_that_reads_the_persons_data_asks_first_and_a_no_stops_it() {
        use solos_api::ConsentKind;
        let yes = bridge_saying(Some(true), &["device_calendar", "device_health", "device_weather"]);
        let mut registry = Registry::new();
        register(&mut registry, yes.clone());
        let c = ctx();
        let cal = registry.get("device_calendar").unwrap();
        assert!(!cal.call(&c, &json!({"action": "list"})).await.is_error);
        assert!(!cal.call(&c, &json!({"action": "list"})).await.is_error);
        assert_eq!(*yes.asked.lock().unwrap(), vec![(ConsentKind::Personal, "device_calendar".to_string())], "asked once, naming the tool");
        // Health is a question of its own.
        assert!(!registry.get("device_health").unwrap().call(&c, &json!({"action": "read"})).await.is_error);
        assert_eq!(yes.asked.lock().unwrap().len(), 2);
        // Weather reads nothing of the person's: no question.
        assert!(!registry.get("device_weather").unwrap().call(&c, &json!({})).await.is_error);
        assert_eq!(yes.asked.lock().unwrap().len(), 2);

        let no = bridge_saying(Some(false), &["device_contacts"]);
        let mut registry = Registry::new();
        register(&mut registry, no.clone());
        let contacts = registry.get("device_contacts").unwrap();
        let out = contacts.call(&c, &json!({"action": "search", "query": "a"})).await;
        assert!(out.is_error && out.text.contains("Do not try again") && out.text.contains("Settings > Privacy"), "{}", out.text);
        assert!(no.calls.lock().unwrap().is_empty(), "the platform was never called");
        let again = contacts.call(&c, &json!({"action": "search", "query": "a"})).await;
        assert!(again.is_error);
        assert_eq!(no.asked.lock().unwrap().len(), 1, "a no is not asked again");
        // Changed in Settings, the same tool now works.
        registry.consents().set(ConsentKind::Personal, Some(true));
        assert!(!contacts.call(&c, &json!({"action": "search", "query": "a"})).await.is_error);

        // The person cannot be asked (the app is in the background): the call
        // is refused, with its own words, and nothing is kept.
        let away = bridge_saying(None, &["device_photos"]);
        let mut registry = Registry::new();
        register(&mut registry, away.clone());
        let out = registry.get("device_photos").unwrap().call(&c, &json!({"action": "list"})).await;
        assert!(out.is_error && out.text.contains("could not be asked just now"), "{}", out.text);
        assert_eq!(registry.consents().get(), solos_api::Consents::default());
        assert!(away.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_image_path_reaches_the_platform_as_the_file_on_the_device() {
        let b = bridge(&["device_vision"], json!({"text": ""}));
        let mut registry = Registry::new();
        register(&mut registry, b.clone());
        let c = ctx();
        let ws = c.sandbox.workspace_dir();
        let tool = registry.get("device_vision").unwrap();
        for path in ["solos://ws/shot.png", "/solos/ws/shot.png", "shot.png"] {
            let out = tool.call(&c, &json!({"action": "ocr", "path": path})).await;
            assert!(!out.is_error, "{path}: {}", out.text);
            assert_eq!(b.calls.lock().unwrap().last().unwrap().1["path"], json!(ws.join("shot.png").display().to_string()));
        }
        let out = tool.call(&c, &json!({"action": "ocr", "path": "/etc/hosts"})).await;
        assert!(out.is_error, "outside the workspace is refused before the platform sees it");
        assert_eq!(b.calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_result_names_the_file_as_the_model_did_not_by_its_place_on_the_device() {
        let c = ctx();
        let host = c.sandbox.workspace_dir().join("shot.png").display().to_string();
        let b = bridge(&["device_vision"], json!({"path": host, "lines": []}));
        let mut registry = Registry::new();
        register(&mut registry, b);
        let out = registry.get("device_vision").unwrap().call(&c, &json!({"action": "ocr", "path": "/solos/ws/shot.png"})).await;
        let result: Value = serde_json::from_str(&out.text).unwrap();
        assert_eq!(result["path"], json!("/solos/ws/shot.png"));
    }

    /// A permission prompt can be left open; stopping the turn must not wait for it.
    #[tokio::test]
    async fn stopping_the_turn_stops_waiting_for_the_device() {
        struct Stuck;
        impl DeviceBridge for Stuck {
            fn capabilities(&self) -> Vec<String> {
                vec!["device_contacts".into()]
            }
            fn call(&self, _: &str, _: Value) -> Value {
                std::thread::sleep(std::time::Duration::from_secs(5));
                json!({})
            }
            fn consent(&self, _: solos_api::ConsentKind, _: &str) -> Option<bool> {
                Some(true)
            }
        }
        let mut registry = Registry::new();
        register(&mut registry, Arc::new(Stuck));
        let c = ctx();
        let cancel = c.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let started = std::time::Instant::now();
        let out = registry.get("device_contacts").unwrap().call(&c, &json!({"query": "x"})).await;
        assert!(out.is_error && out.text.contains("Stopped"), "{}", out.text);
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    }
}
