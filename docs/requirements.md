# Solos — Requirements

Scope for the first release. The bar is simple: for every feature listed as
in scope, Solos must be at least as good as the reference app in the same
conditions — same endpoint, same model, same prompt.
"As good" is checked by running both side by side, not argued.

Implementation is ours to design. The reference app is the reference for *behaviour*,
not for architecture; an implementation detail of the reference app is adopted only when a
side-by-side comparison shows it is the better one.

## In scope

### Conversation
- Multiple providers: OpenAI-compatible (incl. relays), Anthropic, Gemini.
- Endpoints as a list: each with its own base URL and key and an Enabled switch;
  model list fetched from the endpoint and kept; default model; per-session model switch.
- Thinking on/off.
- AI Agent Mode on/off, per chat with a default in Settings (default on). Off is plain chat:
  the model gets no tools and the sandbox is not started; attachments still work.
- Streaming replies; stop; resume a turn cut off by the system.
- Copy, retry, edit-and-resend a message.
- Rendering: Markdown (code, tables, quotes, lists), math, inline images,
  video and audio from the workspace.
- Tool calls shown as one collapsed row each; full arguments and output in a
  detail view.
- Sessions: list, search, rename, delete, clear, automatic title.
- Context management: when a conversation gets long, ask before summarising;
  summaries can be undone.
- Attachments: photos and files.
- Notify when a long turn finishes in the background.

### Linux sandbox (on device, offline)
- Alpine Linux via iSH; persistent filesystem; `apk add` works.
- Model tools: shell (with background jobs), read / write / edit file,
  read image.
- A terminal the user can open and type into, sharing the model's workspace.
- A file browser over the workspace.
- A CLI inside the sandbox so scripts can call device capabilities.

### Browser
- In-app browser shared by the user and the model; the user can watch and
  take over (e.g. to log in).
- Up to three tabs, history, downloads into the workspace, cookie
  persistence across launches, mobile / desktop user agent, viewport.
- Model actions: navigate, read, snapshot, find, click, type, scroll,
  wait, collect, screenshot, eval, fetch (download), tabs, cookies.

### Device capabilities (as model tools)
Calendar, reminders, contacts, location, photos (read / export), clipboard,
notifications, alarms (with an in-app alarm list — AlarmKit alarms are not
visible anywhere else), media playback control, health (read), weather,
maps (search / route / ETA), vision (OCR / classify), natural language,
open URL / app, device info.

### Skills
- A skill is a folder with a `SKILL.md` (a `name` and a `description` at the
  top, instructions below) and whatever scripts and files it needs, in the
  common format other agents use.
- Installed from a GitHub link — by the model when the user asks in a chat,
  or by the user in Settings — or written by the model. Installing the same
  skill again updates it.
- The model is told which skills are installed in every chat and reads a
  skill's instructions when a task calls for it.
- Settings: list, view, turn on and off, delete, add from a link.

### MCP servers
- Servers over HTTP and over stdio (run inside the Linux sandbox).
- Added by the model when the user asks in a chat, or by the user in
  Settings (pasting the usual `mcpServers` JSON); the model can use a
  server's tools as soon as it is added.
- Settings: list, turn on and off, delete, see each server's tools.

### Settings
Endpoints and keys (keys in the Keychain), default model, thinking, AI Agent Mode, browser
preferences, sandbox status, skills, MCP servers.

## Also in scope
- Localisation from day one: English source strings; Simplified and
  Traditional Chinese at launch. The reference app ships 9 languages (en, zh-Hans,
  zh-Hant, ja, ko, de, fr, es, ru); the string pipeline must make adding
  those a translation task, not a code change.
- No personal data in the repository: no keys, endpoints, team IDs or
  bundle identifiers hard-coded; all come from build configuration or
  settings.
- Reproducible dependency builds from source; CI builds a clean clone.
- No telemetry.

## Out of scope (first release)
Voice chat and speech (TTS / STT), OAuth sign-in (Claude / Codex / Kimi),
iCloud sync and backup, "soul" / persona and long-term memory, MCP OAuth
sign-in, environment variables UI, model groups and fallback routing, home
screen widget and Live Activities, share extension, Files app provider,
App Intents / Shortcuts, HomeKit, NFC, Bluetooth, FFmpeg, remote folder
mounts (rclone), Face ID app lock, web apps, session folders, cross-device
sessions.

## Platforms
iOS first. The non-UI core is shared and platform-neutral so that an Android
client only needs its UI and device layer.

## Licence
GPL-3.0 — required because the iOS sandbox (iSH) is GPL-3.0 and runs in the
app's process. Solos's own code will carry an App Store distribution
exception, as iSH does, so the app can be both open source and published.
