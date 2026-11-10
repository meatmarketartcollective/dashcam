use mayday::capture::{self, ProxyConfig};
use mayday::store::Store;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
            let resp_body = format!("{{\"echo\":{}}}", serde_json::json!(req_body));
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
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = capture::serve(s2, cfg, r2);
    });
    thread::sleep(Duration::from_millis(300));

    let resp = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .set("Authorization", "Bearer supersecrettoken123456")
        .send_string("{\"model\":\"stub\",\"api_key\":\"Bearer supersecrettoken123456\",\"messages\":[{\"role\":\"user\",\"content\":\"why did you do that\"}]}")
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.into_string().unwrap();
    assert!(text.contains("why did you do that"));

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
