//! Skills: folders of instructions, and often scripts, that the model reads
//! when a task calls for one — a `SKILL.md` with a `name` and a
//! `description` at the top, the format other agents share. They live in the
//! workspace (`/solos/ws/skills/<folder>`), so the model's tools, the file
//! browser and `solos://` links reach them, and the folder on disk is the
//! only record of a skill: whatever puts one there installs it.

use crate::sandbox::GUEST_WORKSPACE;
use solos_api::{CoreError, Skill};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// The skills' folder, inside the workspace.
pub const FOLDER: &str = "skills";
pub const FILE: &str = "SKILL.md";

/// A skill is text and small scripts; these keep a wrong link (a whole
/// monorepo, a release with binaries) from filling the device.
const MAX_DOWNLOAD: u64 = 50 << 20;
const MAX_UNPACKED: u64 = 100 << 20;
const MAX_FILES: usize = 2_000;
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
/// Only the front matter is needed to list a skill.
const MAX_LISTED_READ: u64 = 256 << 10;
/// Listed in the prompt; past these the model is pointed at the folder.
const PROMPT_SKILLS: usize = 50;
const PROMPT_DESCRIPTION_CHARS: usize = 300;

pub fn guest_dir() -> String {
    format!("{GUEST_WORKSPACE}/{FOLDER}")
}

pub fn host_dir(workspace: &Path) -> PathBuf {
    workspace.join(FOLDER)
}

/// Every folder in the skills folder that holds a `SKILL.md`, by folder name.
pub fn list(workspace: &Path, disabled: &BTreeSet<String>) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(host_dir(workspace)) else {
        return vec![];
    };
    let mut out: Vec<Skill> = entries
        .flatten()
        .filter_map(|e| {
            let folder = e.file_name().to_str()?.to_string();
            if folder.starts_with('.') {
                return None;
            }
            let text = read_head(&e.path().join(FILE))?;
            Some(describe(folder, &text, disabled))
        })
        .collect();
    out.sort_by(|a, b| a.folder.cmp(&b.folder));
    out
}

fn read_head(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_LISTED_READ).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn describe(folder: String, skill_md: &str, disabled: &BTreeSet<String>) -> Skill {
    let fm = front_matter(skill_md);
    let name = fm.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| folder.clone());
    Skill {
        path: format!("{}/{folder}", guest_dir()),
        enabled: !disabled.contains(&folder),
        description: fm.get("description").cloned().unwrap_or_default(),
        name,
        folder,
    }
}

/// The top-level `key: value` pairs of a `---` block at the top of a
/// Markdown file. Enough YAML for skills' front matter: plain and quoted
/// scalars, values continued on indented lines, and `|` / `>` blocks.
/// Nested maps become their key's text, which no caller reads.
pub fn front_matter(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return out;
    }
    // The key being read, whether its lines join with newlines, and its lines.
    let mut current: Option<(String, bool, Vec<String>)> = None;
    let flush = |current: &mut Option<(String, bool, Vec<String>)>, out: &mut BTreeMap<String, String>| {
        if let Some((key, literal, parts)) = current.take() {
            let joined = if literal {
                parts.join("\n").trim_end().to_string()
            } else {
                parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
            };
            out.insert(key, unquote(&joined));
        }
    };
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            break;
        }
        if line.starts_with([' ', '\t']) || trimmed.is_empty() {
            if let Some((_, _, parts)) = current.as_mut() {
                parts.push(line.trim().to_string());
            }
            continue;
        }
        flush(&mut current, &mut out);
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim().to_string(), value.trim());
        current = match value {
            "|" | "|-" | "|+" => Some((key, true, vec![])),
            ">" | ">-" | ">+" => Some((key, false, vec![])),
            _ => Some((key, false, vec![value.to_string()])),
        };
    }
    flush(&mut current, &mut out);
    out
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        return v[1..v.len() - 1].replace("\\\"", "\"").replace("\\n", "\n").replace("\\\\", "\\");
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

/// A folder name from a skill's name: lower case, letters, digits, `-`,
/// `_` and `.`, at most 64 characters; `None` when nothing usable is left
/// (a name in another script).
pub fn folder_name(raw: &str) -> Option<String> {
    let mut s = String::new();
    for c in raw.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_matches(['-', '.']).chars().take(64).collect();
    let s = s.trim_end_matches(['-', '.']).to_string();
    (!s.is_empty()).then_some(s)
}

/// A folder the user or the model names: one plain path component.
fn checked_folder(folder: &str) -> Result<&str, CoreError> {
    let ok = !folder.is_empty() && !folder.starts_with('.') && !folder.contains(['/', '\\']);
    if ok { Ok(folder) } else { Err(CoreError::NoSuchSkill { folder: folder.to_string() }) }
}

/// A skill's `SKILL.md`, whole.
pub fn instructions(workspace: &Path, folder: &str) -> Result<String, CoreError> {
    let path = host_dir(workspace).join(checked_folder(folder)?).join(FILE);
    std::fs::read(&path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .map_err(|_| CoreError::NoSuchSkill { folder: folder.to_string() })
}

pub fn remove(workspace: &Path, folder: &str) -> Result<(), CoreError> {
    let dir = host_dir(workspace).join(checked_folder(folder)?);
    if !dir.join(FILE).is_file() {
        return Err(CoreError::NoSuchSkill { folder: folder.to_string() });
    }
    std::fs::remove_dir_all(&dir).map_err(|e| CoreError::Storage { detail: e.to_string() })
}

/// Where a skill is on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubSource {
    pub owner: String,
    pub repo: String,
    /// A branch, tag or commit; `HEAD` for the default branch.
    pub reference: String,
    /// The folder within the repository the link points at; empty for the
    /// whole repository.
    pub path: String,
}

impl GitHubSource {
    pub fn archive_url(&self) -> String {
        format!("https://codeload.github.com/{}/{}/zip/{}", self.owner, self.repo, self.reference)
    }

    /// A link to a folder in the same repository.
    pub fn folder_link(&self, folder: &str) -> String {
        let base = format!("https://github.com/{}/{}", self.owner, self.repo);
        if folder.is_empty() { base } else { format!("{base}/tree/{}/{folder}", self.reference) }
    }
}

/// Reads `https://github.com/<owner>/<repo>`, with `/tree/<ref>/<folder>` or
/// `/blob/<ref>/<file>` after it, with or without the scheme, and the
/// `<owner>/<repo>` shorthand. A branch name with a slash in it is read as
/// its first part; such links have to name a commit or tag instead.
pub fn parse_source(source: &str) -> Option<GitHubSource> {
    let s = source.trim().split(['?', '#']).next()?.trim_end_matches('/');
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let rest = match s.strip_prefix("github.com/") {
        Some(rest) => rest,
        // `owner/repo`; an owner with a dot would be a domain.
        None if s.split('/').count() == 2 && !s.contains("://") && !s.split('/').next()?.contains('.') => s,
        None => return None,
    };
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let plain = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    let (owner, repo) = (*parts.first()?, parts.get(1)?.trim_end_matches(".git"));
    if !plain(owner) || !plain(repo) {
        return None;
    }
    let (reference, path) = match parts.get(2..) {
        Some([]) | None => ("HEAD".to_string(), String::new()),
        Some([kind, reference, rest @ ..]) if *kind == "tree" || *kind == "blob" => {
            let mut path: Vec<&str> = rest.to_vec();
            // A file link means the folder it is in.
            if *kind == "blob" {
                path.pop();
            }
            (reference.to_string(), path.join("/"))
        }
        _ => return None,
    };
    Some(GitHubSource { owner: owner.into(), repo: repo.into(), reference, path })
}

/// What an install did.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub folder: String,
    /// The skill's files, relative to its folder.
    pub files: Vec<String>,
    /// An earlier copy of the folder was replaced.
    pub replaced: bool,
    pub instructions: String,
}

/// Download a skill from GitHub and put it in the skills folder, replacing
/// any earlier copy of the same folder.
pub async fn install(source: &str, workspace: &Path) -> Result<Installed, CoreError> {
    let src = parse_source(source).ok_or_else(|| CoreError::NotASkillSource { source_text: source.to_string() })?;
    let archive = tokio::time::timeout(DOWNLOAD_TIMEOUT, download(&src.archive_url()))
        .await
        .map_err(|_| CoreError::Network { detail: format!("no complete download in {} s", DOWNLOAD_TIMEOUT.as_secs()) })??;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || unpack(&archive, &src, &workspace))
        .await
        .map_err(|e| CoreError::Internal { detail: e.to_string() })?
}

async fn download(url: &str) -> Result<Vec<u8>, CoreError> {
    use futures::StreamExt;
    let resp = crate::providers::http::client()
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::Network { detail: e.to_string() })?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let detail = if status == 404 { "repository, branch or folder not found (or not public)".to_string() } else { resp.text().await.unwrap_or_default() };
        return Err(CoreError::Http { status, detail });
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CoreError::Network { detail: format!("after {} bytes: {e}", body.len()) })?;
        body.extend_from_slice(&chunk);
        if body.len() as u64 > MAX_DOWNLOAD {
            return Err(CoreError::Network { detail: format!("the archive is larger than {} MB", MAX_DOWNLOAD >> 20) });
        }
    }
    Ok(body)
}

/// One file of the archive, by its path within the repository.
struct Entry {
    index: usize,
    path: PathBuf,
}

/// Picks the skill's folder out of a GitHub archive (whose entries all sit
/// under one top folder) and writes it to the skills folder.
pub fn unpack(archive: &[u8], src: &GitHubSource, workspace: &Path) -> Result<Installed, CoreError> {
    let bad = |e: zip::result::ZipError| CoreError::Protocol { detail: format!("the archive could not be read: {e}") };
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(bad)?;
    let mut files = Vec::new();
    for index in 0..zip.len() {
        let f = zip.by_index(index).map_err(bad)?;
        if f.is_dir() || f.is_symlink() {
            continue;
        }
        let Some(name) = f.enclosed_name() else { continue };
        // Drop the archive's own top folder (`<repo>-<ref>/`).
        let path: PathBuf = name.components().skip(1).collect();
        if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_))) {
            continue;
        }
        files.push(Entry { index, path });
    }

    let wanted = Path::new(&src.path);
    let hidden = |p: &Path| p.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
    let mut candidates: Vec<PathBuf> = files
        .iter()
        .filter(|e| e.path.starts_with(wanted) && e.path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(FILE)))
        .filter_map(|e| e.path.parent().map(Path::to_path_buf))
        .filter(|dir| !hidden(dir))
        .collect();
    candidates.sort();
    let dir = if candidates.iter().any(|c| c == wanted) {
        wanted.to_path_buf()
    } else if candidates.len() == 1 {
        candidates.remove(0)
    } else {
        return Err(CoreError::NoSingleSkill {
            source_text: src.folder_link(&src.path),
            candidates: candidates.iter().map(|c| src.folder_link(&c.to_string_lossy())).collect(),
        });
    };

    let chosen: Vec<&Entry> = files.iter().filter(|e| e.path.starts_with(&dir)).collect();
    if chosen.len() > MAX_FILES {
        return Err(CoreError::Protocol { detail: format!("the skill has {} files; at most {MAX_FILES} are taken", chosen.len()) });
    }
    let mut total = 0u64;
    for e in &chosen {
        total += zip.by_index(e.index).map_err(bad)?.size();
    }
    if total > MAX_UNPACKED {
        return Err(CoreError::Protocol { detail: format!("the skill unpacks to {} MB; at most {} MB are taken", total >> 20, MAX_UNPACKED >> 20) });
    }

    let skill_md = chosen
        .iter()
        .find(|e| e.path.parent() == Some(dir.as_path()) && e.path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(FILE)))
        .map(|e| e.index)
        .ok_or_else(|| CoreError::Internal { detail: "the chosen folder lost its SKILL.md".into() })?;
    let mut instructions = String::new();
    zip.by_index(skill_md).map_err(bad)?.read_to_string(&mut instructions).map_err(|e| CoreError::Protocol { detail: e.to_string() })?;
    let source_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| src.repo.clone());
    let folder = front_matter(&instructions)
        .get("name")
        .and_then(|n| folder_name(n))
        .or_else(|| folder_name(&source_name))
        .unwrap_or_else(|| "skill".into());

    let io = |e: std::io::Error| CoreError::Storage { detail: e.to_string() };
    let root = host_dir(workspace);
    std::fs::create_dir_all(&root).map_err(io)?;
    let staging = root.join(format!(".installing-{}", uuid::Uuid::new_v4()));
    let written = (|| -> Result<Vec<String>, CoreError> {
        let mut names = Vec::new();
        for e in &chosen {
            let rel = e.path.strip_prefix(&dir).expect("filtered by prefix");
            // `SKILL.md` keeps its usual spelling whatever the archive had.
            let rel = if rel.as_os_str().eq_ignore_ascii_case(FILE) { Path::new(FILE) } else { rel };
            let target = staging.join(rel);
            std::fs::create_dir_all(target.parent().expect("inside staging")).map_err(io)?;
            let mut f = zip.by_index(e.index).map_err(bad)?;
            let executable = f.unix_mode().is_some_and(|m| m & 0o111 != 0);
            let mut out = std::fs::File::create(&target).map_err(io)?;
            std::io::copy(&mut f, &mut out).map_err(io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 })).map_err(io)?;
            }
            let _ = executable;
            names.push(rel.to_string_lossy().into_owned());
        }
        Ok(names)
    })();
    let mut names = match written {
        Ok(names) => names,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    let dest = root.join(&folder);
    let replaced = dest.exists();
    if replaced {
        std::fs::remove_dir_all(&dest).map_err(io)?;
    }
    std::fs::rename(&staging, &dest).map_err(io)?;
    names.sort();
    Ok(Installed { folder, files: names, replaced, instructions })
}

/// The system prompt's part about skills. Always there, so the model
/// installs the first skill the way the app knows about (measured against
/// the reference app: without such guidance its model downloaded an MCP
/// server by hand, and the app never knew of it).
pub fn prompt_section(skills: &[Skill]) -> String {
    let dir = guest_dir();
    let mut out = format!(
        "\n\nSkills:\n\
- A skill is a folder in {dir}/ with a SKILL.md: instructions for a kind of task, often with scripts beside it. When a task matches an installed skill, read its SKILL.md with `file_read` first and follow it; run its scripts from its folder.\n\
- To install a skill from a GitHub link, use `skill_install`; it puts the skill in {dir}/ and returns its SKILL.md. Do not download skills by hand. A skill you write yourself goes in its own folder there, with `name` and `description` at the top of its SKILL.md.\n"
    );
    let on: Vec<&Skill> = skills.iter().filter(|s| s.enabled).collect();
    if on.is_empty() {
        out.push_str("- No skills are installed.");
        return out;
    }
    out.push_str("Installed skills:");
    for s in on.iter().take(PROMPT_SKILLS) {
        let mut description: String = s.description.chars().take(PROMPT_DESCRIPTION_CHARS).collect();
        if description.len() < s.description.len() {
            description.push('…');
        }
        if description.is_empty() {
            out.push_str(&format!("\n- {} ({}/{FILE})", s.name, s.path));
        } else {
            out.push_str(&format!("\n- {}: {description} ({}/{FILE})", s.name, s.path));
        }
    }
    if on.len() > PROMPT_SKILLS {
        out.push_str(&format!("\n- … and {} more in {dir}/", on.len() - PROMPT_SKILLS));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solos-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A GitHub-style archive: every entry under `<repo>-HEAD/`.
    fn archive(files: &[(&str, &str, u32)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            for (path, body, mode) in files {
                let opts = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
                z.start_file(format!("repo-HEAD/{path}"), opts).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn src(path: &str) -> GitHubSource {
        GitHubSource { owner: "o".into(), repo: "repo".into(), reference: "HEAD".into(), path: path.into() }
    }

    #[test]
    fn front_matter_reads_plain_quoted_and_block_values() {
        let fm = front_matter(
            "---\nname: 12306-skill\ndescription: \"查询车票: 余票\"\nlong: >\n  one\n  two\nlit: |\n  a\n  b\nmetadata:\n  author: x\n---\n# Body\nname: not this",
        );
        assert_eq!(fm["name"], "12306-skill");
        assert_eq!(fm["description"], "查询车票: 余票");
        assert_eq!(fm["long"], "one two");
        assert_eq!(fm["lit"], "a\nb");
        assert!(!fm.contains_key("author"));
        assert!(front_matter("# no front matter\nname: x").is_empty());
        assert_eq!(front_matter("---\ndescription: first\n  continued\n---")["description"], "first continued");
    }

    #[test]
    fn links_name_a_repository_and_a_folder() {
        let p = parse_source("https://github.com/Joooook/12306-skill").unwrap();
        assert_eq!((p.owner.as_str(), p.repo.as_str(), p.reference.as_str(), p.path.as_str()), ("Joooook", "12306-skill", "HEAD", ""));
        assert_eq!(p.archive_url(), "https://codeload.github.com/Joooook/12306-skill/zip/HEAD");
        let t = parse_source("github.com/anthropics/skills/tree/main/skills/pdf/").unwrap();
        assert_eq!((t.reference.as_str(), t.path.as_str()), ("main", "skills/pdf"));
        let b = parse_source("https://github.com/a/b/blob/v1/x/SKILL.md?plain=1").unwrap();
        assert_eq!((b.reference.as_str(), b.path.as_str()), ("v1", "x"));
        assert_eq!(parse_source("a/b.git").unwrap().repo, "b");
        assert_eq!(parse_source("https://github.com/a/b.git").unwrap().repo, "b");
        assert!(parse_source("https://gitlab.com/a/b").is_none());
        assert!(parse_source("https://github.com/a/b/issues/3").is_none());
        assert!(parse_source("https://github.com/a").is_none());
        assert!(parse_source("example.com/x").is_none());
        assert_eq!(src("").folder_link("skills/pdf"), "https://github.com/o/repo/tree/HEAD/skills/pdf");
    }

    #[test]
    fn folder_names_are_plain() {
        assert_eq!(folder_name("PDF Tools!").as_deref(), Some("pdf-tools"));
        assert_eq!(folder_name("12306-skill").as_deref(), Some("12306-skill"));
        assert_eq!(folder_name("火车票"), None);
        assert_eq!(folder_name("../etc").as_deref(), Some("etc"));
    }

    #[test]
    fn a_repository_with_one_skill_installs_whole_and_lists() {
        let ws = scratch();
        let zip = archive(&[
            ("SKILL.md", "---\nname: 12306-skill\ndescription: 查询车票\n---\nRun scripts/q.py", 0o644),
            ("scripts/q.py", "print(1)", 0o755),
            ("README.md", "readme", 0o644),
        ]);
        let done = unpack(&zip, &src(""), &ws).unwrap();
        assert_eq!(done.folder, "12306-skill");
        assert_eq!(done.files, vec!["README.md", "SKILL.md", "scripts/q.py"]);
        assert!(!done.replaced);
        assert!(done.instructions.contains("Run scripts/q.py"));
        let dir = host_dir(&ws).join("12306-skill");
        assert_eq!(std::fs::read_to_string(dir.join("scripts/q.py")).unwrap(), "print(1)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("scripts/q.py")).unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(std::fs::metadata(dir.join("README.md")).unwrap().permissions().mode() & 0o777, 0o644);
        }
        let listed = list(&ws, &BTreeSet::new());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].description, "查询车票");
        assert_eq!(listed[0].path, "/solos/ws/skills/12306-skill");
        assert!(listed[0].enabled);
        assert!(!list(&ws, &BTreeSet::from(["12306-skill".to_string()]))[0].enabled);
        // Nothing is left behind from the staging folder.
        assert_eq!(std::fs::read_dir(host_dir(&ws)).unwrap().count(), 1);
    }

    #[test]
    fn installing_again_replaces_the_folder() {
        let ws = scratch();
        unpack(&archive(&[("SKILL.md", "---\nname: s\n---\nv1", 0o644), ("old.txt", "x", 0o644)]), &src(""), &ws).unwrap();
        let again = unpack(&archive(&[("SKILL.md", "---\nname: s\n---\nv2", 0o644)]), &src(""), &ws).unwrap();
        assert!(again.replaced);
        assert_eq!(again.files, vec!["SKILL.md"]);
        assert!(!host_dir(&ws).join("s/old.txt").exists());
        assert_eq!(instructions(&ws, "s").unwrap(), "---\nname: s\n---\nv2");
    }

    #[test]
    fn several_skills_need_a_folder_link() {
        let ws = scratch();
        let zip = archive(&[
            ("skills/pdf/SKILL.md", "---\nname: pdf\n---", 0o644),
            ("skills/pdf/x.py", "", 0o644),
            ("skills/docx/SKILL.md", "---\nname: docx\n---", 0o644),
            (".github/SKILL.md", "not a skill", 0o644),
        ]);
        match unpack(&zip, &src(""), &ws) {
            Err(CoreError::NoSingleSkill { candidates, .. }) => assert_eq!(
                candidates,
                vec!["https://github.com/o/repo/tree/HEAD/skills/docx", "https://github.com/o/repo/tree/HEAD/skills/pdf"]
            ),
            other => panic!("{other:?}"),
        }
        let pdf = unpack(&zip, &src("skills/pdf"), &ws).unwrap();
        assert_eq!((pdf.folder.as_str(), pdf.files.clone()), ("pdf", vec!["SKILL.md".to_string(), "x.py".to_string()]));
        // A link to the folder itself chooses it.
        assert_eq!(unpack(&zip, &src("skills/docx"), &ws).unwrap().folder, "docx");
        match unpack(&archive(&[("README.md", "", 0o644)]), &src(""), &ws) {
            Err(CoreError::NoSingleSkill { candidates, .. }) => assert!(candidates.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_folder_falls_back_to_where_the_skill_was() {
        let ws = scratch();
        let done = unpack(&archive(&[("tools/票务/SKILL.md", "---\nname: 火车票\n---", 0o644)]), &src(""), &ws);
        // Neither the skill's name nor its folder's is usable as a folder name.
        assert_eq!(done.unwrap().folder, "skill");
        let done = unpack(&archive(&[("SKILL.md", "no front matter", 0o644)]), &src(""), &ws).unwrap();
        assert_eq!(done.folder, "repo");
        assert_eq!(list(&ws, &BTreeSet::new()).iter().find(|s| s.folder == "repo").unwrap().name, "repo");
    }

    #[test]
    fn named_folders_stay_inside_the_skills_folder() {
        let ws = scratch();
        std::fs::create_dir_all(ws.join("keep")).unwrap();
        std::fs::write(ws.join("keep/SKILL.md"), "x").unwrap();
        for bad in ["", "..", "../keep", ".installing-x", "a/b"] {
            assert!(matches!(remove(&ws, bad), Err(CoreError::NoSuchSkill { .. })), "{bad}");
            assert!(instructions(&ws, bad).is_err(), "{bad}");
        }
        assert!(ws.join("keep/SKILL.md").exists());
    }

    #[test]
    fn the_prompt_always_says_how_to_install() {
        let none = prompt_section(&[]);
        assert!(none.contains("`skill_install`") && none.contains("No skills are installed."));
        let skill = |folder: &str, enabled: bool| Skill {
            folder: folder.into(),
            name: folder.into(),
            description: "d".repeat(400),
            path: format!("/solos/ws/skills/{folder}"),
            enabled,
        };
        let some = prompt_section(&[skill("a", true), skill("b", false)]);
        assert!(some.contains("\n- a: ddd"));
        assert!(some.contains("… (/solos/ws/skills/a/SKILL.md)"));
        assert!(!some.contains("- b"));
        assert!(!some.contains("No skills"));
    }
}
