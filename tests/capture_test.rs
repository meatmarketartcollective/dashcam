use mayday::capture::{self, ProxyConfig};
use mayday::store::Store;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::net::TcpStream;
use std::time::Duration;

/// The proxy thread binds inside serve(); a fixed sleep races on a loaded box.
fn wait_port(port: u16) {
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("proxy port {port} never came up");
}

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// A minimal upstream provider stub: answers every POST with an echo JSON body.
fn spawn_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut chunk = [0u8; 8192];
            let n = stream.read(&mut chunk).unwrap_or(0);
            let text = String::from_utf8_lossy(&chunk[..n]).to_string();
            let req_body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let auth = text
                .lines()
                .find(|l| l.to_lowercase().starts_with("authorization:"))
                .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string())
                .unwrap_or_default();
            let resp_body = format!(
                "{{\"echo\":{},\"auth_received\":{}}}",
                serde_json::json!(req_body),
                serde_json::json!(auth)
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                resp_body.len(),
                resp_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

/// An SSE upstream (chunked streaming, what real providers send): the proxy
/// must pass it through and record the reassembled stream.
#[test]
fn streaming_responses_are_captured_intact() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut chunk = [0u8; 8192];
            let _ = stream.read(&mut chunk);
            let mut out = String::new();
            out.push_str("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
            for piece in ["data: {\"delta\":\"step one\"}\n\n", "data: {\"delta\":\"step two\"}\n\n"] {
                out.push_str(&format!("{:x}\r
{}\r\n", piece.len(), piece));
            }
            out.push_str("0\r\n\r\n");
            let _ = stream.write_all(out.as_bytes());
            let _ = stream.flush();
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store.new_session(Some("streaming harness")).unwrap();

    let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener2.local_addr().unwrap().port();
    drop(listener2);

    let running = Arc::new(AtomicBool::new(true));
    let cfg = ProxyConfig {
        port: proxy_port,
        upstream: format!("http://127.0.0.1:{port}"),
        session,
        retention: Some(mayday::store::RetentionPolicy { max_events: None, max_age_days: None }),
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = capture::serve(s2, cfg, r2);
    });
    wait_port(proxy_port);

    let resp = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string("{}")
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.into_string().unwrap();
    assert!(text.contains("step one"));
    assert!(text.contains("step two"));

    running.store(false, Ordering::Relaxed);
    let events = store.events(session).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].payload.contains("step one"));
    assert!(events[0].payload.contains("step two"));
}

#[test]
fn scrub_survives_multibyte_text() {
    // a secret next to multi-byte UTF-8 must be redacted, not panic the proxy
    let out = mayday::capture::scrub("reasoning says \u{2014} use sk-abcdef0123456789 here");
    assert!(!out.contains("sk-abcdef0123456789"));
    assert!(out.contains("[redacted]"));
    assert!(out.contains('\u{2014}'));
    assert!(mayday::capture::scrub("h\u{e9}llo w\u{f6}rld \u{1F43A}").contains('\u{1F43A}'));
}

#[test]
fn proxied_turns_land_in_the_timeline() {
    let upstream_port = spawn_upstream();

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store.new_session(Some("scripted agent")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let running = Arc::new(AtomicBool::new(true));
    let cfg = ProxyConfig {
        port: proxy_port,
        upstream: format!("http://127.0.0.1:{upstream_port}"),
        session,
        retention: Some(mayday::store::RetentionPolicy { max_events: None, max_age_days: None }),
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = capture::serve(s2, cfg, r2);
    });
    wait_port(proxy_port);

    let resp = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .set("Authorization", "Bearer supersecrettoken123456")
        .send_string("{\"model\":\"stub\",\"api_key\":\"Bearer supersecrettoken123456\",\"messages\":[{\"role\":\"user\",\"content\":\"why did you do that\"}]}")
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.into_string().unwrap();
    assert!(text.contains("why did you do that"));
    // the auth header must be forwarded upstream, not just dropped
    assert!(text.contains("auth_received"));
    assert!(text.contains("Bearer supersecrettoken123456"));

    let resp2 = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string("{\"model\":\"stub\",\"messages\":[{\"role\":\"user\",\"content\":\"second hop\"}]}")
        .unwrap();
    assert_eq!(resp2.status(), 200);

    running.store(false, Ordering::Relaxed);

    let events = store.events(session).unwrap();
    assert_eq!(events.len(), 2);
    let first = &events[0];
    eprintln!("DEBUG summary={:?} kind={:?} head={:.80}", first.summary, first.kind, first.payload);
    assert_eq!(first.kind, mayday::event::EventKind::ModelTurn);
    assert!(first.summary.contains("POST /v1/chat/completions"));
    assert!(first.payload.contains("why did you do that"));
    assert!(!first.payload.contains("supersecrettoken123456"));
    assert!(first.payload.contains("[redacted]"));

    // the database file itself must hold no readable content
    let bytes = std::fs::read(&db_path).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("supersecrettoken123456"));
    assert!(!raw.contains("why did you do that"));
}

/// SSE upstream: streams three data events with small gaps, then closes.
fn spawn_sse_upstream() -> u16 {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut chunk = [0u8; 8192];
            let _ = stream.read(&mut chunk);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            for part in ["data: alpha\n\n", "data: beta\n\n", "data: gamma\n\n"] {
                thread::sleep(Duration::from_millis(30));
                let _ = stream.write_all(part.as_bytes());
                let _ = stream.flush();
            }
            // dropping the stream closes it: the SSE body ends at EOF
        }
    });
    port
}

#[test]
fn sse_turns_stream_through_and_record_assembled() {
    use std::io::Read as _;
    use std::sync::atomic::AtomicBool;

    let up = spawn_sse_upstream();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("t.db"), Some(K)).unwrap());
    let s = store.new_session(Some("sse run")).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let port = 23000 + (std::process::id() % 20000) as u16;
    let store2 = store.clone();
    thread::spawn(move || {
        let _ = capture::serve(
            store2,
            ProxyConfig {
                port,
                upstream: format!("http://127.0.0.1:{up}"),
                session: s,
                retention: None,
            },
            running,
        );
    });
    wait_port(port);

    // the client receives every streamed chunk, in order
    let resp = ureq::post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string("{\"model\":\"stub\",\"stream\":true,\"messages\":[{\"role\":\"user\",\"content\":\"sse-ping\"}]}")
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.content_type().contains("text/event-stream"));
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body).unwrap();
    for part in ["alpha", "beta", "gamma"] {
        assert!(body.contains(part), "streamed body missing {part}");
    }

    // and the timeline still records ONE hop with the assembled body
    let events = store.events(s).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].payload.contains("alpha"));
    assert!(events[0].payload.contains("gamma"));
    assert!(events[0].payload.contains("\"streamed\":true"));
    assert!(events[0].summary.contains("streamed"));
}

/// An upstream that answers every request with a canned tool_calls JSON body
/// whose arguments carry a secret: the derived tool_call hops must appear in
/// the timeline, ordered after the turn, scrubbed like every other body.
#[test]
fn tool_calls_are_derived_from_recorded_turns() {
    let tool_body = r#"{"choices":[{"message":{"role":"assistant","tool_calls":[
        {"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Tokyo\",\"key\":\"sk-secret123456789012\"}"}},
        {"id":"call_2","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"/tmp/x\"}"}}
    ]}}]}"#;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut chunk = [0u8; 8192];
            let _ = stream.read(&mut chunk);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                tool_body.len(),
                tool_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store.new_session(Some("tool-call harness")).unwrap();

    let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener2.local_addr().unwrap().port();
    drop(listener2);

    let running = Arc::new(AtomicBool::new(true));
    let cfg = ProxyConfig {
        port: proxy_port,
        upstream: format!("http://127.0.0.1:{port}"),
        session,
        retention: Some(mayday::store::RetentionPolicy { max_events: None, max_age_days: None }),
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = capture::serve(s2, cfg, r2);
    });
    wait_port(proxy_port);

    let resp = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string("{}")
        .unwrap();
    assert_eq!(resp.status(), 200);
    running.store(false, Ordering::Relaxed);

    let events = store.events(session).unwrap();
    assert_eq!(events.len(), 3, "one turn + two derived tool calls");
    assert_eq!(events[0].kind, mayday::event::EventKind::ModelTurn);
    for e in &events[1..] {
        assert_eq!(e.kind, mayday::event::EventKind::ToolCall);
        assert!(e.summary.starts_with("tool_call "));
        let payload: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
        assert_eq!(payload["from_hop"], 1, "derived calls link back to the turn");
    }
    assert!(events[1].payload.contains("get_weather"));
    assert!(events[1].payload.contains("Tokyo"));
    assert!(events[2].payload.contains("write_file"));
    // secrets inside tool arguments are scrubbed at write time too
    let masked = capture::scrub("sk-secret123456789012");
    assert!(!masked.contains("sk-secret123456789012"));
    let mut secret_outside = false;
    for e in &events {
        if e.payload.contains("sk-secret123456789012") {
            secret_outside = true;
        }
    }
    assert!(!secret_outside, "raw secret must not survive scrub");
    assert!(events[1].payload.contains(&masked) || !events[1].payload.contains("sk-"));

    // the database file itself must hold no readable secret
    let bytes = std::fs::read(&db_path).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("sk-secret123456789012"));
}
