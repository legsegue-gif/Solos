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
            let mut skill = describe(folder, &text, disabled);
            skill.source = source_of(&e.path());
            skill.file_count = count_files(&e.path());
            Some(skill)
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
        source: None,
        file_count: 0,
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
        format!("{}/{}/{}/zip/{}", GitHubHosts::default().archive, self.owner, self.repo, self.reference)
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

/// Where a skill installed from a GitHub link keeps that link, in its own
/// folder, so it can be updated from it.
pub const SOURCE_FILE: &str = ".solos-source";

/// Where GitHub answers: its API (the file list), its raw files, and its
/// archives. Replaceable so tests can stand in for it.
#[derive(Debug, Clone)]
pub struct GitHubHosts {
    pub api: String,
    pub raw: String,
    pub archive: String,
}

impl Default for GitHubHosts {
    fn default() -> Self {
        Self {
            api: "https://api.github.com".into(),
            raw: "https://raw.githubusercontent.com".into(),
            archive: "https://codeload.github.com".into(),
        }
    }
}

/// Download a skill from GitHub and put it in the skills folder, replacing
/// any earlier copy of the same folder.
pub async fn install(source: &str, workspace: &Path) -> Result<Installed, CoreError> {
    install_from(source, workspace, &GitHubHosts::default()).await
}

/// The skill's own files only, from GitHub's list of the repository's files
/// (one request) and its raw files: measured on a folder of anthropics/skills,
/// about 2 s against 13 s for the repository's 4 MB archive. When the list
/// cannot be had (GitHub limits it to 60 requests an hour without a key, and
/// cuts it short for huge repositories), the archive is used as before.
pub async fn install_from(source: &str, workspace: &Path, hosts: &GitHubHosts) -> Result<Installed, CoreError> {
    let src = parse_source(source).ok_or_else(|| CoreError::NotASkillSource { source_text: source.to_string() })?;
    let link = source.trim().to_string();
    let workspace = workspace.to_path_buf();
    let timed_out = || CoreError::Network { detail: format!("no complete download in {} s", DOWNLOAD_TIMEOUT.as_secs()) };
    match tokio::time::timeout(DOWNLOAD_TIMEOUT, from_file_list(&src, hosts)).await.map_err(|_| timed_out())? {
        Ok((dir, files)) => {
            let repo = src.repo.clone();
            tokio::task::spawn_blocking(move || place(files, &dir, &|p| src.folder_link(p), &repo, Some(&link), &workspace))
                .await
                .map_err(|e| CoreError::Internal { detail: e.to_string() })?
        }
        Err(ListFailed::Final(e)) => Err(e),
        Err(ListFailed::Retry(why)) => {
            tracing::info!("installing {link} from its archive: {why}");
            let url = format!("{}/{}/{}/zip/{}", hosts.archive, src.owner, src.repo, src.reference);
            let archive = tokio::time::timeout(DOWNLOAD_TIMEOUT, download(&url)).await.map_err(|_| timed_out())??;
            tokio::task::spawn_blocking(move || unpack(&archive, &src, &link, &workspace))
                .await
                .map_err(|e| CoreError::Internal { detail: e.to_string() })?
        }
    }
}

enum ListFailed {
    /// The answer stands: no such repository, no single skill, too big.
    Final(CoreError),
    /// The list could not be had; the archive may still work.
    Retry(String),
}

/// The skill's folder and its files, by their paths in the repository.
async fn from_file_list(src: &GitHubSource, hosts: &GitHubHosts) -> Result<(PathBuf, Vec<Entry>), ListFailed> {
    use futures::StreamExt;
    let client = crate::providers::http::client();
    let url = format!("{}/repos/{}/{}/git/trees/{}?recursive=1", hosts.api, src.owner, src.repo, src.reference);
    let resp = client
        .get(&url)
        // GitHub's API refuses requests without one.
        .header("User-Agent", "Solos")
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ListFailed::Retry(e.to_string()))?;
    match resp.status().as_u16() {
        200 => {}
        404 | 422 => {
            return Err(ListFailed::Final(CoreError::Http { status: 404, detail: "repository, branch or folder not found (or not public)".into() }))
        }
        status => return Err(ListFailed::Retry(format!("the file list answered {status}"))),
    }
    let list: serde_json::Value = resp.json().await.map_err(|e| ListFailed::Retry(e.to_string()))?;
    if list.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(ListFailed::Retry("the file list was cut short".into()));
    }
    // Files only: not folders, links or submodules.
    let blobs: Vec<(PathBuf, bool, u64)> = list
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("type").and_then(serde_json::Value::as_str) == Some("blob"))
        .filter_map(|e| {
            let mode = e.get("mode").and_then(serde_json::Value::as_str)?;
            (mode != "120000").then_some(())?;
            let path = PathBuf::from(e.get("path")?.as_str()?);
            path.components().all(|c| matches!(c, Component::Normal(_))).then_some(())?;
            Some((path, mode == "100755", e.get("size").and_then(serde_json::Value::as_u64).unwrap_or(0)))
        })
        .collect();
    let dir = choose_dir(blobs.iter().map(|(p, _, _)| p.as_path()), Path::new(&src.path), &|p| src.folder_link(p)).map_err(ListFailed::Final)?;
    let chosen: Vec<&(PathBuf, bool, u64)> = blobs.iter().filter(|(p, _, _)| p.starts_with(&dir)).collect();
    if let Some(e) = too_big(chosen.len(), chosen.iter().map(|(_, _, size)| size).sum()) {
        return Err(ListFailed::Final(e));
    }
    let jobs: Vec<(PathBuf, bool, reqwest::Url)> = chosen
        .into_iter()
        .map(|(path, executable, _)| {
            let mut url = reqwest::Url::parse(&hosts.raw).expect("a host address");
            url.path_segments_mut()
                .expect("a host address")
                .extend([src.owner.as_str(), src.repo.as_str(), src.reference.as_str()])
                .extend(path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()));
            (path.clone(), *executable, url)
        })
        .collect();
    let fetches = jobs.into_iter().map(|(path, executable, url)| {
        let client = client.clone();
        async move {
            let resp = client.get(url).timeout(std::time::Duration::from_secs(60)).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("{} answered {}", path.display(), resp.status().as_u16()));
            }
            let data = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok::<_, String>(Entry { path, data, executable })
        }
    });
    let mut files = Vec::new();
    let mut downloads = futures::stream::iter(fetches).buffer_unordered(8);
    while let Some(file) = downloads.next().await {
        files.push(file.map_err(ListFailed::Retry)?);
    }
    Ok((dir, files))
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

/// A GitHub archive: its entries all sit under one top folder.
pub fn unpack(archive: &[u8], src: &GitHubSource, link: &str, workspace: &Path) -> Result<Installed, CoreError> {
    let files = read_zip(archive, true)?;
    place(files, Path::new(&src.path), &|p| src.folder_link(p), &src.repo, Some(link), workspace)
}

/// A skill from a file: a `.zip` (a `.skill` is one) holding its folder, a
/// `SKILL.md` alone, or a folder (what the model wrote or downloaded).
pub fn install_file(path: &Path, workspace: &Path) -> Result<Installed, CoreError> {
    let io = |e: std::io::Error| CoreError::Storage { detail: format!("{}: {e}", path.display()) };
    let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if path.is_dir() {
        if path.starts_with(host_dir(workspace)) {
            return Err(CoreError::NotASkillSource { source_text: "a folder already in the skills folder".into() });
        }
        return place(read_folder(path)?, Path::new(""), &|p| p.to_string(), &stem, None, workspace);
    }
    let size = std::fs::metadata(path).map_err(io)?.len();
    if size > MAX_DOWNLOAD {
        return Err(CoreError::Protocol { detail: format!("the file is larger than {} MB", MAX_DOWNLOAD >> 20) });
    }
    let bytes = std::fs::read(path).map_err(io)?;
    if bytes.starts_with(b"PK\x03\x04") {
        return place(read_zip(&bytes, false)?, Path::new(""), &|p| p.to_string(), &stem, None, workspace);
    }
    let fallback = if stem.eq_ignore_ascii_case("skill") { "pasted-skill" } else { stem.as_str() };
    place(text_entries(&String::from_utf8_lossy(&bytes))?, Path::new(""), &|p| p.to_string(), fallback, None, workspace)
}

/// A skill from its `SKILL.md`, pasted.
pub fn install_text(text: &str, workspace: &Path) -> Result<Installed, CoreError> {
    place(text_entries(text)?, Path::new(""), &|p| p.to_string(), "pasted-skill", None, workspace)
}

fn text_entries(text: &str) -> Result<Vec<Entry>, CoreError> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(CoreError::NoSingleSkill { source_text: "the text".into(), candidates: vec![] });
    }
    Ok(vec![Entry { path: PathBuf::from(FILE), data: text.as_bytes().to_vec(), executable: false }])
}

/// One file of a skill being installed, by its path within the source.
struct Entry {
    path: PathBuf,
    data: Vec<u8>,
    executable: bool,
}

fn too_big(files: usize, bytes: u64) -> Option<CoreError> {
    if files > MAX_FILES {
        return Some(CoreError::Protocol { detail: format!("it has more than {MAX_FILES} files") });
    }
    (bytes > MAX_UNPACKED).then(|| CoreError::Protocol { detail: format!("it unpacks to more than {} MB", MAX_UNPACKED >> 20) })
}

/// A zip's files, without directories or links, each path inside it; with
/// `strip_top`, the archive's own top folder (`<repo>-<ref>/`) dropped.
fn read_zip(archive: &[u8], strip_top: bool) -> Result<Vec<Entry>, CoreError> {
    let bad = |e: zip::result::ZipError| CoreError::Protocol { detail: format!("the archive could not be read: {e}") };
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(bad)?;
    let (mut files, mut total) = (Vec::new(), 0u64);
    for index in 0..zip.len() {
        let mut f = zip.by_index(index).map_err(bad)?;
        if f.is_dir() || f.is_symlink() {
            continue;
        }
        let Some(name) = f.enclosed_name() else { continue };
        let path: PathBuf = name.components().skip(usize::from(strip_top)).collect();
        // macOS puts resource forks beside the files it zips.
        if path.as_os_str().is_empty() || path.starts_with("__MACOSX") || !path.components().all(|c| matches!(c, Component::Normal(_))) {
            continue;
        }
        total += f.size();
        if let Some(e) = too_big(files.len() + 1, total) {
            return Err(e);
        }
        let executable = f.unix_mode().is_some_and(|m| m & 0o111 != 0);
        let mut data = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut data).map_err(|e| CoreError::Protocol { detail: format!("the archive could not be read: {e}") })?;
        files.push(Entry { path, data, executable });
    }
    Ok(files)
}

fn read_folder(dir: &Path) -> Result<Vec<Entry>, CoreError> {
    let io = |e: std::io::Error| CoreError::Storage { detail: e.to_string() };
    let (mut files, mut total, mut pending) = (Vec::new(), 0u64, vec![dir.to_path_buf()]);
    while let Some(d) = pending.pop() {
        for e in std::fs::read_dir(&d).map_err(io)?.flatten() {
            let kind = e.file_type().map_err(io)?;
            if kind.is_dir() {
                pending.push(e.path());
            } else if kind.is_file() {
                let data = std::fs::read(e.path()).map_err(io)?;
                total += data.len() as u64;
                if let Some(err) = too_big(files.len() + 1, total) {
                    return Err(err);
                }
                #[cfg(unix)]
                let executable = std::os::unix::fs::PermissionsExt::mode(&e.metadata().map_err(io)?.permissions()) & 0o111 != 0;
                #[cfg(not(unix))]
                let executable = false;
                let path = e.path().strip_prefix(dir).expect("walked from dir").to_path_buf();
                files.push(Entry { path, data, executable });
            }
        }
    }
    Ok(files)
}

fn is_skill_md(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(FILE))
}

/// The skill's folder among `paths`: the one at `wanted`, or the only one
/// below it with a `SKILL.md`; otherwise the candidates, named by `link`.
fn choose_dir<'a>(paths: impl Iterator<Item = &'a Path>, wanted: &Path, link: &dyn Fn(&str) -> String) -> Result<PathBuf, CoreError> {
    let hidden = |p: &Path| p.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
    let mut candidates: Vec<PathBuf> = paths
        .filter(|p| p.starts_with(wanted) && is_skill_md(p))
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|dir| !hidden(dir))
        .collect();
    candidates.sort();
    if candidates.iter().any(|c| c == wanted) {
        Ok(wanted.to_path_buf())
    } else if candidates.len() == 1 {
        Ok(candidates.remove(0))
    } else {
        Err(CoreError::NoSingleSkill {
            source_text: link(&wanted.to_string_lossy()),
            candidates: candidates.iter().map(|c| link(&c.to_string_lossy())).collect(),
        })
    }
}

/// Picks the skill's folder among `files` (the one at `wanted`, or the only
/// one with a `SKILL.md`), and writes it to the skills folder, replacing an
/// earlier copy. `link` names a candidate folder for the user; `fallback`
/// names the skill when its `SKILL.md` gives no usable name; `source` is the
/// GitHub link it can be updated from.
fn place(
    files: Vec<Entry>,
    wanted: &Path,
    link: &dyn Fn(&str) -> String,
    fallback: &str,
    source: Option<&str>,
    workspace: &Path,
) -> Result<Installed, CoreError> {
    let dir = choose_dir(files.iter().map(|e| e.path.as_path()), wanted, link)?;

    let chosen: Vec<&Entry> = files.iter().filter(|e| e.path.starts_with(&dir) && e.path != Path::new(SOURCE_FILE)).collect();
    let instructions = chosen
        .iter()
        .find(|e| e.path.parent() == Some(dir.as_path()) && is_skill_md(&e.path))
        .map(|e| String::from_utf8_lossy(&e.data).into_owned())
        .ok_or_else(|| CoreError::Internal { detail: "the chosen folder lost its SKILL.md".into() })?;
    let own_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| fallback.to_string());
    let root = host_dir(workspace);
    let folder = match front_matter(&instructions).get("name").and_then(|n| folder_name(n)) {
        Some(named) => named,
        // Without a name of its own, a skill does not replace another one
        // that also had none.
        None => {
            let base = folder_name(&own_name).or_else(|| folder_name(fallback)).unwrap_or_else(|| "skill".into());
            (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|f| !root.join(f).exists()).expect("some name is free")
        }
    };

    let io = |e: std::io::Error| CoreError::Storage { detail: e.to_string() };
    std::fs::create_dir_all(&root).map_err(io)?;
    let staging = root.join(format!(".installing-{}", uuid::Uuid::new_v4()));
    let written = (|| -> Result<Vec<String>, CoreError> {
        let mut names = Vec::new();
        for e in &chosen {
            let rel = e.path.strip_prefix(&dir).expect("filtered by prefix");
            // `SKILL.md` keeps its usual spelling whatever the source had.
            let rel = if rel.as_os_str().eq_ignore_ascii_case(FILE) { Path::new(FILE) } else { rel };
            let target = staging.join(rel);
            std::fs::create_dir_all(target.parent().expect("inside staging")).map_err(io)?;
            std::fs::write(&target, &e.data).map_err(io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(if e.executable { 0o755 } else { 0o644 })).map_err(io)?;
            }
            names.push(rel.to_string_lossy().into_owned());
        }
        if let Some(source) = source {
            std::fs::write(staging.join(SOURCE_FILE), source).map_err(io)?;
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

/// A skill's files, relative to its folder, sorted; the folder's record of
/// where it came from is not one of them.
pub fn files(workspace: &Path, folder: &str) -> Result<Vec<String>, CoreError> {
    let dir = host_dir(workspace).join(checked_folder(folder)?);
    if !dir.join(FILE).is_file() {
        return Err(CoreError::NoSuchSkill { folder: folder.to_string() });
    }
    let mut out: Vec<String> = read_folder(&dir)?
        .into_iter()
        .map(|e| e.path.to_string_lossy().into_owned())
        .filter(|p| p != SOURCE_FILE)
        .collect();
    out.sort();
    Ok(out)
}

/// The GitHub link a skill was installed from, if it was.
fn source_of(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(SOURCE_FILE)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn count_files(dir: &Path) -> u32 {
    let mut n = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => pending.push(e.path()),
                Ok(t) if t.is_file() && e.file_name() != SOURCE_FILE => n += 1,
                _ => {}
            }
        }
    }
    n
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
- To install a skill from a GitHub link, or from a .zip/.skill file or folder in the workspace, use `skill_install`; it puts the skill in {dir}/ and returns its SKILL.md. Do not download skills from GitHub by hand. A skill you write yourself goes in its own folder there, with `name` and `description` at the top of its SKILL.md.\n"
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
    use std::sync::Arc;

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

    const LINK: &str = "https://github.com/o/repo";

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
        let done = unpack(&zip, &src(""), LINK, &ws).unwrap();
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
        unpack(&archive(&[("SKILL.md", "---\nname: s\n---\nv1", 0o644), ("old.txt", "x", 0o644)]), &src(""), LINK, &ws).unwrap();
        let again = unpack(&archive(&[("SKILL.md", "---\nname: s\n---\nv2", 0o644)]), &src(""), LINK, &ws).unwrap();
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
        match unpack(&zip, &src(""), LINK, &ws) {
            Err(CoreError::NoSingleSkill { candidates, .. }) => assert_eq!(
                candidates,
                vec!["https://github.com/o/repo/tree/HEAD/skills/docx", "https://github.com/o/repo/tree/HEAD/skills/pdf"]
            ),
            other => panic!("{other:?}"),
        }
        let pdf = unpack(&zip, &src("skills/pdf"), LINK, &ws).unwrap();
        assert_eq!((pdf.folder.as_str(), pdf.files.clone()), ("pdf", vec!["SKILL.md".to_string(), "x.py".to_string()]));
        // A link to the folder itself chooses it.
        assert_eq!(unpack(&zip, &src("skills/docx"), LINK, &ws).unwrap().folder, "docx");
        match unpack(&archive(&[("README.md", "", 0o644)]), &src(""), LINK, &ws) {
            Err(CoreError::NoSingleSkill { candidates, .. }) => assert!(candidates.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_folder_falls_back_to_where_the_skill_was() {
        let ws = scratch();
        let done = unpack(&archive(&[("tools/票务/SKILL.md", "---\nname: 火车票\n---", 0o644)]), &src(""), LINK, &ws);
        // Neither the skill's name nor its folder's is usable as a folder
        // name; the repository's is.
        assert_eq!(done.unwrap().folder, "repo");
        // With no name, it does not replace the one above.
        let done = unpack(&archive(&[("SKILL.md", "no front matter", 0o644)]), &src(""), LINK, &ws).unwrap();
        assert_eq!(done.folder, "repo-2");
        assert_eq!(list(&ws, &BTreeSet::new()).iter().find(|s| s.folder == "repo-2").unwrap().name, "repo-2");
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
    fn a_github_skill_remembers_its_link_and_lists_its_files() {
        let ws = scratch();
        unpack(&archive(&[("SKILL.md", "---\nname: s\n---", 0o644), ("scripts/a.py", "", 0o644)]), &src(""), LINK, &ws).unwrap();
        let s = &list(&ws, &BTreeSet::new())[0];
        assert_eq!((s.source.as_deref(), s.file_count), (Some(LINK), 2));
        assert_eq!(files(&ws, "s").unwrap(), vec!["SKILL.md", "scripts/a.py"]);
        assert!(files(&ws, "nope").is_err());
    }

    /// A zip the user made: no top folder of GitHub's, maybe macOS's extras.
    fn user_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            for (path, body) in files {
                z.start_file(*path, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn files_and_pasted_text_install_too() {
        let ws = scratch();
        let picked = scratch();
        // A zip with the folder at its top, and the resource forks macOS adds.
        std::fs::write(picked.join("pdf.skill"), user_zip(&[("pdf/SKILL.md", "---\nname: pdf\n---"), ("pdf/x.py", "1"), ("__MACOSX/pdf/._x.py", "junk")])).unwrap();
        let done = install_file(&picked.join("pdf.skill"), &ws).unwrap();
        assert_eq!((done.folder.as_str(), done.files.clone()), ("pdf", vec!["SKILL.md".to_string(), "x.py".to_string()]));
        assert_eq!(list(&ws, &BTreeSet::new())[0].source, None, "nothing to update from");
        // A zip with the files at its top: named after the file.
        std::fs::write(picked.join("Trains.zip"), user_zip(&[("SKILL.md", "no name here"), ("q.py", "")])).unwrap();
        assert_eq!(install_file(&picked.join("Trains.zip"), &ws).unwrap().folder, "trains");
        // A SKILL.md alone.
        std::fs::write(picked.join("SKILL.md"), "---\nname: Weather Now\n---\nAsk").unwrap();
        assert_eq!(install_file(&picked.join("SKILL.md"), &ws).unwrap().folder, "weather-now");
        // A folder the model made.
        std::fs::create_dir_all(picked.join("made/bin")).unwrap();
        std::fs::write(picked.join("made/SKILL.md"), "x").unwrap();
        std::fs::write(picked.join("made/bin/run"), "x").unwrap();
        assert_eq!(install_file(&picked.join("made"), &ws).unwrap().files, vec!["SKILL.md", "bin/run"]);
        // Pasted text: without a name, a second paste does not replace the first.
        assert_eq!(install_text("Just instructions", &ws).unwrap().folder, "pasted-skill");
        assert_eq!(install_text("Other instructions", &ws).unwrap().folder, "pasted-skill-2");
        assert_eq!(install_text("---\nname: notes\n---\nv1", &ws).unwrap().folder, "notes");
        assert!(install_text("---\nname: notes\n---\nv2", &ws).unwrap().replaced, "a named one is replaced by name");
        assert!(matches!(install_text("  ", &ws), Err(CoreError::NoSingleSkill { .. })));
        assert!(install_file(&host_dir(&ws).join("pdf"), &ws).is_err(), "not from the skills folder itself");
    }

    /// A stand-in for GitHub: its file list (or a refusal with `list_status`),
    /// raw files, and the archive; it counts the raw files asked for.
    async fn fake_github(list_status: u16) -> (GitHubHosts, Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::{extract::Path as P, http::StatusCode, response::IntoResponse, routing::get, Router};
        let raw_asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let asked = raw_asked.clone();
        let tree = serde_json::json!({"truncated": false, "tree": [
            {"path": "skills", "type": "tree", "mode": "040000"},
            {"path": "skills/pdf/SKILL.md", "type": "blob", "mode": "100644", "size": 20},
            {"path": "skills/pdf/run me.sh", "type": "blob", "mode": "100755", "size": 9},
            {"path": "skills/pdf/link", "type": "blob", "mode": "120000", "size": 4},
            {"path": "skills/docx/SKILL.md", "type": "blob", "mode": "100644", "size": 20}
        ]});
        let zip = archive(&[("skills/pdf/SKILL.md", "---\nname: pdf\n---\nfrom zip", 0o644)]);
        let app = Router::new()
            .route("/repos/o/repo/git/trees/HEAD", get(move || {
                let tree = tree.clone();
                async move { if list_status == 200 { axum::Json(tree).into_response() } else { StatusCode::from_u16(list_status).unwrap().into_response() } }
            }))
            .route("/o/repo/HEAD/{*path}", get(move |P(path): P<String>| {
                let asked = asked.clone();
                async move {
                    asked.lock().unwrap().push(path.clone());
                    match path.as_str() {
                        "skills/pdf/SKILL.md" => "---\nname: pdf\n---\nraw".into_response(),
                        "skills/pdf/run me.sh" => "echo hi\n".into_response(),
                        _ => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }))
            .route("/o/repo/zip/HEAD", get(move || { let zip = zip.clone(); async move { zip } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (GitHubHosts { api: base.clone(), raw: base.clone(), archive: base }, raw_asked)
    }

    #[tokio::test]
    async fn a_folder_comes_from_the_file_list_with_only_its_own_files() {
        let ws = scratch();
        let (hosts, asked) = fake_github(200).await;
        let done = install_from("https://github.com/o/repo/tree/HEAD/skills/pdf", &ws, &hosts).await.unwrap();
        assert_eq!((done.folder.as_str(), done.instructions.as_str()), ("pdf", "---\nname: pdf\n---\nraw"));
        assert_eq!(done.files, vec!["SKILL.md", "run me.sh"]);
        let mut fetched = asked.lock().unwrap().clone();
        fetched.sort();
        assert_eq!(fetched, vec!["skills/pdf/SKILL.md", "skills/pdf/run me.sh"], "not the other skill, not the link");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(host_dir(&ws).join("pdf/run me.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(list(&ws, &BTreeSet::new())[0].source.as_deref(), Some("https://github.com/o/repo/tree/HEAD/skills/pdf"));
        // The whole repository: its two skills are named, nothing is fetched.
        asked.lock().unwrap().clear();
        match install_from("https://github.com/o/repo", &ws, &hosts).await {
            Err(CoreError::NoSingleSkill { candidates, .. }) => assert_eq!(candidates.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_file_list_falls_back_to_the_archive_and_a_missing_repository_does_not() {
        let ws = scratch();
        let (hosts, asked) = fake_github(403).await;
        let done = install_from("https://github.com/o/repo/tree/HEAD/skills/pdf", &ws, &hosts).await.unwrap();
        assert_eq!(done.instructions, "---\nname: pdf\n---\nfrom zip");
        assert!(asked.lock().unwrap().is_empty());
        let (hosts, _) = fake_github(404).await;
        assert!(matches!(install_from("https://github.com/o/repo", &ws, &hosts).await, Err(CoreError::Http { status: 404, .. })));
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
            source: None,
            file_count: 1,
        };
        let some = prompt_section(&[skill("a", true), skill("b", false)]);
        assert!(some.contains("\n- a: ddd"));
        assert!(some.contains("… (/solos/ws/skills/a/SKILL.md)"));
        assert!(!some.contains("- b"));
        assert!(!some.contains("No skills"));
    }
}

/// Against GitHub itself, so not run by default:
/// `cargo test -p solos-core --lib real_github -- --ignored --nocapture`.
#[cfg(test)]
mod real {
    #[tokio::test]
    #[ignore]
    async fn real_github() {
        let ws = std::env::temp_dir().join(format!("solos-real-{}", uuid::Uuid::new_v4()));
        for link in ["https://github.com/anthropics/skills/tree/main/skills/pdf", "https://github.com/Joooook/12306-skill"] {
            let t = std::time::Instant::now();
            let done = super::install(link, &ws).await.unwrap();
            println!("{link}: {} files in {:.1} s", done.files.len(), t.elapsed().as_secs_f64());
        }
    }
}
