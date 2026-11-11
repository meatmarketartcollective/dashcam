use crate::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Read-only local viewer: the "why did it do that" surface. Step-through
/// replay with state at each hop, terminal-first replay's web sibling.
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
.nav { margin: 1rem 0; }
.nav a { margin-right: 1.2rem; }
.nav .off { color:#4a4f5c; }
.state { background:#101216; border:1px solid #262a33; border-radius:6px;
         padding:.7rem 1rem; margin:1rem 0; }
.state li { margin:.15rem 0; }
input { background:#101216; color:#c9cdd6; border:1px solid #262a33;
        border-radius:4px; padding:.35rem .6rem; font-family:inherit; }
button { background:#1e222b; color:#d4af6a; border:1px solid #2a2f3a;
         border-radius:4px; padding:.35rem .8rem; font-family:inherit; cursor:pointer; }
";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{STYLE}</style></head><body>{}</body></html>",
        escape(title),
        body
    )
}

/// Show a JSON payload pretty when it parses, otherwise as recorded.
fn pretty(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| payload.to_string()),
        Err(_) => payload.to_string(),
    }
}

fn search_form() -> String {
    "<form action=\"/search\" method=\"get\"><input name=\"q\" placeholder=\"search the timeline\" aria-label=\"search\"> <button type=\"submit\">search</button></form>".to_string()
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
                "<h1>dashcam</h1><p class=\"muted\">local flight recorder · sessions on this machine</p>{}\
                 <table><tr><th>session</th><th>started</th><th>note</th><th>hops</th></tr>{rows}</table>",
                search_form()
            );
            page("dashcam · sessions", &body)
        }
        Ok(_) => page(
            "dashcam",
            &format!("<h1>dashcam</h1><p class=\"muted\">no sessions recorded yet</p>{}", search_form()),
        ),
        Err(e) => page("dashcam", &format!("<h1>dashcam</h1><p>error reading timeline: {e}</p>")),
    }
}

pub fn render_session(store: &Store, session: i64) -> String {
    match store.events(session) {
        Ok(events) => {
            let mut body = format!(
                "<p class=\"muted\"><a href=\"/\">&larr; all sessions</a></p><h1>session {session}</h1>{}",
                search_form()
            );
            if events.is_empty() {
                body.push_str("<p class=\"muted\">no hops recorded</p>");
            }
            for e in &events {
                body.push_str(&format!(
                    "<div class=\"hop\"><a href=\"/session/{session}/hop/{seq}\"><span class=\"kind\">{kind:?}</span> seq {seq}</a> \
                     <span class=\"muted\">{ts}</span><br>{summary}</div>",
                    seq = e.seq,
                    kind = e.kind,
                    ts = escape(&e.ts),
                    summary = escape(&e.summary),
                ));
            }
            page(&format!("dashcam · session {session}"), &body)
        }
        Err(e) => page("dashcam", &format!("<h1>dashcam</h1><p>error: {e}</p>")),
    }
}

/// One step of the replay: the hop's full payload, prev/next navigation, and
/// the state the agent was in at this hop (every summary up to and including
/// this hop, in order).
pub fn render_hop(store: &Store, session: i64, seq: i64) -> String {
    let events = match store.events(session) {
        Ok(e) => e,
        Err(e) => return page("dashcam", &format!("<h1>dashcam</h1><p>error: {e}</p>")),
    };
    let Some(idx) = events.iter().position(|e| e.seq == seq) else {
        return page(
            "dashcam",
            &format!("<h1>dashcam</h1><p class=\"muted\"><a href=\"/session/{session}\">&larr; session {session}</a></p><p>hop {seq} not found</p>"),
        );
    };
    let hop = &events[idx];
    let first = events.first().map(|e| e.seq).unwrap_or(seq);
    let last = events.last().map(|e| e.seq).unwrap_or(seq);
    let prev = if idx > 0 { Some(events[idx - 1].seq) } else { None };
    let next = if idx + 1 < events.len() { Some(events[idx + 1].seq) } else { None };

    let mut nav = String::from("<p class=\"nav\">");
    nav.push_str(&format!("<a href=\"/session/{session}\">top</a> "));
    if idx > 0 {
        nav.push_str(&format!("<a href=\"/session/{session}/hop/{first}\">&laquo; first</a> "));
    } else {
        nav.push_str("<span class=\"off\">&laquo; first</span> ");
    }
    match prev {
        Some(p) => nav.push_str(&format!("<a href=\"/session/{session}/hop/{p}\">&larr; prev</a> ")),
        None => nav.push_str("<span class=\"off\">&larr; prev</span> "),
    }
    match next {
        Some(n) => nav.push_str(&format!("<a href=\"/session/{session}/hop/{n}\">next &rarr;</a> ")),
        None => nav.push_str("<span class=\"off\">next &rarr;</span> "),
    }
    if idx + 1 < events.len() {
        nav.push_str(&format!("<a href=\"/session/{session}/hop/{last}\">last &raquo;</a>"));
    } else {
        nav.push_str("<span class=\"off\">last &raquo;</span>");
    }
    nav.push_str("</p>");

    let mut state = String::new();
    for (i, e) in events.iter().take(idx + 1).enumerate() {
        state.push_str(&format!(
            "<li><span class=\"kind\">{:?}</span> {}</li>",
            e.kind,
            escape(&e.summary)
        ));
        let _ = i;
    }

    let body = format!(
        "<p class=\"muted\"><a href=\"/session/{session}\">&larr; session {session}</a></p>\
         <h1>hop {seq} <span class=\"muted\">of {}</span></h1>\
         <p class=\"muted\">{} · {ts}</p>{nav}\
         <h2>what happened</h2><p>{summary}</p>\
         <h2>full record</h2><pre>{payload}</pre>\
         <h2>state at this hop</h2><ol class=\"state\">{state}</ol>",
        events.len(),
        format!("{:?}", hop.kind),
        ts = escape(&hop.ts),
        summary = escape(&hop.summary),
        payload = escape(&pretty(&hop.payload)),
    );
    page(&format!("dashcam · session {session} · hop {seq}"), &body)
}

pub fn render_search(store: &Store, q: &str) -> String {
    let mut body = format!(
        "<p class=\"muted\"><a href=\"/\">&larr; all sessions</a></p><h1>search</h1>{}<p class=\"muted\">query: {}</p>",
        search_form(),
        escape(q)
    );
    match store.search_timeline(q) {
        Ok(results) if results.is_empty() => {
            body.push_str("<p class=\"muted\">no hops matched</p>");
        }
        Ok(results) => {
            for (session, e) in &results {
                body.push_str(&format!(
                    "<div class=\"hop\"><a href=\"/session/{s}/hop/{q}\">session {s} · seq {q}</a> \
                     <span class=\"kind\">{:?}</span> <span class=\"muted\">{ts}</span><br>{summary}</div>",
                    e.kind,
                    s = session,
                    q = e.seq,
                    ts = escape(&e.ts),
                    summary = escape(&e.summary),
                ));
            }
            body.push_str(&format!("<p class=\"muted\">{} hops matched</p>", results.len()));
        }
        Err(e) => body.push_str(&format!("<p>error: {e}</p>")),
    }
    page("dashcam · search", &body)
}

/// Minimal percent-decoding for the one query parameter the viewer takes.
fn decode(raw: &str) -> String {
    let s = raw.replace('+', " ");
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if k == name {
            return Some(decode(v));
        }
    }
    None
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
        let full = request.url().to_string();
        let (url, query) = match full.split_once('?') {
            Some((a, b)) => (a.to_string(), Some(b.to_string())),
            None => (full.clone(), None),
        };
        let (body, status) = if url == "/" {
            (render_index(&store), 200)
        } else if url == "/search" {
            let q = query
                .as_deref()
                .and_then(|qs| query_param(qs, "q"))
                .unwrap_or_default();
            (render_search(&store, &q), 200)
        } else if let Some(rest) = url.strip_prefix("/session/") {
            match rest.split_once("/hop/") {
                Some((sid, hseq)) => match (sid.parse::<i64>(), hseq.parse::<i64>()) {
                    (Ok(id), Ok(sq)) => (render_hop(&store, id, sq), 200),
                    _ => (page("dashcam", "<h1>dashcam</h1><p>bad session or hop id</p>"), 400),
                },
                None => match rest.parse::<i64>() {
                    Ok(id) => (render_session(&store, id), 200),
                    Err(_) => (page("dashcam", "<h1>dashcam</h1><p>bad session id</p>"), 400),
                },
            }
        } else {
            (page("dashcam", "<h1>dashcam</h1><p>not found</p>"), 404)
        };
        let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .unwrap();
        let _ = request.respond(tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(header));
    }
    Ok(())
}
