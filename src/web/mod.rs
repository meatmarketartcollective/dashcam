use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Read-only local viewer: the "why did it do that" surface, terminal's shy sibling.
/// Binds 127.0.0.1 only.
pub struct WebConfig {
    pub port: u16,
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = "
body { background:#16181d; color:#c9cdd6; font-family: ui-monospace, Menlo, Consolas, monospace;
       max-width: 860px; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; }
a { color:#d4af6a; text-decoration: none; }
a:hover { text-decoration: underline; }
h1 { color:#e8eaf0; font-size:1.3rem; font-weight:600; }
h2 { color:#e8eaf0; font-size:1.05rem; font-weight:600; margin-top:1.6rem; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align:left; padding:.35rem .6rem; border-bottom:1px solid #262a33; }
th { color:#8a90a0; font-weight:500; font-size:.85rem; }
code, pre { color:#9fb3c8; white-space: pre-wrap; word-break: break-word; }
.hop { border-left: 2px solid #2a2f3a; padding: .45rem .8rem; margin: .5rem 0; }
.kind { color:#d4af6a; font-size:.8rem; }
.muted { color:#8a90a0; font-size:.85rem; }
";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{STYLE}</style></head><body>{}</body></html>",
        escape(title),
        body
    )
}

pub fn render_index(store: &Store) -> String {
    let mut rows = String::new();
    match store.sessions() {
        Ok(sessions) if !sessions.is_empty() => {
            for (id, started, note, count) in sessions {
                rows.push_str(&format!(
                    "<tr><td><a href=\"/session/{id}\">{id}</a></td><td>{}</td><td>{}</td><td>{count}</td></tr>",
                    escape(&started),
                    escape(note.as_deref().unwrap_or("")),
                ));
            }
            let body = format!(
                "<h1>mayday</h1><p class=\"muted\">local flight recorder · sessions on this machine</p>\
                 <table><tr><th>session</th><th>started</th><th>note</th><th>hops</th></tr>{rows}</table>"
            );
            page("mayday · sessions", &body)
        }
        Ok(_) => page("mayday", "<h1>mayday</h1><p class=\"muted\">no sessions recorded yet</p>"),
        Err(e) => page("mayday", &format!("<h1>mayday</h1><p>error reading timeline: {e}</p>")),
    }
}

pub fn render_session(store: &Store, session: i64) -> String {
    match store.events(session) {
        Ok(events) => {
            let mut body = format!(
                "<p class=\"muted\"><a href=\"/\">&larr; all sessions</a></p><h1>session {session}</h1>"
            );
            if events.is_empty() {
                body.push_str("<p class=\"muted\">no hops recorded</p>");
            }
            for e in events {
                body.push_str(&format!(
                    "<div class=\"hop\"><span class=\"kind\">{:?}</span> <span class=\"muted\">{} · seq {}</span><br>{}<pre>{}</pre></div>",
                    e.kind,
                    escape(&e.ts),
                    e.seq,
                    escape(&e.summary),
                    escape(&e.payload),
                ));
            }
            page(&format!("mayday · session {session}"), &body)
        }
        Err(e) => page("mayday", &format!("<h1>mayday</h1><p>error: {e}</p>")),
    }
}

pub fn serve(store: Arc<Store>, cfg: WebConfig, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", cfg.port))
        .map_err(std::io::Error::other)?;
    while running.load(Ordering::Relaxed) {
        let request = match server.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => break,
        };
        let url = request.url().split('?').next().unwrap_or("/").to_string();
        let (body, status) = if url == "/" {
            (render_index(&store), 200)
        } else if let Some(rest) = url.strip_prefix("/session/") {
            match rest.parse::<i64>() {
                Ok(id) => (render_session(&store, id), 200),
                Err(_) => (page("mayday", "<h1>mayday</h1><p>bad session id</p>"), 400),
            }
        } else {
            (page("mayday", "<h1>mayday</h1><p>not found</p>"), 404)
        };
        let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .unwrap();
        let _ = request.respond(tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(header));
    }
    Ok(())
}
