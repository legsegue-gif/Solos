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

/// The three tools, sharing one ledger and the table of shared folders.
pub fn tools(mounts: Arc<crate::mounts::Mounts>) -> Vec<Arc<dyn Tool>> {
    let ledger = Arc::new(Ledger::default());
    vec![
        Arc::new(FileRead(ledger.clone(), mounts.clone())),
        Arc::new(FileWrite(ledger.clone(), mounts.clone())),
        Arc::new(FileEdit(ledger, mounts)),
    ]
}

/// A path as the model wrote it — `/solos/ws/x`, `solos://ws/x`, `x`
/// relative to the workspace, or a shared folder's — to the file on the
/// device.
fn resolve(ctx: &ToolContext, mounts: &crate::mounts::Mounts, path: &str) -> Result<crate::mounts::Located, String> {
    crate::mounts::locate(path, &ctx.sandbox.workspace_dir(), &mounts.list())
}

/// The refusal for a change in a folder the user did not allow changes in.
fn read_only(at: &crate::mounts::Located) -> String {
    format!("{} is in a read-only shared folder. The user can allow changes in Settings ▸ Folders.", at.guest)
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

// ---------------------------------------------------------------------------

pub struct FileRead(Arc<Ledger>, Arc<crate::mounts::Mounts>);

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
        let at = match resolve(ctx, &self.1, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let (host, guest) = (at.host.clone(), at.guest.clone());
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

pub struct FileWrite(Arc<Ledger>, Arc<crate::mounts::Mounts>);

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
        let at = match resolve(ctx, &self.1, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let (host, guest) = (at.host.clone(), at.guest.clone());
        if !at.writable {
            return ToolOutput::error(read_only(&at));
        }
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
        ToolOutput::ok(format!("{verb} {} bytes to {guest} (now {size} bytes).\nLink: {}", content.len(), at.url()))
    }
}

// ---------------------------------------------------------------------------

pub struct FileEdit(Arc<Ledger>, Arc<crate::mounts::Mounts>);

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
        let at = match resolve(ctx, &self.1, path) {
            Ok(x) => x,
            Err(e) => return ToolOutput::error(e),
        };
        let (host, guest) = (at.host.clone(), at.guest.clone());
        if !at.writable {
            return ToolOutput::error(read_only(&at));
        }
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
        ToolOutput::ok(format!("Replaced {n} occurrence{} in {guest}.\nLink: {}", if n == 1 { "" } else { "s" }, at.url()))
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
        tools(Default::default()).into_iter().find(|t| t.spec().name == name).unwrap()
    }

    #[tokio::test]
    async fn write_read_and_edit_a_file_by_any_of_its_names() {
        let (c, ws) = ctx();
        let t = tools(Default::default());
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
        let t = tools(Default::default());
        t[0].call(&c, &json!({"path": "n.txt"})).await;
        // Something else (the shell, the user) rewrites it.
        std::fs::write(ws.join("n.txt"), "one two\n").unwrap();
        let e = t[2].call(&c, &json!({"path": "n.txt", "old_string": "one", "new_string": "1"})).await;
        assert!(e.is_error && e.text.contains("changed since"), "{}", e.text);
    }

    use crate::tools::ToolSource;

    /// A shared folder next to the workspace, one table for every tool.
    fn with_mount(writable: bool) -> (ToolContext, PathBuf, PathBuf, Arc<crate::mounts::Mounts>) {
        let (c, ws) = ctx();
        let dir = std::env::temp_dir().join(format!("solos-shared-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mounts = Arc::new(crate::mounts::Mounts::default());
        mounts.set(vec![solos_api::Mount { name: "notes".into(), path: dir.to_string_lossy().into_owned(), writable }]).unwrap();
        (c, ws, dir, mounts)
    }

    fn named(mounts: &Arc<crate::mounts::Mounts>, name: &str) -> Arc<dyn Tool> {
        tools(mounts.clone())
            .into_iter()
            .chain(crate::mounts::MountTools(mounts.clone()).tools())
            .find(|t| t.spec().name == name)
            .unwrap()
    }

    #[tokio::test]
    async fn a_shared_folder_is_read_listed_and_copied_but_only_written_when_allowed() {
        let (c, ws, dir, mounts) = with_mount(false);
        std::fs::write(dir.join("day.md"), "one\ntwo\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.bin"), [0u8, 1, 2]).unwrap();

        let r = named(&mounts, "file_read").call(&c, &json!({"path": "/solos/mnt/notes/day.md"})).await;
        assert!(!r.is_error && r.text.contains("one") && r.text.starts_with("/solos/mnt/notes/day.md"), "{}", r.text);
        let l = named(&mounts, "file_list").call(&c, &json!({"path": "/solos/mnt/notes"})).await;
        assert!(l.text.contains("sub/") && l.text.contains("day.md  "), "{}", l.text);

        // Read-only: no write, no edit, no copy into it.
        for (tool, args) in [
            ("file_write", json!({"path": "/solos/mnt/notes/new.md", "content": "x"})),
            ("file_edit", json!({"path": "/solos/mnt/notes/day.md", "old_string": "one", "new_string": "1"})),
            ("file_copy", json!({"from": "/solos/mnt/notes/day.md", "to": "/solos/mnt/notes/copy.md"})),
        ] {
            let r = named(&mounts, tool).call(&c, &args).await;
            assert!(r.is_error && r.text.contains("read-only"), "{tool}: {}", r.text);
        }
        assert!(!dir.join("new.md").exists() && !dir.join("copy.md").exists());
        assert_eq!(std::fs::read_to_string(dir.join("day.md")).unwrap(), "one\ntwo\n");

        // Copying out works, binary included, whole folders too, and does not overwrite by itself.
        let r = named(&mounts, "file_copy").call(&c, &json!({"from": "/solos/mnt/notes/sub", "to": "/solos/ws/got"})).await;
        assert!(!r.is_error && r.text.contains("Copied 1 file"), "{}", r.text);
        assert_eq!(std::fs::read(ws.join("got/a.bin")).unwrap(), [0u8, 1, 2]);
        let one = json!({"from": "/solos/mnt/notes/day.md", "to": "/solos/ws/day.md"});
        assert!(!named(&mounts, "file_copy").call(&c, &one).await.is_error);
        let again = named(&mounts, "file_copy").call(&c, &one).await;
        assert!(again.is_error && again.text.contains("already exists"), "{}", again.text);
        let into = named(&mounts, "file_copy").call(&c, &json!({"from": "/solos/mnt/notes/day.md", "to": "/solos/ws/got"})).await;
        assert!(!into.is_error && ws.join("got/day.md").exists(), "an existing folder gets the copy inside: {}", into.text);

        // Allowed: the same calls now change the folder.
        let allowed: Vec<_> = mounts.list().into_iter().map(|m| solos_api::Mount { writable: true, ..m }).collect();
        mounts.set(allowed).unwrap();
        let w = named(&mounts, "file_write").call(&c, &json!({"path": "/solos/mnt/notes/new.md", "content": "x"})).await;
        assert!(!w.is_error && w.text.contains("solos://mnt/notes/new.md"), "{}", w.text);
        let e = named(&mounts, "file_read").call(&c, &json!({"path": "/solos/mnt/notes/day.md"})).await;
        assert!(!e.is_error);
        let e = named(&mounts, "file_edit").call(&c, &json!({"path": "/solos/mnt/notes/day.md", "old_string": "one", "new_string": "1"})).await;
        assert!(!e.is_error, "{}", e.text);
        assert_eq!(std::fs::read_to_string(dir.join("day.md")).unwrap(), "1\ntwo\n");
        let back = named(&mounts, "file_copy").call(&c, &json!({"from": "/solos/ws/got/a.bin", "to": "/solos/mnt/notes/sub"})).await;
        assert!(back.is_error, "a file onto an existing file needs overwrite: {}", back.text);
        let back = named(&mounts, "file_copy").call(&c, &json!({"from": "/solos/ws/got/a.bin", "to": "/solos/mnt/notes/sub", "overwrite": true})).await;
        assert!(!back.is_error, "{}", back.text);
    }

    #[tokio::test]
    async fn the_list_and_copy_tools_exist_only_while_something_is_shared() {
        let (_, _, _, mounts) = with_mount(true);
        assert_eq!(crate::mounts::MountTools(mounts.clone()).tools().len(), 2);
        mounts.set(vec![]).unwrap();
        assert!(crate::mounts::MountTools(mounts).tools().is_empty());
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
