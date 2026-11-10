use crate::event::Event;
use crate::store::Store;

/// Watch engine: poll a session for hops newer than `last_seq` and render
/// them one to a line. The CLI loop is thin I/O (sleep, print, repeat);
/// polling and rendering live here so they are testable.
pub fn poll_once(store: &Store, session: i64, last_seq: i64) -> Result<Vec<Event>, String> {
    store.events_since(session, last_seq).map_err(|e| e.to_string())
}

/// One line per hop, matching replay's shape.
pub fn render_line(e: &Event) -> String {
    format!("[{}] {:?} {}", e.ts, e.kind, e.summary)
}
