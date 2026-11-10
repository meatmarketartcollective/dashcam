use crate::crypto;
use crate::event::{Event, EventKind};
use rusqlite::Connection;
use std::sync::Mutex;

/// The timeline. Every session is one agent run; every event is one hop.
///
/// With a key, summaries, session notes, and full payloads are encrypted at
/// rest (AES-256-GCM): a tool that opens the file directly sees structured
/// metadata only, never readable content. All real access goes through this
/// app, which holds the key and decrypts in memory.
pub struct Store {
    conn: Mutex<Connection>,
    key: Option<Vec<u8>>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    harness TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id),
    seq INTEGER NOT NULL,
    ts TEXT NOT NULL DEFAULT (datetime('now')),
    kind TEXT NOT NULL,
    summary TEXT NOT NULL DEFAULT '',
    payload TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, seq);
";

impl Store {
    pub fn open(path: &std::path::Path, key_hex: Option<&str>) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let conn = Connection::open(path)?;
        let key = match key_hex {
            None => None,
            Some(hex) => match crypto::hex_decode(hex) {
                Some(k) if k.len() == 32 => Some(k),
                _ => {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "timeline key must be 64 hex characters (32 bytes)".into(),
                    ))
                }
            },
        };
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn), key })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("timeline lock")
    }

    fn enc(&self, s: &str) -> String {
        match &self.key {
            Some(k) => crypto::encrypt(k, s),
            None => s.to_string(),
        }
    }

    fn dec(&self, s: &str) -> String {
        match &self.key {
            Some(k) => crypto::decrypt(k, s).unwrap_or_else(|| s.to_string()),
            None => s.to_string(),
        }
    }

    pub fn new_session(&self, note: Option<&str>) -> rusqlite::Result<i64> {
        self.conn().execute(
            "INSERT INTO sessions (note) VALUES (?1)",
            [self.enc(note.unwrap_or_default()).as_str()],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn append(
        &self,
        session: i64,
        kind: EventKind,
        summary: &str,
        payload: &str,
    ) -> rusqlite::Result<()> {
        let seq: i64 = self.conn().query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
            [session],
            |r| r.get::<_, i64>(0),
        )? + 1;
        self.conn().execute(
            "INSERT INTO events (session_id, seq, kind, summary, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                session,
                seq,
                kind_to_str(kind),
                self.enc(summary),
                self.enc(payload)
            ],
        )?;
        Ok(())
    }

    pub fn sessions(&self) -> rusqlite::Result<Vec<(i64, String, Option<String>, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.started_at, s.note,
                    (SELECT COUNT(*) FROM events e WHERE e.session_id = s.id) AS n
             FROM sessions s ORDER BY s.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, started, note, n) = row?;
            out.push((id, started, note.map(|n| self.dec(&n)), n));
        }
        Ok(out)
    }

    pub fn events(&self, session: i64) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, seq, ts, kind, summary, payload
             FROM events WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([session], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, session_id, seq, ts, kind, summary, payload) = row?;
            out.push(Event {
                id,
                session_id,
                seq,
                ts,
                kind: str_to_kind(&kind),
                summary: self.dec(&summary),
                payload: self.dec(&payload),
            });
        }
        Ok(out)
    }
}

/// How much history the timeline keeps. Either limit may be None (no limit).
pub struct RetentionPolicy {
    /// Keep at most this many most-recent events across all sessions.
    pub max_events: Option<i64>,
    /// Drop events from sessions older than this many days.
    pub max_age_days: Option<i64>,
}

/// (events removed, sessions removed)
pub type PruneResult = (i64, i64);

impl Store {
    /// Apply a retention policy: oldest events go first; sessions left with
    /// no events are removed too. Never touches live sessions' newest history
    /// beyond what the policy asks for.
    pub fn prune(&self, policy: &RetentionPolicy) -> rusqlite::Result<PruneResult> {
        let conn = self.conn();
        let mut events_removed = 0i64;
        let sessions_removed: i64;

        if let Some(days) = policy.max_age_days {
            events_removed += conn
                .execute(
                    "DELETE FROM events WHERE session_id IN (
                         SELECT id FROM sessions
                         WHERE started_at < datetime('now', ?1))",
                    [format!("-{days} days")],
                )? as i64;
        }
        if let Some(max) = policy.max_events {
            let total: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
            if total > max {
                events_removed += conn
                    .execute(
                        "DELETE FROM events WHERE id NOT IN (
                             SELECT id FROM events ORDER BY id DESC LIMIT ?1)",
                        [max],
                    )? as i64;
            }
        }
        // sessions left with no events are history that can no longer be replayed
        sessions_removed = conn.execute(
            "DELETE FROM sessions WHERE id NOT IN (SELECT DISTINCT session_id FROM events)",
            [],
        )? as i64;
        Ok((events_removed, sessions_removed))
    }
}

fn kind_to_str(k: EventKind) -> &'static str {
    match k {
        EventKind::ModelTurn => "model_turn",
        EventKind::ToolCall => "tool_call",
        EventKind::Prompt => "prompt",
        EventKind::FileDiff => "file_diff",
    }
}

fn str_to_kind(s: &str) -> EventKind {
    match s {
        "model_turn" => EventKind::ModelTurn,
        "tool_call" => EventKind::ToolCall,
        "prompt" => EventKind::Prompt,
        "file_diff" => EventKind::FileDiff,
        _ => EventKind::ModelTurn,
    }
}
