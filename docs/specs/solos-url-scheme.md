# Solos URL Scheme Specification

**Status:** Draft
**Date:** 2026-02-16

## 1. Overview

`solos://` is a **session-scoped** unified resource locator for persistent, addressable resources within the Solos ecosystem. It bridges three layers — the AI agent, the iSH Linux shell, and the iOS host app — with a single URL that works in tool results, Markdown rendering, and inter-component references.

**`solos://` URLs are inherently session-bound.** Every resolution, read, write, and render operation is performed in the context of the current active session. There is no cross-session resource visibility — a `solos://attachments/photo.png` in Session A and the same URL in Session B refer to completely independent files. The agent, the shell, and the UI all see only the resources belonging to the active session.

### Design Principles

1. **Session-scoped**: All resource access is bound to the current session. `solos://` URLs are opaque identifiers — they carry no session ID, because they are always resolved against the active session's storage. Cross-session access is neither supported nor exposed.
2. **Agent-first**: Every persistent resource produced by a tool MUST be returned to the model as a `solos://` URL so the agent can reference it in subsequent turns.
3. **Render-ready**: The chat UI resolves `solos://` URLs inline — images, audio, video, and downloadable files render natively in Markdown.
4. **Bidirectional**: Files written by the shell or the host app are both addressable via the same URL scheme.
5. **Persistent within session**: Resources survive app restarts. Each session's files are isolated in persistent storage and mounted into `/var/solos/` on session load.

## 2. URL Format

```
solos://<namespace>/<path>
```

| Component     | Description |
|---------------|-------------|
| `solos://`    | Scheme. Always lowercase. |
| `<namespace>` | Top-level category (see §3). Mapped to URL host component. |
| `<path>`      | Relative path within the namespace. May include subdirectories. Mapped to URL path component. |

### Examples

```
solos://attachments/screenshot.png
solos://attachments/photos/vacation/img_001.jpg
solos://workspace/report.csv
solos://workspace/project/src/main.py
solos://offloads/shell_execute_1707000000_abc12345.txt
solos://browser/snapshot_1707000000.jpg
```

## 3. Session Model

`solos://` is a **session-relative** addressing scheme. The same URL string in different sessions resolves to different physical files.

### 3.1 Why No Session ID in the URL

The URL deliberately omits the session ID:
- The **agent** operates within a single session and has no concept of other sessions.
- The **shell** (`/var/solos/`) only ever sees one session's files at a time.
- The **chat UI** renders messages in the context of their owning session.
- Embedding session IDs would leak an implementation detail and create a temptation for cross-session references, which are not supported.

### 3.2 Resolution Context

Every `solos://` URL is resolved with an implicit session context:

| Layer | How session context is determined |
|-------|-----------------------------------|
| Agent (tool execution) | `AIChatViewModel.sessionId` — set when the session is loaded |
| Shell (iSH filesystem) | `/var/solos/` is mounted from the active session's persistent storage |
| Chat UI (Markdown render) | Messages belong to a session; the renderer resolves against the current session's files |
| Persistent storage | `Library/SolosChat/solos/<sessionId>/` — each session has its own directory tree |

### 3.3 Cross-Session Semantics

- **No cross-session reads:** An agent cannot access files from another session. There is no `solos://other-session/...` syntax.
- **No cross-session writes:** Writing to `/var/solos/` always targets the active session's storage.
- **Session deletion:** When a session is deleted, its entire `Library/SolosChat/solos/<sessionId>/` tree is removed. All `solos://` URLs from that session become permanently unresolvable.
- **History rendering:** When viewing chat history from a past session, `solos://` URLs resolve against that session's persisted files (loaded on session switch).

## 4. Namespaces

### 4.1 `attachments` — Media & Displayable Files

**Purpose:** Images, audio, video, and other media intended for inline display in chat.

| Property | Value |
|----------|-------|
| Linux path | `/var/solos/attachments/<path>` |
| Persistent storage | `Library/SolosChat/solos/<sessionId>/attachments/<path>` |
| Writable by | Agent (file_write, shell_execute), Host app (saveAttachment), User (input attachments) |
| Inline rendering | Yes — images, audio, video auto-render in Markdown |

**Supported inline media types:**
- Images: `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.bmp`, `.tiff`, `.svg`
- Audio: `.mp3`, `.m4a`, `.wav`, `.aac`, `.ogg`, `.flac`
- Video: `.mp4`, `.mov`, `.m4v`, `.avi`, `.mkv`, `.webm`

**Markdown usage:**
```markdown
![description](solos://attachments/filename.png)
![audio](solos://attachments/recording.mp3)
![video](solos://attachments/demo.mp4)
```

### 4.2 `workspace` — General Working Files

**Purpose:** Scripts, data files, configuration, project files, and any non-media artifacts the agent creates or the user works with.

| Property | Value |
|----------|-------|
| Linux path | `/var/solos/workspace/<path>` |
| Persistent storage | `Library/SolosChat/solos/<sessionId>/workspace/<path>` |
| Writable by | Agent (file_write, shell_execute) |
| Inline rendering | No (text link only) |

**Markdown usage:**
```markdown
[report.csv](solos://workspace/report.csv)
```

### 4.3 `offloads` — Truncated Tool Outputs

**Purpose:** Automatically saved when a tool result exceeds `maxToolResultLength` (20,000 chars). The agent receives the truncated output plus a reference to the full file.

| Property | Value |
|----------|-------|
| Linux path | `/var/solos/offloads/<filename>` |
| Persistent storage | `Library/SolosChat/solos/<sessionId>/offloads/<filename>` |
| Writable by | Host app (automatic offload) |
| Inline rendering | No |
| Filename pattern | `<toolName>_<timestamp>_<toolIdPrefix>.txt` |

### 4.4 `browser` — Browser Snapshots (New)

**Purpose:** Screenshots and readable-text extracts from `browser_use` tool actions, addressable for the agent to reference later.

| Property | Value |
|----------|-------|
| Linux path | `/var/solos/browser/<filename>` |
| Persistent storage | `Library/SolosChat/solos/<sessionId>/browser/<filename>` |
| Writable by | Host app (after browser_use actions) |
| Inline rendering | Yes (images) |

## 5. Path Resolution

### 5.1 `solos://` → Linux Path

```
solos://<namespace>/<path>  →  /var/solos/<namespace>/<path>
```

Extract `host` as namespace, concatenate with `path`:
```swift
let linuxPath = "/var/solos/\(url.host!)\(url.path)"
```

### 5.2 Linux Path → Host Filesystem

```
/var/solos/<namespace>/<path>  →  <dataPath>/var/solos/<namespace>/<path>
```

Where `dataPath` = `~/Documents/alpine-rootfs/data/`.

```swift
func resolveHostPath(_ linuxPath: String) -> URL? {
    guard linuxPath.hasPrefix("/"), !linuxPath.contains("..") else { return nil }
    let relative = String(linuxPath.dropFirst())
    return RootfsManager.shared.dataPath.appendingPathComponent(relative)
}
```

### 5.3 Host Filesystem → Persistent Storage (Session Isolation)

```
<dataPath>/var/solos/<namespace>/<path>  →  Library/SolosChat/solos/<sessionId>/<namespace>/<path>
```

The iSH-visible directory (`/var/solos/`) is a **session-unaware working copy** — a transient mount point that always reflects exactly one session's files. The agent and the shell never see a session ID; they simply read and write `/var/solos/`. The host app is responsible for swapping the backing storage on session transitions.

**Session switch lifecycle:**
1. **Harvest** — copy any new/modified files from iSH data → outgoing session's persistent storage (captures shell-written files that haven't been persisted yet)
2. **Clear** — remove all files from the iSH-visible `/var/solos/` directory
3. **Mount** — copy all persistent files from the incoming session's storage → iSH-visible `/var/solos/`
4. **Register** — ensure all mounted files exist in meta.db so the iSH kernel can access them

**Isolation guarantee:** At no point during this lifecycle are files from two different sessions simultaneously visible under `/var/solos/`. The clear-then-mount sequence is atomic from the agent's perspective (it only runs within a session).

### 5.4 meta.db Registration

Every file written to the host filesystem (`dataPath/var/solos/...`) MUST be registered in iSH's `meta.db` for the Linux kernel to see it:

- `ensureFakefsMetadata(for: linuxPath, isDirectory: false)` — registers file inode
- `ensureParentDirsInMetaDB(for: linuxPath)` — ensures all ancestor directories exist

## 6. Tool Result Contract

**Core rule:** When a tool action produces or modifies a persistent, addressable resource, the tool result MUST include the `solos://` URL so the model can reference it.

### 6.1 `file_write` — Current Behavior & Enhancement

**Current** tool result:
```
Wrote to /var/solos/attachments/chart.png (1234 bytes)
```

**Enhanced** tool result for files under `/var/solos/`:
```
Wrote to /var/solos/attachments/chart.png (1234 bytes)
solos_url: solos://attachments/chart.png
```

The `solos_url` field is appended only when the written path falls under `/var/solos/`. This gives the model an explicit, copy-paste-ready URL to embed in Markdown responses.

**Implementation:** In `executeFileWrite`, after a successful write to any path under `/var/solos/`:
```swift
// After successful write
var result = "\(action) to \(path) (\(bytesWritten) bytes)"
if path.hasPrefix("/var/solos/") {
    let solosPath = String(path.dropFirst("/var/solos/".count))
    let namespace = solosPath.components(separatedBy: "/").first ?? ""
    let rest = String(solosPath.dropFirst(namespace.count))
    result += "\nsolos_url: solos://\(namespace)\(rest)"
}
```

### 6.2 `shell_execute` — Post-Scan Enhancement

After a `shell_execute` completes, scan for new or modified files under `/var/solos/` and append their `solos://` URLs to the tool result.

**Enhanced** tool result:
```
<normal stdout/stderr output>

[solos] New files:
  solos://attachments/output.png
  solos://workspace/results.json
```

**Implementation:** Before and after execution, snapshot the set of files under each `/var/solos/` subdirectory. Diff to find new/modified files. Append their URLs.

### 6.3 `browser_use` — Screenshot URLs

When `browser_use` captures a screenshot, save it to `/var/solos/browser/` and include the URL in the tool result.

**Enhanced** tool result:
```
Screenshot captured (1280x720)
solos_url: solos://browser/screenshot_1707000000.jpg
```

### 6.4 Offload References

Already implemented. When tool output exceeds the limit:
```
<truncated output>

[OUTPUT TRUNCATED] Full output (50000 chars) saved to: /var/solos/offloads/shell_execute_1707000000_abc12345.txt
Use file_read tool to read the complete output.
```

**Enhancement:** Add `solos://` URL for consistency:
```
solos_url: solos://offloads/shell_execute_1707000000_abc12345.txt
```

### 6.5 User Attachment URLs

When the user attaches an image/file via the input bar and it's saved to `/var/solos/attachments/`, the `solos://` URL is included in the user message context so the agent knows the file path.

Already implemented via `saveAttachment()` → returns `solos://attachments/<filename>`.

## 7. System Prompt Update

The agent system prompt should be updated to document the URL scheme and the tool result contract:

```
Shared directory /var/solos/ (bidirectional read/write between shell and app):
  /var/solos/attachments/ — Media files (images, audio, video). Display inline with ![desc](solos://attachments/filename).
  /var/solos/workspace/   — Working files (scripts, data, configs). Link with [name](solos://workspace/filename).
  /var/solos/offloads/    — Auto-saved large outputs. Read with file_read.
  /var/solos/browser/     — Browser screenshots and extracts.

The solos:// URL scheme:
  solos://attachments/file.png  →  /var/solos/attachments/file.png
  solos://workspace/data.csv    →  /var/solos/workspace/data.csv

When you write files to /var/solos/, the tool result includes a solos_url you can embed directly in Markdown.
Supported inline types: images (.png/.jpg/.gif/.webp), audio (.mp3/.m4a/.wav), video (.mp4/.mov/.m4v).
For non-media files, use Markdown links: [filename](solos://workspace/filename).
```

## 8. Chat UI Rendering

### 8.1 SolosImageProvider (Existing)

The `SolosImageProvider` in `AIChatView.swift` handles all `solos://` URLs in Markdown image syntax (`![](solos://...)`). It dispatches based on file extension:

- **Image extensions** → `UIImage` with tap-to-fullscreen, retry-on-load (6 attempts, 500ms interval)
- **Audio extensions** → `SolosAudioPlayerView` with play/pause, seek, duration
- **Video extensions** → `SolosVideoPlayerView` with thumbnail + fullscreen player

### 8.2 Link Rendering (Enhancement)

Markdown links with `solos://` scheme (`[name](solos://...)`) should be rendered as tappable file chips that:
- Show the filename and a file-type icon
- On tap: open a preview (Quick Look) or share sheet for the file
- Non-media files (`.csv`, `.txt`, `.py`, `.json`, etc.) get a document icon + filename pill

### 8.3 Subdirectory Support

The current `resolveSolosFileURL` already supports arbitrary path depth:
```swift
// solos://attachments/photos/img.jpg → /var/solos/attachments/photos/img.jpg
let linuxPath = "/var/solos/\(host)\(url.path)"
```

No changes needed for resolution. The persistent storage and mount logic should be extended to handle nested subdirectories (currently only handles flat file lists in `mountSolosSubdir`).

## 9. Implementation Checklist

### Phase 1: Tool Result URLs
- [ ] `file_write`: Append `solos_url:` when path is under `/var/solos/`
- [ ] `shell_execute`: Scan for new/modified files under `/var/solos/` after execution, append URLs
- [ ] `offloadToolOutput`: Append `solos_url:` to truncation notice
- [ ] Update system prompt with full URL scheme documentation

### Phase 2: Browser Namespace
- [ ] Add `browser` namespace directories to session mount/persist logic
- [ ] Save browser screenshots to `/var/solos/browser/` with `solos://` URL in tool result
- [ ] Add `browser` directory to `mountSolosSubdir` calls

### Phase 3: Enhanced Rendering
- [ ] Implement tappable file chip for `solos://` links (non-image Markdown links)
- [ ] Quick Look / share sheet integration for non-media files
- [ ] Recursive subdirectory support in `mountSolosSubdir` (currently flat)

### Phase 4: Workspace Awareness
- [ ] `file_read`: When reading from `/var/solos/`, include `solos_url:` in result
- [ ] Consider `solos://` URL auto-complete or suggestion in agent context

## 10. Security Considerations

- **Session isolation**: The agent and shell MUST only access the active session's resources. The mount/unmount lifecycle (§5.3) enforces this at the filesystem level. No API or filesystem path exposes another session's data.
- **Path traversal**: `resolveHostPath` rejects paths containing `..`. Maintain this.
- **Namespace validation**: Only known namespaces (`attachments`, `workspace`, `offloads`, `browser`) should be accepted. Reject unknown namespaces.
- **No session ID in URLs**: Session IDs are never exposed in `solos://` URLs, tool results, or system prompts. This prevents the agent from attempting cross-session references.
- **File size limits**: Large files in `/var/solos/attachments/` could consume device storage. Consider per-session quotas (future).
- **meta.db integrity**: Always register files in meta.db atomically. Current implementation handles this correctly.
