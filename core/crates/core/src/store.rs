//! Durable state: sessions, messages, turns, settings.
//!
//! One thread owns the SQLite connection. Everything else sends it a job and
//! awaits the answer, so async code never blocks an executor thread on disk
//! I/O, and there is never more than one writer.
//!
//! Every operation returns a `Result`. Callers decide what a failure means;
//! nothing here swallows one.

use rusqlite::{params, Connection, OptionalExtension};
use solos_api::{Message, Millis, ModelChoice, Part, SessionInfo};
use std::path::Path;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the store thread has stopped")]
    Closed,
}

pub type Result<T> = std::result::Result<T, StoreError>;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    jobs: mpsc::UnboundedSender<Job>,
}

/// Schema versions, applied in order. Each entry moves the database from
/// version `i` to `i + 1`. Never edit a shipped entry; add a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    r#"
    CREATE TABLE sessions (
        id          TEXT PRIMARY KEY,
        title       TEXT,
        model_json  TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE TABLE messages (
        seq         INTEGER PRIMARY KEY AUTOINCREMENT,
        id          TEXT NOT NULL UNIQUE,
        session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role        TEXT NOT NULL,
        parts_json  TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
    CREATE INDEX messages_by_session ON messages(session_id, seq);
    CREATE TABLE turns (
        id          TEXT PRIMARY KEY,
        session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        state       TEXT NOT NULL,
        error_json  TEXT,
        started_at  INTEGER NOT NULL,
        ended_at    INTEGER
    );
    CREATE TABLE queued_inputs (
        seq         INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        text        TEXT NOT NULL
    );
    CREATE TABLE settings (
        key         TEXT PRIMARY KEY,
        value_json  TEXT NOT NULL
    );
    "#,
    // 2: per-session thinking switch
    r#"
    ALTER TABLE sessions ADD COLUMN thinking INTEGER;
    "#,
    // 3: queued input carries attachments: parts replace plain text
    r#"
    ALTER TABLE queued_inputs ADD COLUMN parts_json TEXT;
    UPDATE queued_inputs SET parts_json = json_array(json_object('kind', 'text', 'text', text));
    "#,
    // 4: summaries that stand in for the start of a conversation
    r#"
    CREATE TABLE compactions (
        id                  TEXT PRIMARY KEY,
        session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        through_message_id  TEXT NOT NULL,
        summary             TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        undone              INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX compactions_by_session ON compactions(session_id, created_at);
    "#,
    // 5: sessions pinned to the top of the list
    r#"
    ALTER TABLE sessions ADD COLUMN pinned_at INTEGER;
    "#,
];

pub fn now_millis() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as Millis)
        .unwrap_or(0)
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::start(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("solos-store".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .map_err(|_| StoreError::Closed)?;
        Ok(Self { jobs: tx })
    }

    /// Run `f` on the store thread and wait for its answer.
    async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.jobs
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    // -- sessions ------------------------------------------------------------

    pub async fn insert_session(&self, info: SessionInfo) -> Result<()> {
        self.run(move |c| {
            let model = info.model.as_ref().map(serde_json::to_string).transpose()?;
            c.execute(
                "INSERT INTO sessions (id, title, model_json, thinking, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![info.id, info.title, model, info.thinking, info.created_at, info.updated_at],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn session(&self, id: String) -> Result<Option<SessionInfo>> {
        self.run(move |c| {
            c.query_row(SESSION_SELECT_BY_ID, params![id], row_to_session)
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        self.run(|c| {
            let mut stmt = c.prepare(&format!("{SESSION_SELECT_LIST} ORDER BY s.updated_at DESC"))?;
            let rows = stmt.query_map([], row_to_session)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn set_session_model(&self, id: String, model: ModelChoice) -> Result<()> {
        self.run(move |c| {
            c.execute(
                "UPDATE sessions SET model_json = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(&model)?],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_thinking(&self, id: String, thinking: Option<bool>) -> Result<()> {
        self.run(move |c| {
            c.execute("UPDATE sessions SET thinking = ?2 WHERE id = ?1", params![id, thinking])?;
            Ok(())
        })
        .await
    }

    pub async fn set_title(&self, id: String, title: Option<String>) -> Result<()> {
        self.run(move |c| {
            c.execute("UPDATE sessions SET title = ?2 WHERE id = ?1", params![id, title])?;
            Ok(())
        })
        .await
    }

    pub async fn set_pinned(&self, id: String, pinned_at: Option<Millis>) -> Result<()> {
        self.run(move |c| {
            c.execute("UPDATE sessions SET pinned_at = ?2 WHERE id = ?1", params![id, pinned_at])?;
            Ok(())
        })
        .await
    }

    /// A new session holding a copy of another's messages and summaries, in
    /// one transaction. Messages get new ids (they are unique across the
    /// store); summaries follow the messages they stood in for.
    pub async fn duplicate_session(&self, from: String, info: SessionInfo) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction()?;
            let model = info.model.as_ref().map(serde_json::to_string).transpose()?;
            tx.execute(
                "INSERT INTO sessions (id, title, model_json, thinking, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![info.id, info.title, model, info.thinking, info.created_at, info.updated_at],
            )?;
            let mut ids = std::collections::HashMap::new();
            {
                let mut stmt = tx.prepare("SELECT id, role, parts_json, created_at FROM messages WHERE session_id = ?1 ORDER BY seq")?;
                let rows = stmt
                    .query_map(params![from], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (old, role, parts, created) in rows {
                    let new = uuid::Uuid::new_v4().simple().to_string();
                    tx.execute(
                        "INSERT INTO messages (id, session_id, role, parts_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![new, info.id, role, parts, created],
                    )?;
                    ids.insert(old, new);
                }
            }
            {
                let mut stmt = tx.prepare("SELECT through_message_id, summary, created_at, undone FROM compactions WHERE session_id = ?1 ORDER BY created_at, rowid")?;
                let rows = stmt
                    .query_map(params![from], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, bool>(3)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (through, summary, created, undone) in rows {
                    let Some(through) = ids.get(&through) else { continue };
                    tx.execute(
                        "INSERT INTO compactions (id, session_id, through_message_id, summary, created_at, undone) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![uuid::Uuid::new_v4().simple().to_string(), info.id, through, summary, created, undone],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn delete_session(&self, id: String) -> Result<()> {
        self.run(move |c| {
            c.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
            Ok(())
        })
        .await
    }

    // -- messages ------------------------------------------------------------

    /// Append a message and bump the session's `updated_at`, atomically.
    pub async fn append_message(&self, session_id: String, message: Message) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO messages (id, session_id, role, parts_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    message.id,
                    session_id,
                    serde_json::to_string(&message.role)?,
                    serde_json::to_string(&message.parts)?,
                    message.created_at
                ],
            )?;
            tx.execute(
                "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
                params![session_id, message.created_at],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn messages(&self, session_id: String) -> Result<Vec<Message>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, role, parts_json, created_at FROM messages WHERE session_id = ?1 ORDER BY seq",
            )?;
            let rows = stmt.query_map(params![session_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (id, role, parts, created_at) = row?;
                out.push(Message {
                    id,
                    role: serde_json::from_str(&role)?,
                    parts: serde_json::from_str(&parts)?,
                    created_at,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Delete every message after `after` (all when `None`), and the queue
    /// with them when everything goes.
    pub async fn truncate(&self, session_id: String, after: Option<String>) -> Result<()> {
        self.run(move |c| {
            let tx = c.transaction()?;
            match &after {
                Some(id) => {
                    tx.execute(
                        "DELETE FROM messages WHERE session_id = ?1
                         AND seq > (SELECT seq FROM messages WHERE id = ?2 AND session_id = ?1)",
                        params![session_id, id],
                    )?;
                }
                None => {
                    tx.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])?;
                    tx.execute("DELETE FROM queued_inputs WHERE session_id = ?1", params![session_id])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Replace a message's parts in place.
    pub async fn replace_parts(&self, id: String, parts: Vec<solos_api::Part>) -> Result<()> {
        self.run(move |c| {
            c.execute("UPDATE messages SET parts_json = ?2 WHERE id = ?1", params![id, serde_json::to_string(&parts)?])?;
            Ok(())
        })
        .await
    }

    /// Sessions whose title or messages contain `query`, newest first.
    pub async fn search_sessions(&self, query: String) -> Result<Vec<SessionInfo>> {
        self.run(move |c| {
            let pattern = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            let sql = format!(
                "{SESSION_SELECT_LIST} WHERE s.title LIKE ?1 ESCAPE '\\'
                 OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id
                            AND m.role IN ('\"user\"', '\"assistant\"') AND m.parts_json LIKE ?1 ESCAPE '\\')
                 ORDER BY s.updated_at DESC"
            );
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![pattern], row_to_session)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    // -- compactions ---------------------------------------------------------

    pub async fn add_compaction(&self, session_id: String, c: solos_api::Compaction) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO compactions (id, session_id, through_message_id, summary, created_at, undone) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![c.id, session_id, c.through_message_id, c.summary, c.created_at, c.undone],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_compaction_undone(&self, id: String, undone: bool) -> Result<()> {
        self.run(move |c| {
            c.execute("UPDATE compactions SET undone = ?2 WHERE id = ?1", params![id, undone])?;
            Ok(())
        })
        .await
    }

    /// Oldest first; summaries of deleted messages (after a retry or a clear)
    /// are dropped with them.
    pub async fn compactions(&self, session_id: String) -> Result<Vec<solos_api::Compaction>> {
        self.run(move |c| {
            c.execute(
                "DELETE FROM compactions WHERE session_id = ?1 AND through_message_id NOT IN (SELECT id FROM messages WHERE session_id = ?1)",
                params![session_id],
            )?;
            let mut stmt = c.prepare(
                "SELECT id, through_message_id, summary, created_at, undone FROM compactions WHERE session_id = ?1 ORDER BY created_at, rowid",
            )?;
            let rows = stmt.query_map(params![session_id], |r| {
                Ok(solos_api::Compaction {
                    id: r.get(0)?,
                    through_message_id: r.get(1)?,
                    summary: r.get(2)?,
                    created_at: r.get(3)?,
                    undone: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    // -- turns ---------------------------------------------------------------

    pub async fn start_turn(&self, turn_id: String, session_id: String) -> Result<()> {
        self.run(move |c| {
            c.execute(
                "INSERT INTO turns (id, session_id, state, started_at) VALUES (?1, ?2, 'running', ?3)",
                params![turn_id, session_id, now_millis()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn end_turn(&self, turn_id: String, state: &'static str, error_json: Option<String>) -> Result<()> {
        self.run(move |c| {
            c.execute(
                "UPDATE turns SET state = ?2, error_json = ?3, ended_at = ?4 WHERE id = ?1",
                params![turn_id, state, error_json, now_millis()],
            )?;
            Ok(())
        })
        .await
    }

    /// The error the session's latest turn ended with, if it failed.
    pub async fn last_turn_error(&self, session_id: String) -> Result<Option<solos_api::CoreError>> {
        self.run(move |c| {
            let row: Option<(String, Option<String>)> = c
                .query_row(
                    "SELECT state, error_json FROM turns WHERE session_id = ?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                    params![session_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(match row {
                Some((state, Some(json))) if state == "failed" => serde_json::from_str(&json).ok(),
                _ => None,
            })
        })
        .await
    }

    /// Turns a killed process left marked as running, with their sessions.
    pub async fn unfinished_turns(&self) -> Result<Vec<(String, String)>> {
        self.run(|c| {
            let mut stmt = c.prepare("SELECT id, session_id FROM turns WHERE state = 'running'")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    // -- queued input --------------------------------------------------------

    pub async fn push_queued(&self, session_id: String, parts: Vec<Part>) -> Result<()> {
        self.run(move |c| {
            let text = Message { id: String::new(), role: solos_api::Role::User, parts: parts.clone(), created_at: 0 }.text();
            c.execute(
                "INSERT INTO queued_inputs (session_id, text, parts_json) VALUES (?1, ?2, ?3)",
                params![session_id, text, serde_json::to_string(&parts)?],
            )?;
            Ok(())
        })
        .await
    }

    /// Remove and return the oldest queued input.
    pub async fn pop_queued(&self, session_id: String) -> Result<Option<Vec<Part>>> {
        self.run(move |c| {
            let tx = c.transaction()?;
            let row: Option<(i64, String)> = tx
                .query_row(
                    "SELECT seq, parts_json FROM queued_inputs WHERE session_id = ?1 ORDER BY seq LIMIT 1",
                    params![session_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((seq, _)) = &row {
                tx.execute("DELETE FROM queued_inputs WHERE seq = ?1", params![seq])?;
            }
            tx.commit()?;
            Ok(row.map(|(_, parts)| serde_json::from_str(&parts)).transpose()?)
        })
        .await
    }

    /// Queued input as the user would recognise it: its text, and the names
    /// of any attached files.
    pub async fn queued(&self, session_id: String) -> Result<Vec<String>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT parts_json FROM queued_inputs WHERE session_id = ?1 ORDER BY seq")?;
            let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let parts: Vec<Part> = serde_json::from_str(&row?)?;
                let mut line = Message { id: String::new(), role: solos_api::Role::User, parts: parts.clone(), created_at: 0 }.text();
                for p in &parts {
                    if let Part::Attachment { name, .. } = p {
                        line.push_str(if line.is_empty() { "" } else { " " });
                        line.push_str(&format!("[{name}]"));
                    }
                }
                out.push(line);
            }
            Ok(out)
        })
        .await
    }

    // -- settings ------------------------------------------------------------

    pub async fn put_setting(&self, key: &'static str, value: serde_json::Value) -> Result<()> {
        self.run(move |c| {
            c.execute(
                "INSERT INTO settings (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                params![key, serde_json::to_string(&value)?],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn setting(&self, key: &'static str) -> Result<Option<serde_json::Value>> {
        self.run(move |c| {
            let raw: Option<String> = c
                .query_row("SELECT value_json FROM settings WHERE key = ?1", params![key], |r| r.get(0))
                .optional()?;
            Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
        })
        .await
    }
}

/// The session columns, plus the parts of its latest assistant message for
/// the list preview.
const SESSION_SELECT_BY_ID: &str = "SELECT s.id, s.title, s.model_json, s.created_at, s.updated_at, s.thinking,
    (SELECT parts_json FROM messages m WHERE m.session_id = s.id AND m.role = '\"assistant\"'
     ORDER BY m.seq DESC LIMIT 1), s.pinned_at
    FROM sessions s WHERE s.id = ?1";
const SESSION_SELECT_LIST: &str = "SELECT s.id, s.title, s.model_json, s.created_at, s.updated_at, s.thinking,
    (SELECT parts_json FROM messages m WHERE m.session_id = s.id AND m.role = '\"assistant\"'
     ORDER BY m.seq DESC LIMIT 1), s.pinned_at
    FROM sessions s";
fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionInfo> {
    let model_json: Option<String> = r.get(2)?;
    let last_parts: Option<String> = r.get(6)?;
    Ok(SessionInfo {
        id: r.get(0)?,
        title: r.get(1)?,
        model: model_json.and_then(|s| serde_json::from_str(&s).ok()),
        thinking: r.get(5)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        preview: last_parts.and_then(|p| preview_of(&p)),
        pinned_at: r.get(7)?,
    })
}

/// The first line of text in a stored message, trimmed for a list row.
fn preview_of(parts_json: &str) -> Option<String> {
    let parts: Vec<solos_api::Part> = serde_json::from_str(parts_json).ok()?;
    let text = parts.iter().find_map(|p| match p {
        solos_api::Part::Text { text } if !text.trim().is_empty() => Some(text.trim()),
        _ => None,
    })?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```"))?;
    Some(strip_markdown(line).chars().take(120).collect())
}

/// One line of Markdown as the words a reader sees: no heading, quote or
/// list marker, no emphasis or code marks, links as their text.
fn strip_markdown(line: &str) -> String {
    let mut s = line.trim_start_matches(['#', '>', ' ']).to_string();
    for marker in ["- [ ] ", "- [x] ", "- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest.to_string();
            break;
        }
    }
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' | '_' | '`' | '~' => {}
            '!' if chars.get(i + 1) == Some(&'[') => {}
            '[' => {
                // [text](target) → text
                if let Some(close) = chars[i..].iter().position(|&c| c == ']').map(|p| p + i) {
                    if chars.get(close + 1) == Some(&'(') {
                        if let Some(end) = chars[close..].iter().position(|&c| c == ')').map(|p| p + close) {
                            out.extend(&chars[i + 1..close]);
                            i = end + 1;
                            continue;
                        }
                    }
                }
                out.push('[');
            }
            c => out.push(c),
        }
        i += 1;
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use solos_api::{Part, Role};

    fn session(id: &str) -> SessionInfo {
        SessionInfo { id: id.into(), title: None, model: None, thinking: None, created_at: 1, updated_at: 1, preview: None, pinned_at: None }
    }

    fn msg(id: &str, role: Role, text: &str, at: i64) -> Message {
        Message { id: id.into(), role, parts: vec![Part::Text { text: text.into() }], created_at: at }
    }

    #[tokio::test]
    async fn messages_round_trip_in_order_and_touch_the_session() {
        let store = Store::open_in_memory().unwrap();
        store.insert_session(session("s")).await.unwrap();
        store.append_message("s".into(), msg("a", Role::User, "hi", 10)).await.unwrap();
        store.append_message("s".into(), msg("b", Role::Assistant, "hello\nthere", 20)).await.unwrap();
        let got = store.messages("s".into()).await.unwrap();
        assert_eq!(got.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        let info = store.session("s".into()).await.unwrap().unwrap();
        assert_eq!(info.updated_at, 20);
        assert_eq!(info.preview.as_deref(), Some("hello"));
    }

    #[test]
    fn previews_read_as_text_not_markdown() {
        let p = |t: &str| preview_of(&serde_json::to_string(&vec![Part::Text { text: t.into() }]).unwrap());
        assert_eq!(p("17 × 23 = **391**").as_deref(), Some("17 × 23 = 391"));
        assert_eq!(p("\n## Results\nmore").as_deref(), Some("Results"));
        assert_eq!(p("- see [the chart](solos://ws/c.png) and `ls`").as_deref(), Some("see the chart and ls"));
        assert_eq!(p("```sh\nls\n```").as_deref(), Some("ls"), "the first line inside the fence");
    }

    #[tokio::test]
    async fn a_message_for_a_missing_session_is_an_error_not_a_silent_drop() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.append_message("nope".into(), msg("a", Role::User, "x", 1)).await.is_err());
    }

    #[tokio::test]
    async fn queued_input_comes_back_oldest_first_and_once() {
        let store = Store::open_in_memory().unwrap();
        store.insert_session(session("s")).await.unwrap();
        let text = |t: &str| vec![Part::Text { text: t.into() }];
        store.push_queued("s".into(), text("one")).await.unwrap();
        store.push_queued("s".into(), text("two")).await.unwrap();
        assert_eq!(store.pop_queued("s".into()).await.unwrap(), Some(text("one")));
        assert_eq!(store.queued("s".into()).await.unwrap(), ["two"]);
        assert_eq!(store.pop_queued("s".into()).await.unwrap(), Some(text("two")));
        assert_eq!(store.pop_queued("s".into()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_version_1_database_gains_the_thinking_column_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("solos-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v1.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            c.execute("INSERT INTO sessions (id, title, created_at, updated_at) VALUES ('s', 't', 1, 1)", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let info = s.session("s".into()).await.unwrap().unwrap();
        assert_eq!((info.title.as_deref(), info.thinking), (Some("t"), None));
        s.set_thinking("s".into(), Some(false)).await.unwrap();
        assert_eq!(s.session("s".into()).await.unwrap().unwrap().thinking, Some(false));
    }

    #[tokio::test]
    async fn queued_text_from_before_attachments_still_runs() {
        let dir = std::env::temp_dir().join(format!("solos-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v2.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute_batch(MIGRATIONS[1]).unwrap();
            c.pragma_update(None, "user_version", 2).unwrap();
            c.execute("INSERT INTO sessions (id, created_at, updated_at) VALUES ('s', 1, 1)", []).unwrap();
            c.execute("INSERT INTO queued_inputs (session_id, text) VALUES ('s', 'later \"quoted\"')", []).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.pop_queued("s".into()).await.unwrap(), Some(vec![Part::Text { text: "later \"quoted\"".into() }]));
    }

    #[tokio::test]
    async fn migrations_are_idempotent_across_opens() {
        let dir = std::env::temp_dir().join(format!("solos-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let s = Store::open(&path).unwrap();
            s.insert_session(session("s")).await.unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(s.session("s".into()).await.unwrap().is_some());
    }
}
