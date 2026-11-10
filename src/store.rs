use crate::event::{Event, EventKind};
use rusqlite::Connection;

pub struct Store {
    conn: Connection,
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
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn new_session(&self, note: Option<&str>) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO sessions (note) VALUES (?1)",
            [note.unwrap_or_default()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn append(&self, session: i64, kind: EventKind, summary: &str, payload: &str) -> rusqlite::Result<()> {
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
            [session],
            |r| r.get(0),
        )? + 1;
        self.conn.execute(
            "INSERT INTO events (session_id, seq, kind, summary, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![session, seq, kind_to_str(kind), summary, payload],
        )?;
        Ok(())
    }

    pub fn sessions(&self) -> rusqlite::Result<Vec<(i64, String, Option<String>, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.started_at, s.note,
                    (SELECT COUNT(*) FROM events e WHERE e.session_id = s.id) AS n
             FROM sessions s ORDER BY s.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.collect()
    }

    pub fn events(&self, session: i64) -> rusqlite::Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, seq, ts, kind, summary, payload
             FROM events WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([session], |r| {
            Ok(Event {
                id: r.get(0)?,
                session_id: r.get(1)?,
                seq: r.get(2)?,
                ts: r.get(3)?,
                kind: str_to_kind(&r.get::<_, String>(4)?),
                summary: r.get(5)?,
                payload: r.get(6)?,
            })
        })?;
        rows.collect()
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
