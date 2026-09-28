//! `file_read`, `file_write`, `file_edit`: files in the workspace, without a
//! shell.
//!
//! The reference app has the same three, and for the same reasons: writing
//! a file through a shell means quoting it into a command, which breaks on
//! the content that matters most (code, JSON, quotes); reading one through
//! `cat` has no way to page. The workspace is a folder of the device's own,
//! so these do plain file I/O on it and never start the emulator. Paths
//! elsewhere in the guest are refused with a pointer to `shell`.

use super::{object_schema, Tool, ToolContext, ToolOutput, ToolSpec};
use crate::sandbox::GUEST_WORKSPACE;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Characters `file_read` returns by default.
const READ_CHARS: usize = 15_000;

/// When each file was last read or written through these tools, so an edit
/// can be refused if something else changed the file since: the model would
/// otherwise replace text in a version it has never seen.
#[derive(Default)]
pub struct Ledger(Mutex<HashMap<PathBuf, (SystemTime, u64)>>);

impl Ledger {
    fn record(&self, host: &Path) {
        if let Some(stamp) = stamp(host) {
            self.0.lock().unwrap().insert(host.to_path_buf(), stamp);
        }
    }

    fn changed_since_seen(&self, host: &Path) -> bool {
        match (self.0.lock().unwrap().get(host), stamp(host)) {
            (Some(seen), Some(now)) => *seen != now,
            _ => false,
        }
    }
}

fn stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The three tools, sharing one ledger.
pub fn tools() -> Vec<Arc<dyn Tool>> {
    let ledger = Arc::new(Ledger::default());
    vec![Arc::new(FileRead(ledger.clone())), Arc::new(FileWrite(ledger.clone())), Arc::new(FileEdit(ledger))]
}

/// A path as the model wrote it — `/solos/ws/x`, `solos://ws/x`, or `x`
/// relative to the workspace — to the file on the device and its guest path.
fn resolve(ctx: &ToolContext, path: &str) -> Result<(PathBuf, String), String> {
    let path = path.trim();
    let guest = if path.starts_with(crate::files::URL_PREFIX) || path.starts_with('/') {
        path.to_string()
    } else {
        format!("{GUEST_WORKSPACE}/{}", path.trim_start_matches("./"))
    };
    let workspace = ctx.sandbox.workspace_dir();
    let host = crate::files::resolve(&guest, &workspace).ok_or_else(|| {
        format!("{path} is not inside the workspace ({GUEST_WORKSPACE}). These tools only reach the workspace; use `shell` for other paths.")
    })?;
    let rel = host.strip_prefix(&workspace).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((host, format!("{GUEST_WORKSPACE}/{rel}")))
}

fn text_arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

fn uint_arg(input: &Value, key: &str) -> Option<usize> {
    input.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).map(|n| n as usize)
}

fn bool_arg(input: &Value, key: &str) -> bool {
    input.get(key).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true"))).unwrap_or(false)
}

fn url_of(guest: &str) -> String {
    guest.replacen(GUEST_WORKSPACE, "solos://ws", 1)
}

// ---------------------------------------------------------------------------

pub struct FileRead(Arc<Ledger>);

#[async_trait]
impl Tool for FileRead {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_read".into(),
            description: "Read a text file in the workspace. Quicker than `cat` through the shell, and it pages: the \
                first line says which lines came back out of how many, and ends with next_offset=N when there is more — \
                pass that as `offset` to go on. Binary files are refused with their size."
                .into(),
            schema: object_schema(
                json!({
                    "path": {"type": "string", "description": "The file, e.g. /solos/ws/data.csv (a solos://ws/ link or a path relative to /solos/ws also works)."},
                    "offset": {"type": "integer", "description": "First line to return, counting from 1. Default 1."},
                    "lines": {"type": "integer", "description": "At most this many lines."},
                    "max_length": {"type": "integer", "description": "At most this many characters. Default 15000."},
                    "direction": {"type": "string", "enum": ["head", "tail"], "description": "\"tail\" reads from the end of the file instead; `offset` is then ignored."}
                }),
                &["path"],
            ),
            parallel: true,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let Some(path) = text_arg(input, "path") else { return ToolOutput::error("`path` is required.") };
        let (host, guest) = match resolve(ctx, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let bytes = match std::fs::read(&host) {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Cannot read {guest}: {e}")),
        };
        if bytes.contains(&0) {
            return ToolOutput::error(format!("{guest} is a binary file ({} bytes); look at it with `shell` (`xxd`, `od`).", bytes.len()));
        }
        self.0.record(&host);
        let text = String::from_utf8_lossy(&bytes);
        let all: Vec<&str> = text.lines().collect();
        let total = all.len();
        let budget = uint_arg(input, "max_length").unwrap_or(READ_CHARS).max(1);
        let want = uint_arg(input, "lines");
        let tail = text_arg(input, "direction") == Some("tail");

        let (start, end) = if tail {
            (total - want.unwrap_or(total).min(total), total)
        } else {
            let start = uint_arg(input, "offset").unwrap_or(1).max(1).saturating_sub(1).min(total);
            (start, want.map(|n| (start + n).min(total)).unwrap_or(total))
        };
        let mut body = String::new();
        let mut used = 0;
        let mut i = start;
        // Whole lines only, from the end when reading the tail.
        let range: Vec<usize> = if tail { (start..end).rev().collect() } else { (start..end).collect() };
        let mut kept: Vec<usize> = Vec::new();
        for n in range {
            let cost = all[n].chars().count() + 1;
            if used + cost > budget && !kept.is_empty() {
                break;
            }
            used += cost;
            kept.push(n);
        }
        kept.sort_unstable();
        for &n in &kept {
            body.push_str(all[n]);
            body.push('\n');
            i = n + 1;
        }
        let first = kept.first().map(|n| n + 1).unwrap_or(0);
        let last = kept.last().map(|n| n + 1).unwrap_or(0);
        let mut header = format!("{guest} · lines {first}-{last} of {total}");
        if !tail && i < total {
            header.push_str(&format!(" · next_offset={}", i + 1));
        }
        if tail && first > 1 {
            header.push_str(" · earlier lines not shown");
        }
        ToolOutput::ok(format!("{header}\n{body}"))
    }
}

// ---------------------------------------------------------------------------

pub struct FileWrite(Arc<Ledger>);

#[async_trait]
impl Tool for FileWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_write".into(),
            description: "Write a text file in the workspace, replacing it or, with `append`, adding to its end. Better \
                than echo or a heredoc: nothing is quoted, so the content arrives exactly. Every character of `content` \
                has to be generated and sent before the write happens, so a large file (more than about 8 KB) is best \
                written in parts with `append`, or produced by a short script run with `shell` when it is repetitive or \
                can be computed."
                .into(),
            schema: object_schema(
                json!({
                    "path": {"type": "string", "description": "The file, e.g. /solos/ws/report.md."},
                    "content": {"type": "string", "description": "The text to write."},
                    "append": {"type": "boolean", "description": "Add to the end of the file instead of replacing it. Default false."},
                    "create_dirs": {"type": "boolean", "description": "Create missing parent folders. Default false."}
                }),
                &["path", "content"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let (Some(path), Some(content)) = (text_arg(input, "path"), text_arg(input, "content")) else {
            return ToolOutput::error("`path` and `content` are required.");
        };
        let (host, guest) = match resolve(ctx, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        if let Some(parent) = host.parent().filter(|p| !p.exists()) {
            if !bool_arg(input, "create_dirs") {
                let dir = Path::new(&guest).parent().map(|p| p.display().to_string()).unwrap_or_default();
                return ToolOutput::error(format!("The folder {dir} does not exist. Pass create_dirs: true to create it."));
            }
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::error(format!("Cannot create the folders for {guest}: {e}"));
            }
        }
        let append = bool_arg(input, "append");
        let result = if append {
            use std::io::Write;
            std::fs::OpenOptions::new().create(true).append(true).open(&host).and_then(|mut f| f.write_all(content.as_bytes()))
        } else {
            std::fs::write(&host, content.as_bytes())
        };
        if let Err(e) = result {
            return ToolOutput::error(format!("Cannot write {guest}: {e}"));
        }
        self.0.record(&host);
        let size = std::fs::metadata(&host).map(|m| m.len()).unwrap_or(0);
        let verb = if append { "Appended" } else { "Wrote" };
        ToolOutput::ok(format!("{verb} {} bytes to {guest} (now {size} bytes).\nLink: {}", content.len(), url_of(&guest)))
    }
}

// ---------------------------------------------------------------------------

pub struct FileEdit(Arc<Ledger>);

#[async_trait]
impl Tool for FileEdit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_edit".into(),
            description: "Change part of a file in the workspace by replacing exact text. Read the file with file_read \
                first: `old_string` must match the file exactly, spaces and indentation included, and must occur once \
                unless `replace_all` is set. For changing an existing file this beats rewriting it with file_write, since \
                only the changed part is sent. The edit is refused if the file changed after it was last read."
                .into(),
            schema: object_schema(
                json!({
                    "path": {"type": "string", "description": "The file to change."},
                    "old_string": {"type": "string", "description": "The exact text to replace."},
                    "new_string": {"type": "string", "description": "What to put in its place; empty to delete it."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence. Default false."}
                }),
                &["path", "old_string", "new_string"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let (Some(path), Some(old), Some(new)) = (text_arg(input, "path"), text_arg(input, "old_string"), text_arg(input, "new_string")) else {
            return ToolOutput::error("`path`, `old_string` and `new_string` are required.");
        };
        if old.is_empty() {
            return ToolOutput::error("`old_string` is empty; to write a whole file use file_write.");
        }
        let (host, guest) = match resolve(ctx, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let current = match std::fs::read_to_string(&host) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Cannot read {guest}: {e}")),
        };
        if self.0.changed_since_seen(&host) {
            return ToolOutput::error(format!("{guest} has changed since it was last read. Read it again with file_read, then edit."));
        }
        let count = current.matches(old).count();
        let all = bool_arg(input, "replace_all");
        if count == 0 {
            return ToolOutput::error(format!("`old_string` does not occur in {guest}. It must match exactly, spaces and line breaks included."));
        }
        if count > 1 && !all {
            return ToolOutput::error(format!(
                "`old_string` occurs {count} times in {guest}. Include more of the surrounding text so it is unique, or set replace_all."
            ));
        }
        let updated = if all { current.replace(old, new) } else { current.replacen(old, new, 1) };
        if let Err(e) = std::fs::write(&host, updated.as_bytes()) {
            return ToolOutput::error(format!("Cannot write {guest}: {e}"));
        }
        self.0.record(&host);
        let n = if all { count } else { 1 };
        ToolOutput::ok(format!("Replaced {n} occurrence{} in {guest}.\nLink: {}", if n == 1 { "" } else { "s" }, url_of(&guest)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::host::HostSandbox;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> (ToolContext, PathBuf) {
        let dir = std::env::temp_dir().join(format!("solos-files-{}", uuid::Uuid::new_v4()));
        let sandbox = Arc::new(HostSandbox::new(dir.clone()));
        let ws = crate::sandbox::Sandbox::workspace_dir(&*sandbox);
        std::fs::create_dir_all(&ws).unwrap();
        (ToolContext { session_id: "s".into(), sandbox, cancel: CancellationToken::new(), on_output: Arc::new(|_| {}) }, ws)
    }

    fn by_name(name: &str) -> Arc<dyn Tool> {
        tools().into_iter().find(|t| t.spec().name == name).unwrap()
    }

    #[tokio::test]
    async fn write_read_and_edit_a_file_by_any_of_its_names() {
        let (c, ws) = ctx();
        let t = tools();
        let (read, write, edit) = (&t[0], &t[1], &t[2]);
        let w = write.call(&c, &json!({"path": "/solos/ws/a/b.txt", "content": "hello\nworld\n"})).await;
        assert!(w.is_error, "no folder a/ yet: {}", w.text);
        let w = write.call(&c, &json!({"path": "/solos/ws/a/b.txt", "content": "hello\nworld\n", "create_dirs": true})).await;
        assert!(!w.is_error, "{}", w.text);
        assert!(w.text.contains("solos://ws/a/b.txt"));

        let r = read.call(&c, &json!({"path": "solos://ws/a/b.txt"})).await;
        assert_eq!(r.text, "/solos/ws/a/b.txt · lines 1-2 of 2\nhello\nworld\n");

        let e = edit.call(&c, &json!({"path": "a/b.txt", "old_string": "world", "new_string": "solos"})).await;
        assert!(!e.is_error, "{}", e.text);
        assert_eq!(std::fs::read_to_string(ws.join("a/b.txt")).unwrap(), "hello\nsolos\n");

        let twice = edit.call(&c, &json!({"path": "a/b.txt", "old_string": "l", "new_string": "L"})).await;
        assert!(twice.is_error && twice.text.contains("occurs"), "{}", twice.text);
    }

    #[tokio::test]
    async fn an_edit_is_refused_when_the_file_changed_after_it_was_read() {
        let (c, ws) = ctx();
        std::fs::write(ws.join("n.txt"), "one\n").unwrap();
        let t = tools();
        t[0].call(&c, &json!({"path": "n.txt"})).await;
        // Something else (the shell, the user) rewrites it.
        std::fs::write(ws.join("n.txt"), "one two\n").unwrap();
        let e = t[2].call(&c, &json!({"path": "n.txt", "old_string": "one", "new_string": "1"})).await;
        assert!(e.is_error && e.text.contains("changed since"), "{}", e.text);
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_are_refused_with_the_way_on() {
        let (c, _) = ctx();
        for p in ["/etc/passwd", "/solos/ws/../etc/passwd", "../x"] {
            let r = by_name("file_read").call(&c, &json!({"path": p})).await;
            assert!(r.is_error && r.text.contains("use `shell`"), "{p}: {}", r.text);
        }
    }

    #[tokio::test]
    async fn reading_pages_from_the_start_or_the_end() {
        let (c, ws) = ctx();
        std::fs::write(ws.join("p.txt"), (1..=50).map(|i| format!("line{i}\n")).collect::<String>()).unwrap();
        let read = by_name("file_read");
        let r = read.call(&c, &json!({"path": "p.txt", "offset": 10, "lines": 5})).await;
        assert!(r.text.starts_with("/solos/ws/p.txt · lines 10-14 of 50 · next_offset=15\nline10\n"), "{}", r.text);
        let r = read.call(&c, &json!({"path": "p.txt", "max_length": 20})).await;
        assert!(r.text.starts_with("/solos/ws/p.txt · lines 1-3 of 50 · next_offset=4\n"), "{}", r.text);
        let t = read.call(&c, &json!({"path": "p.txt", "direction": "tail", "lines": 2})).await;
        assert_eq!(t.text, "/solos/ws/p.txt · lines 49-50 of 50 · earlier lines not shown\nline49\nline50\n");
        std::fs::write(ws.join("b.bin"), [1u8, 0, 2]).unwrap();
        assert!(read.call(&c, &json!({"path": "b.bin"})).await.is_error);
    }
}
