//! Folders the user shares with the model.
//!
//! The app asks the user to pick a folder (iCloud Drive, On My iPhone, another
//! app's provider), keeps the permission the system gave, and tells the core
//! each folder's name and where it is on the device. The model then names a
//! file in it as `/solos/mnt/<name>/…` (or `solos://mnt/<name>/…`). The
//! emulated shell cannot see these folders: the file tools reach them, and
//! `file_copy` moves a file between one and the workspace.
//!
//! Every path is checked here, once: a name that is not a shared folder, a
//! `..`, or a symlink inside the folder that leads out of it is refused, and a
//! folder the user did not allow changes in is never written.

use crate::sandbox::GUEST_WORKSPACE;
use crate::tools::{object_schema, Tool, ToolContext, ToolOutput, ToolSource, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};
use solos_api::Mount;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

pub const GUEST_MOUNTS: &str = "/solos/mnt";
pub const MOUNT_URL_PREFIX: &str = "solos://mnt/";

/// The shared folders, as the app last said.
#[derive(Default)]
pub struct Mounts(RwLock<Vec<Mount>>);

impl Mounts {
    pub fn set(&self, mounts: Vec<Mount>) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for m in &mounts {
            if !valid_name(&m.name) {
                return Err(format!("\"{}\" is not a usable folder name", m.name));
            }
            if !seen.insert(m.name.to_lowercase()) {
                return Err(format!("two folders are named \"{}\"", m.name));
            }
            if !Path::new(&m.path).is_absolute() {
                return Err(format!("the path of \"{}\" is not absolute", m.name));
            }
        }
        *self.0.write().unwrap() = mounts;
        Ok(())
    }

    pub fn list(&self) -> Vec<Mount> {
        self.0.read().unwrap().clone()
    }
}

/// A folder name the model can write in a path: not empty, no separator, not
/// hidden, not `.`/`..`, no control characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '%', '?', '#'])
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

/// Where a path the model wrote is on the device.
#[derive(Debug, PartialEq, Eq)]
pub struct Located {
    pub host: PathBuf,
    /// The guest form, `/solos/ws/…` or `/solos/mnt/<name>/…`.
    pub guest: String,
    pub writable: bool,
    /// The shared folder it is in, if it is in one.
    pub mount: Option<String>,
}

impl Located {
    /// The `solos://` address of the file.
    pub fn url(&self) -> String {
        match self.guest.strip_prefix(GUEST_MOUNTS) {
            Some(rest) => format!("solos://mnt{rest}"),
            None => self.guest.replacen(GUEST_WORKSPACE, "solos://ws", 1),
        }
    }
}

/// `/solos/ws/x`, `solos://ws/x`, `/solos/mnt/<name>/x`, `solos://mnt/<name>/x`,
/// or `x` (relative to the workspace). The roots themselves are allowed.
pub fn locate(path: &str, workspace: &Path, mounts: &[Mount]) -> Result<Located, String> {
    let path = path.trim();
    let url = path.starts_with("solos://");
    let (kind, rest) = if let Some(r) = path.strip_prefix(MOUNT_URL_PREFIX) {
        ('m', r)
    } else if let Some(r) = path.strip_prefix(&format!("{GUEST_MOUNTS}/")) {
        ('m', r)
    } else if path == GUEST_MOUNTS || path == "solos://mnt" {
        return Err(mounts_hint(mounts));
    } else if let Some(r) = path.strip_prefix(crate::files::URL_PREFIX) {
        ('w', r)
    } else if path == "solos://ws" {
        ('w', "")
    } else if let Some(r) = path.strip_prefix(GUEST_WORKSPACE) {
        match r.strip_prefix('/') {
            Some(r) => ('w', r),
            None if r.is_empty() => ('w', ""),
            None => return Err(not_shared(path)),
        }
    } else if path.starts_with('/') {
        return Err(not_shared(path));
    } else {
        ('w', path.trim_start_matches("./"))
    };
    // A URL may carry a query or fragment and percent-escapes.
    let rest = if url { rest.split(['?', '#']).next().unwrap_or("") } else { rest };
    let rest = if url { percent_decode(rest).ok_or_else(|| not_shared(path))? } else { rest.to_string() };

    let (root, base_guest, writable, mount, sub) = if kind == 'm' {
        let (name, sub) = rest.split_once('/').unwrap_or((rest.as_str(), ""));
        let m = mounts.iter().find(|m| m.name == name).ok_or_else(|| {
            format!("There is no shared folder named \"{name}\". {}", mounts_hint(mounts))
        })?;
        (PathBuf::from(&m.path), format!("{GUEST_MOUNTS}/{}", m.name), m.writable, Some(m.name.clone()), sub.to_string())
    } else {
        (workspace.to_path_buf(), GUEST_WORKSPACE.to_string(), true, None, rest)
    };

    let mut host = root.clone();
    let mut guest = base_guest;
    for c in Path::new(&sub).components() {
        match c {
            Component::Normal(part) => {
                host.push(part);
                guest.push('/');
                guest.push_str(&part.to_string_lossy());
            }
            Component::CurDir => {}
            _ => return Err(format!("{path} leaves the folder it is in; `..` is not allowed; use `shell` for other paths.")),
        }
    }
    if mount.is_some() && !inside(&root, &host) {
        return Err(format!("{guest} leads outside the shared folder (through a link), so it is refused."));
    }
    Ok(Located { host, guest, writable, mount })
}

fn not_shared(path: &str) -> String {
    format!("{path} is not in the workspace ({GUEST_WORKSPACE}) or a shared folder ({GUEST_MOUNTS}/<name>); use `shell` for other paths.")
}

fn mounts_hint(mounts: &[Mount]) -> String {
    if mounts.is_empty() {
        "The user has not shared any folder.".to_string()
    } else {
        let names: Vec<String> = mounts.iter().map(|m| format!("{GUEST_MOUNTS}/{}", m.name)).collect();
        format!("Shared folders: {}.", names.join(", "))
    }
}

/// Whether `path`, with every link followed as far as it exists, is still
/// under `root`.
fn inside(root: &Path, path: &Path) -> bool {
    let Ok(root) = root.canonicalize() else { return false };
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(real) = probe.canonicalize() {
            return real.starts_with(&root);
        }
        if !probe.pop() {
            return false;
        }
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// What the system prompt says about the shared folders; nothing when there
/// are none.
pub fn prompt_section(mounts: &[Mount]) -> String {
    if mounts.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n\nShared folders (the user chose to share these from their device; `shell` cannot see them):\n"
    );
    for m in mounts {
        out.push_str(&format!(
            "- {GUEST_MOUNTS}/{} ({})\n",
            m.name,
            if m.writable { "changes allowed" } else { "read-only" }
        ));
    }
    out.push_str(
        "Reach them with `file_list`, `file_read`, `file_write`, `file_edit` and `file_copy`. To run a script on a file there, \
`file_copy` it into /solos/ws first, and copy the result back; `read_image` also needs the picture copied into /solos/ws. \
Never change a read-only folder, and do not change a file in a shared folder the user did not ask you to.",
    );
    out
}

// ---------------------------------------------------------------------------
// file_list and file_copy: there only when something is shared

pub struct MountTools(pub Arc<Mounts>);

impl ToolSource for MountTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        if self.0.list().is_empty() {
            return vec![];
        }
        vec![Arc::new(FileList(self.0.clone())), Arc::new(FileCopy(self.0.clone()))]
    }
}

const LIST_LIMIT: usize = 200;

pub struct FileList(Arc<Mounts>);

#[async_trait]
impl Tool for FileList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_list".into(),
            description: "List a folder in the workspace or in a shared folder: its files and sub-folders with sizes. \
                Needed for shared folders, which `shell` cannot see. Folders come first; at most 200 entries."
                .into(),
            schema: object_schema(
                json!({"path": {"type": "string", "description": "The folder, e.g. /solos/mnt/notes or /solos/ws. Default /solos/ws."}}),
                &[],
            ),
            parallel: true,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let path = input.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()).unwrap_or(GUEST_WORKSPACE);
        let at = match locate(path, &ctx.sandbox.workspace_dir(), &self.0.list()) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(e),
        };
        let read = match std::fs::read_dir(&at.host) {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Cannot list {}: {e}", at.guest)),
        };
        let mut rows: Vec<(bool, String, u64)> = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().ok();
            let dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            rows.push((dir, name, meta.map(|m| m.len()).unwrap_or(0)));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        let total = rows.len();
        let mut out = format!("{} · {total} item{}\n", at.guest, if total == 1 { "" } else { "s" });
        for (dir, name, size) in rows.iter().take(LIST_LIMIT) {
            if *dir {
                out.push_str(&format!("{name}/\n"));
            } else if let Some(real) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".icloud")) {
                // iCloud Drive's stand-in for a file that is not on the device.
                out.push_str(&format!("{real}  (in iCloud, not downloaded to this device)\n"));
            } else {
                out.push_str(&format!("{name}  {}\n", human_size(*size)));
            }
        }
        if total > LIST_LIMIT {
            out.push_str(&format!("… {} more not shown\n", total - LIST_LIMIT));
        }
        ToolOutput::ok(out)
    }
}

pub struct FileCopy(Arc<Mounts>);

#[async_trait]
impl Tool for FileCopy {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_copy".into(),
            description: "Copy a file or a folder (with what is in it) between the workspace and shared folders, or within \
                one. The way to use a file from a shared folder with `shell`, or to put a result back. Nothing passes \
                through the conversation, so binary and large files are fine. Never writes into a read-only shared folder."
                .into(),
            schema: object_schema(
                json!({
                    "from": {"type": "string", "description": "The file or folder to copy."},
                    "to": {"type": "string", "description": "Where to put it. An existing folder gets the copy inside it, under the same name."},
                    "overwrite": {"type": "boolean", "description": "Replace a file that is already there. Default false."}
                }),
                &["from", "to"],
            ),
            parallel: false,
        }
    }

    async fn call(&self, ctx: &ToolContext, input: &Value) -> ToolOutput {
        let (Some(from), Some(to)) = (input.get("from").and_then(Value::as_str), input.get("to").and_then(Value::as_str)) else {
            return ToolOutput::error("`from` and `to` are required.");
        };
        let overwrite = input.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
        let (ws, mounts) = (ctx.sandbox.workspace_dir(), self.0.list());
        let src = match locate(from, &ws, &mounts) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(e),
        };
        let mut dst = match locate(to, &ws, &mounts) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(e),
        };
        if !dst.writable {
            return ToolOutput::error(format!("{} is in a read-only shared folder. The user can allow changes in Settings ▸ Folders.", dst.guest));
        }
        if !src.host.exists() {
            return ToolOutput::error(format!("{} does not exist.", src.guest));
        }
        if dst.host.is_dir() {
            if let Some(name) = src.host.file_name() {
                dst = match locate(&format!("{}/{}", dst.guest, name.to_string_lossy()), &ws, &mounts) {
                    Ok(d) => d,
                    Err(e) => return ToolOutput::error(e),
                };
            }
        }
        if dst.host.starts_with(&src.host) {
            return ToolOutput::error("A folder cannot be copied into itself.");
        }
        let mut done = Copied::default();
        match copy_tree(&src.host, &dst.host, overwrite, &mut done) {
            Ok(()) => ToolOutput::ok(format!(
                "Copied {} file{} ({}) from {} to {}.",
                done.files,
                if done.files == 1 { "" } else { "s" },
                human_size(done.bytes),
                src.guest,
                dst.guest
            )),
            Err(e) => ToolOutput::error(format!("{e} ({} file{} copied before it stopped.)", done.files, if done.files == 1 { "" } else { "s" })),
        }
    }
}

#[derive(Default)]
struct Copied {
    files: u64,
    bytes: u64,
}

fn copy_tree(src: &Path, dst: &Path, overwrite: bool, done: &mut Copied) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("Cannot read {}: {e}", src.display()))?;
    if meta.file_type().is_symlink() {
        return Ok(()); // links are not followed
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("Cannot create {}: {e}", dst.display()))?;
        let mut names: Vec<_> = std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            copy_tree(&src.join(&name), &dst.join(&name), overwrite, done)?;
        }
        return Ok(());
    }
    if dst.exists() && !overwrite {
        return Err(format!("{} already exists; pass overwrite: true to replace it.", dst.display()));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    let n = std::fs::copy(src, dst).map_err(|e| format!("Cannot copy {}: {e}", src.display()))?;
    done.files += 1;
    done.bytes += n;
    Ok(())
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("solos-mnt-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn mount(name: &str, path: &Path, writable: bool) -> Mount {
        Mount { name: name.into(), path: path.to_string_lossy().into_owned(), writable }
    }

    #[test]
    fn names_and_paths_resolve_into_the_workspace_or_a_named_folder() {
        let (ws, notes) = (dir("ws"), dir("notes"));
        let ms = vec![mount("notes", &notes, false)];
        let w = locate("/solos/ws/a/b.txt", &ws, &ms).unwrap();
        assert_eq!((w.host.clone(), w.guest.as_str(), w.writable, w.mount.clone()), (ws.join("a/b.txt"), "/solos/ws/a/b.txt", true, None));
        assert_eq!(locate("a.txt", &ws, &ms).unwrap().host, ws.join("a.txt"));
        assert_eq!(locate("solos://ws/x%20y.txt", &ws, &ms).unwrap().host, ws.join("x y.txt"));
        let m = locate("/solos/mnt/notes/2026/day.md", &ws, &ms).unwrap();
        assert_eq!((m.host.clone(), m.writable, m.mount.as_deref()), (notes.join("2026/day.md"), false, Some("notes")));
        assert_eq!(m.url(), "solos://mnt/notes/2026/day.md");
        assert_eq!(locate("solos://mnt/notes/a%20b.md", &ws, &ms).unwrap().host, notes.join("a b.md"));
        assert_eq!(locate("/solos/mnt/notes", &ws, &ms).unwrap().host, notes, "a folder's root is a path too");
        assert_eq!(locate("/solos/ws", &ws, &ms).unwrap().host, ws);
    }

    #[test]
    fn what_is_not_shared_or_leaves_its_folder_is_refused() {
        let (ws, notes) = (dir("ws"), dir("notes"));
        let ms = vec![mount("notes", &notes, true)];
        for bad in ["/etc/passwd", "/solos/wsx/a", "/solos/mnt/other/a", "/solos/mnt", "/solos/mnt/notes/../x", "/solos/ws/../x", "solos://mnt/notes/%2e%2e/x"] {
            assert!(locate(bad, &ws, &ms).is_err(), "{bad}");
        }
        assert!(locate("/solos/mnt/other/a", &ws, &ms).unwrap_err().contains("/solos/mnt/notes"), "the error names the real folders");
        assert!(locate("/solos/mnt/x/a", &ws, &[]).unwrap_err().contains("not shared any folder"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_a_shared_folder_cannot_lead_out_of_it() {
        let (ws, notes, secret) = (dir("ws"), dir("notes"), dir("secret"));
        std::fs::write(secret.join("key.txt"), "s").unwrap();
        std::os::unix::fs::symlink(&secret, notes.join("out")).unwrap();
        std::fs::create_dir_all(notes.join("real")).unwrap();
        let ms = vec![mount("notes", &notes, true)];
        assert!(locate("/solos/mnt/notes/out/key.txt", &ws, &ms).is_err());
        assert!(locate("/solos/mnt/notes/out/new.txt", &ws, &ms).is_err(), "nor a file made through it");
        assert!(locate("/solos/mnt/notes/real/new.txt", &ws, &ms).is_ok());
    }

    #[test]
    fn the_table_refuses_unusable_names_duplicates_and_relative_paths() {
        let t = Mounts::default();
        let p = std::env::temp_dir();
        assert!(t.set(vec![mount("a/b", &p, true)]).is_err());
        assert!(t.set(vec![mount(".hidden", &p, true)]).is_err());
        assert!(t.set(vec![mount("", &p, true)]).is_err());
        assert!(t.set(vec![mount("A", &p, true), mount("a", &p, true)]).is_err());
        assert!(t.set(vec![Mount { name: "x".into(), path: "rel".into(), writable: true }]).is_err());
        assert!(t.set(vec![mount("笔记", &p, true), mount("My Notes", &p, false)]).is_ok());
        assert_eq!(t.list().len(), 2);
    }

    #[test]
    fn the_prompt_says_nothing_without_shared_folders() {
        assert_eq!(prompt_section(&[]), "");
        let s = prompt_section(&[mount("notes", Path::new("/p"), false)]);
        assert!(s.contains("/solos/mnt/notes (read-only)") && s.contains("file_copy"));
    }
}
