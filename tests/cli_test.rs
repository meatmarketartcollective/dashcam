use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Minimal upstream stub: echoes the request body back as JSON.
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
        }
    });
    port
}

/// The M1 closer: the actual `mayday record` binary, a harness pointed at it,
/// and a timeline that decrypts with the key file the binary created.
#[test]
fn record_binary_captures_end_to_end() {
    let upstream_port = spawn_upstream();

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timeline.db");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut child = Command::new(env!("CARGO_BIN_EXE_mayday"))
        .args([
            "--db", db.to_str().unwrap(),
            "record",
            "--port", &port.to_string(),
            "--upstream", &format!("http://127.0.0.1:{upstream_port}"),
            "--note", "scripted agent",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mayday record");

    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut ready = false;
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..50 {
        match lines.next() {
            Some(Ok(line)) if line.contains("recording on") => {
                ready = true;
                break;
            }
            Some(Ok(line)) => { seen.push(line); continue }
            other => { seen.push(format!("stream ended: {other:?}")); break }
        }
    }
    if !ready {
        let mut err = String::new();
        if let Some(e) = child.stderr.take() {
            let _ = BufReader::new(e).read_to_string(&mut err);
        }
        panic!("mayday record never announced readiness; stdout: {seen:?}; stderr: {err}");
    }

    // the harness points its API base URL at the proxy
    let mut resp = None;
    for _ in 0..10 {
        if let Ok(r) = ureq::post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .set("Content-Type", "application/json")
            .send_string("{\"model\":\"stub\",\"messages\":[{\"role\":\"user\",\"content\":\"scripted-agent-ping\"}]}")
        {
            resp = Some(r);
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    let resp = resp.expect("proxy never answered");
    assert_eq!(resp.status(), 200);
    let body = resp.into_string().unwrap();
    assert!(body.contains("scripted-agent-ping"));

    let _ = child.kill();
    let _ = child.wait();

    // reopen with the key file the binary generated
    let key_hex = std::fs::read_to_string(dir.path().join("key")).unwrap();
    let store = mayday::store::Store::open(&db, Some(key_hex.trim())).unwrap();
    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].2.as_deref(), Some("scripted agent"));
    let events = store.events(1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, mayday::event::EventKind::ModelTurn);
    assert!(events[0].payload.contains("scripted-agent-ping"));
    // the db on disk holds no readable text
    let bytes = std::fs::read(&db).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("scripted-agent-ping"));
}
