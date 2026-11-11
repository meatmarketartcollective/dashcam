use crate::crypto;
use crate::redact;
use crate::event::{Event, EventKind};
use rusqlite::Connection;
use std::os::unix::fs::OpenOptionsExt;
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
        // Two dashcam processes can share one timeline (two proxies, a CLI
        // prune while a proxy records): WAL keeps readers from blocking the
        // writer, and a busy timeout makes competing writers wait out each
        // other's commits instead of failing with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0));
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
        // One lock hold across the insert and the rowid read: last_insert_rowid
        // is connection-wide, so another thread's insert between the two
        // statements would hand back the wrong id.
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (note) VALUES (?1)",
            [self.enc(note.unwrap_or_default()).as_str()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// How long (ms) this store waits out another writer's commit instead of
    /// failing with SQLITE_BUSY.
    pub fn busy_timeout_ms(&self) -> u64 {
        let conn = self.conn.lock().expect("timeline lock");
        conn.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    pub fn append(
        &self,
        session: i64,
        kind: EventKind,
        summary: &str,
        payload: &str,
    ) -> rusqlite::Result<()> {
        self.append_get_seq(session, kind, summary, payload).map(|_| ())
    }

    /// Like `append`, but returns the seq of the hop just written, so derived
    /// hops can point back at their source.
    pub fn append_get_seq(
        &self,
        session: i64,
        kind: EventKind,
        summary: &str,
        payload: &str,
    ) -> rusqlite::Result<i64> {
        // The seq is assigned inside the INSERT statement itself: SQLite
        // evaluates the subquery while holding the write lock, so the next-seq
        // read and the write are one atomic step. That holds across threads
        // (the mutex serializes this connection) AND across separate dashcam
        // processes sharing the database file, which a read-then-insert pair
        // would not.
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (session_id, seq, kind, summary, payload)
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1), ?2, ?3, ?4)",
            rusqlite::params![
                session,
                kind_to_str(kind),
                self.enc(summary),
                self.enc(payload)
            ],
        )?;
        // Read back exactly the row we inserted by its rowid, which is exact
        // no matter what other writers appended in the meantime.
        let seq: i64 = conn.query_row(
            "SELECT seq FROM events WHERE id = ?1",
            [conn.last_insert_rowid()],
            |r| r.get(0),
        )?;
        Ok(seq)
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

    /// Hops with a seq greater than `min_seq`, oldest first: the live-tail
    /// read behind `dashcam watch`.
    pub fn events_since(&self, session: i64, min_seq: i64) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, seq, ts, kind, summary, payload
             FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![session, min_seq], |r| {
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

    /// Search every session's decrypted summaries and payloads for `query`
    /// (case-insensitive substring). Returns (session_id, event) pairs, at
    /// most 100, in session then seq order.
    pub fn search_timeline(&self, query: &str) -> rusqlite::Result<Vec<(i64, Event)>> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        if q.is_empty() {
            return Ok(out);
        }
        for (id, _started, _note, _count) in self.sessions()? {
            if out.len() >= 100 {
                break;
            }
            for e in self.events(id)? {
                if e.summary.to_lowercase().contains(&q) || e.payload.to_lowercase().contains(&q) {
                    out.push((id, e));
                    if out.len() >= 100 {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }
    /// (seq, tool name) for every tool call in a session, oldest first.
    /// The name comes from the payload's "tool" field; a row whose payload
    /// does not parse falls back to the summary's word after "tool_call".
    pub fn tool_call_names(&self, session: i64) -> rusqlite::Result<Vec<(i64, String)>> {
        Ok(self
            .events(session)?
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolCall)
            .map(|e| {
                let name = serde_json::from_str::<serde_json::Value>(&e.payload)
                    .ok()
                    .and_then(|v| v.get("tool").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| {
                        e.summary
                            .strip_prefix("tool_call")
                            .unwrap_or(&e.summary)
                            .trim()
                            .to_string()
                    });
                (e.seq, name)
            })
            .collect())
    }
}

impl Store {
    /// Walk the whole timeline and check every encrypted field decrypts with
    /// this store's key. The regular read paths fall back to raw ciphertext
    /// when decryption fails, which would hide a wrong key or damaged rows;
    /// this is the honest check that catches them.
    pub fn verify(&self) -> rusqlite::Result<VerifyReport> {
        let mut report = VerifyReport {
            encrypted: self.key.is_some(),
            ..Default::default()
        };
        if !report.encrypted {
            return Ok(report);
        }
        let k = self.key.as_ref().unwrap();
        for row in self.raw_rows()? {
            report.checked += 1;
            if crypto::decrypt(k, &row.summary).is_none() {
                report.failed.push(VerifyMiss {
                    session_id: row.session_id,
                    seq: row.seq,
                    field: "summary",
                });
            }
            if crypto::decrypt(k, &row.payload).is_none() {
                report.failed.push(VerifyMiss {
                    session_id: row.session_id,
                    seq: row.seq,
                    field: "payload",
                });
            }
        }
        Ok(report)
    }

    /// Scan the timeline for redaction leaks: stored text that would still
    /// change under the redactor's rules, meaning secret-shaped text got in
    /// unscrubbed. With a key, undecryptable rows are counted and skipped
    /// (`verify` is the tool that reports them).
    pub fn audit(&self, redactor: &redact::Redactor) -> rusqlite::Result<AuditReport> {
        let mut report = AuditReport::default();
        for row in self.raw_rows()? {
            report.scanned += 1;
            for (raw, field) in [(&row.summary, "summary"), (&row.payload, "payload")] {
                let text = match &self.key {
                    Some(k) => match crypto::decrypt(k, raw) {
                        Some(t) => t,
                        None => {
                            report.undecryptable += 1;
                            continue;
                        }
                    },
                    None => raw.clone(),
                };
                let scrubbed = redactor.scrub(&text);
                if scrubbed != text {
                    report.leaks.push(LeakHit {
                        session_id: row.session_id,
                        seq: row.seq,
                        field,
                        preview: scrubbed.chars().take(120).collect(),
                    });
                }
            }
        }
        Ok(report)
    }

    fn raw_rows(&self) -> rusqlite::Result<Vec<RawRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, summary, payload FROM events ORDER BY session_id, seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RawRow {
                session_id: r.get(0)?,
                seq: r.get(1)?,
                summary: r.get(2)?,
                payload: r.get(3)?,
            })
        })?;
        rows.collect()
    }
}

/// One hop of the timeline that will not decrypt with the current key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyMiss {
    pub session_id: i64,
    pub seq: i64,
    /// Which encrypted field failed: "summary" or "payload".
    pub field: &'static str,
}

/// What `dashcam verify` found walking the timeline.
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    /// False for a plaintext store (no key): there is nothing to check.
    pub encrypted: bool,
    pub checked: usize,
    pub failed: Vec<VerifyMiss>,
}

/// One field where stored text should have been scrubbed but was not.
#[derive(Debug, Clone)]
pub struct LeakHit {
    pub session_id: i64,
    pub seq: i64,
    /// "summary" or "payload".
    pub field: &'static str,
    /// The field after scrubbing (first 120 chars), so the report shows the
    /// hole without printing the secret.
    pub preview: String,
}

/// What `dashcam audit` found scanning the timeline for redaction leaks.
#[derive(Debug, Clone, Default)]
pub struct AuditReport {
    pub scanned: usize,
    pub undecryptable: usize,
    pub leaks: Vec<LeakHit>,
}

/// A decrypted-lookup row straight from storage, before the decrypt-or-pass
/// fallback. Only integrity tooling reads these.
struct RawRow {
    session_id: i64,
    seq: i64,
    summary: String,
    payload: String,
}

/// How much history the timeline keeps. Either limit may be None (no limit).
pub struct RetentionPolicy {
    /// Keep at most this many most-recent events across all sessions.
    pub max_events: Option<i64>,
    /// Drop events from sessions older than this many days.
    pub max_age_days: Option<i64>,
    /// Keep the timeline under this many stored bytes (oldest pruned first).
    pub max_size_bytes: Option<i64>,
    /// Before retention ages hops out, append them to this JSONL file so
    /// pruning never destroys the only copy.
    pub export_path: Option<std::path::PathBuf>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_events: None,
            max_age_days: None,
            max_size_bytes: None,
            export_path: None,
        }
    }
}

/// (events removed, sessions removed)
pub type PruneResult = (i64, i64);

impl Store {
    /// Apply a retention policy: oldest events go first; sessions left with
    /// no events are removed too. Events about to be aged out are exported
    /// first when the policy names an export file, so pruning never destroys
    /// the only copy of a hop.
    pub fn prune(&self, policy: &RetentionPolicy) -> Result<PruneResult, String> {
        use std::io::Write;
        let conn = self.conn();
        let mut doomed: Vec<i64> = Vec::new();

        if let Some(days) = policy.max_age_days {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM events WHERE session_id IN (
                         SELECT id FROM sessions
                         WHERE started_at < datetime('now', ?1))",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([format!("-{days} days")], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            for id in rows {
                doomed.push(id.map_err(|e| e.to_string())?);
            }
        }
        if let Some(max) = policy.max_events {
            let total: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if total > max {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM events WHERE id NOT IN (
                             SELECT id FROM events ORDER BY id DESC LIMIT ?1)",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([max], |r| r.get::<_, i64>(0))
                    .map_err(|e| e.to_string())?;
                for id in rows {
                    doomed.push(id.map_err(|e| e.to_string())?);
                }
            }
        }
        if let Some(cap) = policy.max_size_bytes {
            let total: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(summary)+LENGTH(payload)),0) FROM events",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if total > cap {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, LENGTH(summary)+LENGTH(payload)
                         FROM events ORDER BY id ASC",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    })
                    .map_err(|e| e.to_string())?;
                let mut running = 0i64;
                for row in rows {
                    let (id, size) = row.map_err(|e| e.to_string())?;
                    if running >= total - cap {
                        break;
                    }
                    doomed.push(id);
                    running += size;
                }
            }
        }

        doomed.sort_unstable();
        doomed.dedup();

        // export before aging out: each doomed hop lands as one readable
        // JSONL line, appended so repeated prunes accumulate
        if !doomed.is_empty() {
            if let Some(path) = &policy.export_path {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|e| format!("export {}: {e}", path.display()))?;
                let mut out = std::io::BufWriter::new(file);
                let mut stmt = conn
                    .prepare(
                        "SELECT id, session_id, seq, ts, kind, summary, payload
                         FROM events WHERE id = ?1",
                    )
                    .map_err(|e| e.to_string())?;
                for id in &doomed {
                    let row = stmt
                        .query_row([id], |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, i64>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, String>(4)?,
                                r.get::<_, String>(5)?,
                                r.get::<_, String>(6)?,
                            ))
                        })
                        .map_err(|e| e.to_string())?;
                    let (id, session_id, seq, ts, kind, summary, payload) = row;
                    let e = Event {
                        id,
                        session_id,
                        seq,
                        ts,
                        kind: str_to_kind(&kind),
                        summary: self.dec(&summary),
                        payload: self.dec(&payload),
                    };
                    writeln!(out, "{}", event_jsonl(&e))
                        .map_err(|e| format!("export write: {e}"))?;
                }
                out.flush().map_err(|e| format!("export write: {e}"))?;
            }
        }

        let mut events_removed = 0i64;
        for chunk in doomed.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            events_removed += conn
                .execute(
                    &format!("DELETE FROM events WHERE id IN ({placeholders})"),
                    rusqlite::params_from_iter(chunk.iter()),
                )
                .map_err(|e| e.to_string())? as i64;
        }
        // sessions left with no events are history that can no longer be replayed
        let sessions_removed = conn
            .execute(
                "DELETE FROM sessions WHERE id NOT IN (SELECT DISTINCT session_id FROM events)",
                [],
            )
            .map_err(|e| e.to_string())? as i64;
        Ok((events_removed, sessions_removed))
    }

    /// Write one session's hops as JSON lines to `out`. Returns the number of
    /// hops written. Errors when the session does not exist.
    pub fn export_session(
        &self,
        session: i64,
        out: &mut impl std::io::Write,
    ) -> Result<usize, String> {
        let exists: i64 = {
            // scope the lock: no other reader may run while it is held
            let conn = self.conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                [session],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?
        };
        if exists == 0 {
            return Err(format!("no such session: {session}"));
        }
        let events = self.events(session).map_err(|e| e.to_string())?;
        for e in &events {
            writeln!(out, "{}", event_jsonl(e)).map_err(|e| format!("export write: {e}"))?;
        }
        Ok(events.len())
    }
}

/// One hop as a JSON line for export files: decrypted, payload embedded as
/// parsed JSON when it parses.
pub fn event_jsonl(e: &Event) -> String {
    let payload = serde_json::from_str::<serde_json::Value>(&e.payload)
        .unwrap_or(serde_json::Value::String(e.payload.clone()));
    serde_json::json!({
        "session": e.session_id,
        "seq": e.seq,
        "ts": e.ts,
        "kind": kind_to_str(e.kind),
        "summary": e.summary,
        "payload": payload,
    })
    .to_string()
}

fn kind_to_str(k: EventKind) -> &'static str {
    match k {
            EventKind::NetworkHop => "network_hop",
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

impl Store {
    /// One hop by session and seq, decrypted. None when there is no such hop.
    pub fn get_hop(&self, session: i64, seq: i64) -> rusqlite::Result<Option<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, seq, ts, kind, summary, payload
             FROM events WHERE session_id = ?1 AND seq = ?2",
        )?;
        let mut rows = stmt.query_map([session, seq], |r| {
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
        match rows.next() {
            Some(row) => {
                let (id, session_id, seq, ts, kind, summary, payload) = row?;
                Ok(Some(Event {
                    id,
                    session_id,
                    seq,
                    ts,
                    kind: str_to_kind(&kind),
                    summary: self.dec(&summary),
                    payload: self.dec(&payload),
                }))
            }
            None => Ok(None),
        }
    }

    /// The parsed payload of a file_diff hop: file, added/removed lines,
    /// content, and edge flags (new_file, deleted, binary, truncation).
    /// `Ok(None)` when the hop is missing or is not a file_diff hop.
    pub fn get_diff(&self, session: i64, seq: i64) -> rusqlite::Result<Option<serde_json::Value>> {
        let Some(e) = self.get_hop(session, seq)? else {
            return Ok(None);
        };
        if e.kind != EventKind::FileDiff {
            return Ok(None);
        }
        Ok(serde_json::from_str(&e.payload).ok())
    }

    /// A compact summary of one session: hop counts by kind, the tools the
    /// agent called, the files it touched, and the run's time span.
    /// `Ok(None)` when the session does not exist.
    pub fn summarize_session(&self, session: i64) -> rusqlite::Result<Option<String>> {
        // scope the lock: conn() guards a non-reentrant mutex, so no other
        // Store reader (events(), sessions()) may run while it is held
        let count: i64 = {
            let conn = self.conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                [session],
                |r| r.get(0),
            )?
        };
        if count == 0 {
            return Ok(None);
        }
        let events = self.events(session)?;
        if events.is_empty() {
            return Ok(Some(format!("session {session}: no hops recorded")));
        }
        let mut kind_counts: std::collections::BTreeMap<String, usize> = Default::default();
        let mut tools: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for e in &events {
            let kind = serde_json::to_value(&e.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".into());
            *kind_counts.entry(kind).or_insert(0) += 1;
            let Ok(p) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
                continue;
            };
            match e.kind {
                EventKind::ToolCall => {
                    let name = p.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
                    if !tools.iter().any(|t| t == name) {
                        tools.push(name.to_string());
                    }
                }
                EventKind::FileDiff => {
                    if let Some(f) = p.get("file").and_then(|f| f.as_str()) {
                        let detail = if p.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false) {
                            "(deleted)".to_string()
                        } else if p.get("binary").and_then(|b| b.as_bool()).unwrap_or(false) {
                            format!(
                                "(binary, {} bytes)",
                                p.get("bytes").and_then(|b| b.as_i64()).unwrap_or(0)
                            )
                        } else {
                            let added = p.get("added").and_then(|v| v.as_array()).map_or(0, |a| a.len());
                            let removed = p.get("removed").and_then(|v| v.as_array()).map_or(0, |a| a.len());
                            if p.get("new_file").and_then(|n| n.as_bool()).unwrap_or(false) {
                                format!("(new file, +{added})")
                            } else {
                                format!("(+{added} -{removed})")
                            }
                        };
                        let entry = format!("{f} {detail}");
                        if !files.contains(&entry) {
                            files.push(entry);
                        }
                    }
                }
                _ => {}
            }
        }
        let note = self
            .sessions()?
            .into_iter()
            .find(|(id, _, _, _)| *id == session)
            .and_then(|(_, _, n, _)| n);
        let mut out = format!("session {session}");
        if let Some(note) = note {
            out.push_str(&format!(" \"{note}\""));
        }
        out.push_str(&format!(
            ": {} hops ({} to {})",
            events.len(),
            events[0].ts,
            events[events.len() - 1].ts
        ));
        let kinds: Vec<String> = kind_counts
            .into_iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect();
        out.push_str(&format!("\nkinds: {}", kinds.join(", ")));
        if !tools.is_empty() {
            out.push_str(&format!("\ntools: {}", tools.join(", ")));
        }
        if !files.is_empty() {
            out.push_str("\nfiles:");
            for f in &files {
                out.push_str(&format!("\n  {f}"));
            }
        }
        Ok(Some(out))
    }
}
