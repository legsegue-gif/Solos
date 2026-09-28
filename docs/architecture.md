# Solos — Architecture

This document describes the system as it is meant to be. It is rewritten
when the design changes; it is not a changelog. Decisions and their reasons
live in `decisions.md`, one short entry each.

Requirements are in `requirements.md`. Everything below serves them.

---

## 1. Principles

1. **Behaviour is verified, not assumed.** A change is done when it has been
   run on the target (simulator or device) and, where the reference app has
   the same feature, compared side by side with it. Compiling is not done.
2. **The reference app sets the floor, not the design.** The reference app defines what
   the user can do and how well; how Solos does it is designed here. Where
   Solos behaves differently on purpose, the difference is measured first.
3. **One source of truth per fact.** State lives in the core's store; the UI
   renders it. Anything derived is re-derivable from the store at any time.
4. **Nothing blocks a caller it does not own.** No synchronous waits across
   the language boundary, no blocking work on async executors.
5. **Failures are values.** An error is reported to whoever can act on it —
   never swallowed, never turned into a timeout somewhere else.
6. **The model sees less, and better.** Tool descriptions steer; tool results
   are shaped for the question that was asked. Both are tested against a
   real model, like code.
7. **Open source from the first commit.** No personal data in the tree,
   builds reproducible from a clean clone, English in code and docs, every
   user-visible string localised.

---

## 2. Overview

```
┌──────────────────────── iOS app (Swift) ─────────────────────────┐
│ UI (SwiftUI)          Platform services                          │
│  Chat · Sessions ·     Device (EventKit, CoreLocation, ...)       │
│  Terminal · Files ·    Browser (WKWebView)                        │
│  Browser · Settings    Secrets (Keychain) · Notifications         │
│        │  ViewState ← reducer(events)     ▲ callbacks (async)     │
└────────┼─────────────────────────────────┼───────────────────────┘
         │ commands (async) / event stream │  UniFFI
┌────────▼─────────────────────────────────┴───────────────────────┐
│ Core (Rust, platform-neutral)                                     │
│  api        commands, events, snapshots (versioned, serde)        │
│  engine     sessions, turns, queueing, lifecycle                  │
│  agent      the turn loop: stream → tools → commit → repeat       │
│  providers  OpenAI-compatible · Anthropic · Gemini adapters       │
│  tools      registry; shell, files, browser, device, ...          │
│  context    token budgets, summaries                              │
│  store      SQLite, migrations, the only writer                   │
│  sandbox    trait; host impl for tests                            │
│  bridge     loopback HTTP API for the guest CLI                   │
└────────┬─────────────────────────────────────────────────────────┘
         │ Sandbox trait
┌────────▼──────────────┐     ┌───────────────────────────────────┐
│ sandbox-ish (Rust+C)  │     │ Guest: Alpine Linux (aarch64)     │
│ iSH kernel, in process│────▶│ /solos/… workspace, `solos` CLI    │
└───────────────────────┘     └───────────────────────────────────┘
```

The core owns every rule that is not about pixels or an OS API. The app
owns the screen and the operating system. They talk through one narrow,
versioned API.

---

## 3. Core ↔ app boundary

### 3.1 Shape

- **Commands** go in as typed calls through UniFFI **async** functions. A
  command returns promptly: it records intent and starts work; it does not
  wait for a turn to finish. Swift awaits it on a background task.
- **Events** come out as one ordered stream. Every event carries a
  monotonically increasing `seq` for its session.
- **Snapshots** are the full current state of a session (messages, running
  turn, pending tools). `session.snapshot` is a command like any other.
- **Platform services** (device, browser, secrets, notifications) are
  foreign traits implemented in Swift. Their methods are `async` on the Rust
  side; the core never parks an executor thread waiting on them.

There is no `block_on` anywhere on the boundary.

### 3.2 Keeping the UI correct

The UI state for a session is `reduce(snapshot, events)` — a pure function,
unit-tested without a simulator.

- The client applies events in `seq` order.
- If it sees a gap, or connects mid-turn, or returns from background, it
  asks for a snapshot and replaces its state. That is the **only** recovery
  path; there are no special cases for "missed end of turn", "cancel said
  idle", and so on.
- The event channel may drop under pressure; correctness never depends on
  it not dropping.

### 3.3 Versioning

The API carries a protocol version. Enums are `#[non_exhaustive]` on the
Rust side and decoded with an unknown-case fallback on the Swift side, so a
newer core never crashes an older UI build during development.

---

## 4. Core modules

### 4.1 `store`

- SQLite (bundled), WAL. **One writer**: a dedicated thread owns the
  connection and serves requests over a channel. Async code sends a request
  and awaits the reply; no executor thread ever blocks on SQLite.
- Schema changes are numbered migrations tracked with `user_version`,
  applied at open, each tested from the previous version.
- Every write returns a `Result` that callers must handle. A failed commit
  fails the turn with a storage error; the UI shows it.
- The transcript is provider-neutral: messages made of parts (text,
  thinking, tool use, tool result, image, summary). Provider-specific opaque
  data (thinking signatures, reasoning echo) is kept on the part it belongs
  to.
- A session row is written when the first message is sent. Creating a
  session allocates an id and directories only.

### 4.2 `engine`

- Owns sessions in memory: current model, active turn, queued input.
- **Queued input is durable until run**: a message sent while a turn runs is
  persisted as queued; when the turn ends — successfully or not — the queue
  is either run or shown to the user. It is never dropped.
- One active turn per session; turns in different sessions run concurrently.
- On start-up, turns left running by a killed process are closed and the
  transcript is made valid (dangling tool calls sealed), so the user can
  resume.

### 4.3 `agent` — the turn loop

```
loop:
  check context budget (may ask the user to summarise; see 4.5)
  stream one assistant message from the provider
  commit it
  if no tool calls: done
  run tool calls (parallel where the tool allows it)
  commit tool results
  guard: rounds, repeated identical call-and-result, cancellation
```

- **Commit before the next request.** An interrupted turn always leaves a
  transcript that can be sent back as is.
- **Streaming**: an idle timeout ends a silent stream; cancellation is
  observed at every await, not only between rounds.
- **Tool-call arguments** are finished by one function whichever way the
  stream ends. Arguments that do not parse are reported to the model as
  such, with the raw text, never replaced with `{}`.
- **Empty answers** (nothing, or thinking only) are retried once, then
  reported.
- Loop detection triggers on the same call *and* the same result, so polling
  is not mistaken for a loop.

### 4.4 `providers`

- One adapter per wire protocol: OpenAI Chat Completions (covers most relays
  and local servers), Anthropic Messages, Gemini.
- **Request shaping is data**: which thinking field, token-limit field,
  cache markers and extra fields a given (endpoint, model) gets is decided by
  a rule table with tests for each row, not by scattered string checks.
- Model facts come from, in order: the endpoint's own model list, a
  published catalogue (models.dev), unknown. Unknown is a legal value; no
  limits are guessed.
- **Capture mode** (debug builds, opt-in): request bodies and raw response
  streams written to disk with keys redacted. This is how "the model did
  not send it" is told apart from "we lost it".

### 4.5 `context`

- Token estimate per request against the model's window when known.
- Tool output clearing first; summarising only after asking the user, and a
  summary can be undone (summaries are marks in the store, not deletions).
- Large tool outputs are spilled to a workspace file; the model gets the
  head, the tail and the path.

### 4.6 `tools`

- A registry of tools, each with: name, description, JSON schema, a
  permission class (allow / ask / deny), whether it may run in parallel,
  and how its card is rendered.
- **Output budget per tool**: every tool shapes its result for the likely
  next step. Example: opening a web page returns the page's title and text;
  the list of clickable elements comes only from `snapshot`.
- **Descriptions and outputs are part of the product and are evaluated.**
  `tools/evals/` holds scenarios (prompt → expected first tool and
  arguments); a runner sends the real request to a configured endpoint N
  times and reports the distribution. Changing a description needs an eval
  run before and after.
- Tool set: `shell`, `shell_jobs`, `file_read`, `file_write`, `file_edit`,
  `read_image`, `browser`, and one tool per device capability. Device
  capabilities are registered only when the platform reports them
  available. Whether some rarely used capabilities are folded into a single
  `device` tool is decided by eval data, not up front.
- Permission `ask` is enforced by the engine through a UI prompt event; the
  platform does not decide.

### 4.7 `sandbox`

```rust
trait Sandbox {
    async fn boot(&self) -> Result<()>;
    async fn exec(&self, spec: ExecSpec, out: OutputSink, cancel) -> Result<ExecResult>;
    async fn open_terminal(&self, rows, cols, output, exited) -> Result<Arc<dyn Terminal>>;
    fn info(&self) -> SandboxInfo;            // description + checked notes for the prompt
    fn workspace_dir(&self) -> PathBuf;       // the device folder behind /solos/ws
    fn guest_root_dir(&self) -> Option<PathBuf>; // the guest's /, read-only from the app
}
```

- `sandbox-ish` implements it on iOS: the iSH kernel linked in process,
  every blocking C call on a dedicated thread, exit notifications through a
  callback registry.
- `HostSandbox` runs the same interface on macOS/Linux for tests and the
  desktop CLI.
- Layout inside the guest: `/solos/ws` (the workspace, the default working
  directory for the model's commands; the terminal opens at home), with the
  user's attachments in `/solos/ws/attachments`. Paths the model sees map one-to-one to
  `solos://` URLs the UI can open.

### 4.8 `bridge` — guest CLI

- When the engine opens it serves the tool registry on `127.0.0.1` (port
  from the kernel) with a token made at that start: `GET /v1/tools`,
  `POST /v1/tools/{name}` (the tool's own fields as the JSON body; the
  session in `X-Solos-Session`), `GET /v1/files/url?path=`. Replies are
  `{ok, tool, data | text | error, images?}`; a tool's refusal is 200 with
  `ok: false`, a refused request 401 / 404 / 400.
- The sandbox adds `SOLOS_API_URL` and `SOLOS_API_TOKEN` to every process it
  starts (commands, background jobs, the terminal); `shell` adds
  `SOLOS_SESSION_ID`. The token is never written into the guest.
- `solos` (`crates/guest`: no dependencies, static musl, about 450 KB) is
  built by `ios/build-core.sh`, carried in the core (`bridge::GUEST_CLI`)
  and written to `/usr/local/bin/solos` at boot: `solos device <capability>
  [<action>] --field value`, `solos call <tool> …`, `solos files url <path>`,
  `solos tools`. Exit codes: 0 ok, 1 the tool said no, 2 bad arguments,
  3 token refused, 4 no such tool.
- A script reaches exactly the tools the model has, and nothing else.

---

## 5. Platform layer (iOS)

### 5.1 UI

- SwiftUI, iOS 18+.
- Per screen, one `@Observable` model holding `ViewState` produced by the
  reducer. Views read state; they do not derive facts from their own
  lifecycle (appear / disappear) or write layout-driving state from it.
- Streaming text is coalesced to a fixed frame rate before it reaches the
  view state.
- Scroll position is read from scroll geometry; following the stream is a
  separate flag that only the user's own scrolling turns off.
- Tool calls render as one row each; the detail sheet shows arguments,
  output and produced files.
- **Localisation**: all user-visible strings are in a String Catalog with
  English as the source language. The core never produces user-facing
  prose: it emits error *kinds* with parameters, and the app turns them into
  localised text. Model-facing text (tool results, notes to the model) is
  English and is not localised.

### 5.2 Platform services

- **Device**: one `DeviceHost` (`capabilities()`, `call(capability, json)`),
  with each capability in its own file (`DeviceTools+<Name>.swift`). Calls
  arrive on a core thread the core is willing to block; framework callbacks
  are awaited through `Handoff` with a deadline. The core resolves any file
  argument to the file on the device before the call, and advertises only
  what `capabilities()` returns (alarms need iOS 26, health a device with
  HealthKit). Argument parsing shared by capabilities (`DateArg`) has unit
  tests.
- **Browser**: a tab set of `WKWebView`s owned by the app, driven by the core
  through a foreign trait. Page-side JavaScript lives in the core. Defaults:
  Mobile Safari user agent; a page that has drawn content is handed over
  after a soft limit even if it never finishes loading.
- **Secrets**: keys in the Keychain, read lazily through a resolver so a key
  changed in Settings applies to the next request.
- **Lifecycle**: background task assertions around running turns; a local
  notification when a turn finishes in the background; turns cut off by the
  system are offered for resume.

---

## 6. Verification

Four layers, all runnable by a contributor and by CI where possible.

| Layer | What | Where it runs |
|---|---|---|
| Core unit & integration | agent loop with a scripted provider, store migrations, request shaping table, tool output shaping | `cargo test`, CI |
| Reducer | events → view state, including gaps and resyncs | `xcodebuild test`, CI |
| Headless turn | a real model turn through the real core, sandbox and platform services, no UI | `solos-probe` app in the simulator; one command |
| On screen | the app itself, driven by `axe`; sessions can be seeded from a headless run's database | simulator |

Plus two comparison tools under `tools/`:

- **Parity runner**: sends the same prompt through Solos and the reference
  app on the same simulator and endpoint, and collects both sides' requests
  (from the endpoint's log or a capture proxy) and screenshots. Used for
  every in-scope feature before it is called done.
- **Eval runner**: the tool-choice evaluations from 4.6.

A feature's definition of done: tests pass, headless turn passes, checked on
screen, parity run shows Solos is not worse.

---

## 7. Repository

```
solos/
  LICENSE                 GPL-3.0 + App Store exception
  README.md               English; README.zh-Hans.md
  core/                   Rust workspace
    crates/api            commands, events, snapshots
    crates/core           engine, agent, providers, tools, context, store
    crates/sandbox-ish    iSH binding
    crates/ffi            UniFFI surface
    crates/guest-cli      the `solos` CLI (musl)
    crates/cli            desktop CLI over HostSandbox (development)
  ios/
    Solos/                app sources
    Probe/                headless turn runner
    Config/               Base.xcconfig; Local.xcconfig (gitignored)
  deps/                   ish, build scripts (reproducible), rootfs recipe
  tools/                  parity runner, eval runner, simulator helpers
  docs/                   requirements, architecture, decisions
```

Team ID, bundle identifier prefix and signing come from
`ios/Config/Local.xcconfig`, created from an example file. No key, endpoint
or personal identifier is ever committed. CI builds a clean clone and runs
the core and reducer tests.

---

## 8. Delivery order

Each step ends with the definition of done in section 6.

1. **Skeleton and harness**: repository layout, CI, core API with async FFI
   and seq/snapshot, store with migrations, headless runner, reducer tests,
   parity runner. One provider (OpenAI-compatible), one tool (`shell`) on
   iSH. A chat that runs a shell command end to end.
2. **Conversation complete**: all providers, thinking toggle, retry / edit,
   resume, queueing, sessions, titles, attachments, Markdown and media
   rendering, context management.
3. **Sandbox complete**: file tools, jobs, terminal, file browser, guest CLI.
4. **Browser**.
5. **Device capabilities**, with the alarm list.
6. **Open-source readiness**: localisation (en, zh-Hans, zh-Hant), README,
   contributor guide, licence headers, dependency build from a clean clone.

Android starts after step 6 and adds a UI and platform services only.
