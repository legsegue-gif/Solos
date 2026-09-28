//! Files the model shows the user.
//!
//! The model names a workspace file as `solos://ws/<path>` (or by its guest
//! path, `/solos/ws/<path>`); the app asks the core where that file is on
//! the device. Nothing outside the workspace is ever answered, whatever the
//! path says.

use std::path::{Component, Path, PathBuf};

pub const URL_PREFIX: &str = "solos://ws/";

/// The host path of a workspace file named by URL or guest path, or `None`
/// when it is not one or would leave the workspace.
pub fn resolve(reference: &str, workspace: &Path) -> Option<PathBuf> {
    let reference = reference.trim();
    let rel = reference
        .strip_prefix(URL_PREFIX)
        .or_else(|| reference.strip_prefix(crate::sandbox::GUEST_WORKSPACE).and_then(|r| r.strip_prefix('/')))?;
    // A URL may carry a query or fragment, and percent-escapes for spaces
    // and non-ASCII names.
    let rel = rel.split(['?', '#']).next().unwrap_or("");
    let rel = percent_decode(rel)?;
    let mut out = workspace.to_path_buf();
    for c in Path::new(&rel).components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            // `..`, a root or a prefix: refuse rather than guess.
            _ => return None,
        }
    }
    (out != workspace).then_some(out)
}

/// As [`resolve`], and also taking a path relative to the workspace, the
/// way a model names a file a tool argument points at.
pub fn resolve_argument(path: &str, workspace: &Path) -> Option<PathBuf> {
    let path = path.trim();
    if path.starts_with(URL_PREFIX) || path.starts_with('/') {
        resolve(path, workspace)
    } else {
        resolve(&format!("{}/{}", crate::sandbox::GUEST_WORKSPACE, path.trim_start_matches("./")), workspace)
    }
}

/// The `solos://ws/` address of a guest path inside the workspace, the one
/// form the user can open from a reply; `None` outside it. ASCII that would
/// end a Markdown link or a URL (spaces, brackets, `%`, `?`, `#`) is escaped;
/// other text is left readable.
pub fn url_for(guest_path: &str) -> Option<String> {
    let rel = guest_path.trim().strip_prefix(crate::sandbox::GUEST_WORKSPACE)?.strip_prefix('/')?;
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut out = String::from(URL_PREFIX);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        for c in part.chars() {
            if c.is_ascii() && !(c.is_ascii_alphanumeric() || "-._~!$&'*+,;=:@".contains(c)) {
                out.push_str(&format!("%{:02X}", c as u32));
            } else {
                out.push(c);
            }
        }
    }
    Some(out)
}

/// Copy a file the user attached into `<workspace>/attachments/`, under its
/// own name when free and `name (2).ext` style otherwise. Returns the guest
/// path and the size.
pub fn import_attachment(workspace: &Path, source: &Path, name: &str) -> std::io::Result<(String, u64)> {
    let dir = workspace.join("attachments");
    std::fs::create_dir_all(&dir)?;
    // Only the last component of the name: it comes from outside.
    let name = Path::new(name).file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty()).unwrap_or("file");
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut candidate = name.to_string();
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{stem} ({n}){ext}");
        n += 1;
    }
    let size = std::fs::copy(source, dir.join(&candidate))?;
    Ok((format!("{}/attachments/{candidate}", crate::sandbox::GUEST_WORKSPACE), size))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_path_gets_the_address_that_resolves_back_to_it() {
        let ws = Path::new("/w");
        for path in ["/solos/ws/report.png", "/solos/ws/a b (1).png", "/solos/ws/报告/图.png", "/solos/ws/x%y#z?.txt"] {
            let url = url_for(path).unwrap();
            assert!(url.starts_with("solos://ws/") && !url.contains(' ') && !url.contains('('), "{url}");
            let rel = path.strip_prefix("/solos/ws/").unwrap();
            assert_eq!(resolve(&url, ws), Some(ws.join(rel)), "{url}");
        }
        assert_eq!(url_for("/solos/ws/报告/图.png").as_deref(), Some("solos://ws/报告/图.png"));
        assert_eq!(url_for("/etc/hosts"), None);
        assert_eq!(url_for("/solos/ws"), None);
        assert_eq!(url_for("/solos/ws/../etc"), None);
    }

    #[test]
    fn urls_and_guest_paths_map_into_the_workspace() {
        let ws = Path::new("/data/workspace");
        assert_eq!(resolve("solos://ws/out/chart.png", ws), Some(ws.join("out/chart.png")));
        assert_eq!(resolve("/solos/ws/hello.txt", ws), Some(ws.join("hello.txt")));
        assert_eq!(resolve("solos://ws/%E6%8A%A5%E5%91%8A%20v2.md?x=1#top", ws), Some(ws.join("报告 v2.md")));
        assert_eq!(resolve("solos://ws/报告.md", ws), Some(ws.join("报告.md")));
    }

    #[test]
    fn attachments_keep_their_names_and_never_overwrite() {
        let root = std::env::temp_dir().join(format!("solos-files-{}", uuid::Uuid::new_v4()));
        let ws = root.join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("src.txt");
        std::fs::write(&src, "hi").unwrap();
        assert_eq!(import_attachment(&ws, &src, "report.txt").unwrap(), ("/solos/ws/attachments/report.txt".into(), 2));
        assert_eq!(import_attachment(&ws, &src, "report.txt").unwrap().0, "/solos/ws/attachments/report (2).txt");
        assert_eq!(import_attachment(&ws, &src, "../../etc/passwd").unwrap().0, "/solos/ws/attachments/passwd");
        assert!(ws.join("attachments/report (2).txt").exists());
    }

    #[test]
    fn nothing_outside_the_workspace_is_answered() {
        let ws = Path::new("/data/workspace");
        assert_eq!(resolve("solos://ws/../solos.db", ws), None);
        assert_eq!(resolve("solos://ws/a/%2E%2E/%2E%2E/x", ws), None);
        assert_eq!(resolve("solos://ws/", ws), None);
        assert_eq!(resolve("/etc/passwd", ws), None);
        assert_eq!(resolve("https://example.com/a.png", ws), None);
        assert_eq!(resolve("solos://ws/bad%zz", ws), None);
    }
}
