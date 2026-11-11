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
            // read until the full request body has arrived: under load the
            // request can arrive in more than one TCP segment, and a single
            // read echoes a truncated body
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                let Some(pos) = text.find("\r\n\r\n") else { continue };
                let headers = &text[..pos];
                let cl = headers
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok());
                match cl {
                    Some(cl) if buf.len() >= pos + 4 + cl => break,
                    Some(_) => continue,
                    None => break,
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
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

/// The real binary, end to end: record proxies a turn into the encrypted
/// timeline, then the CLI reads it back through the app.
#[test]
fn record_binary_captures_end_to_end() {
    let upstream_port = spawn_upstream();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timeline.db");

    // Spawn `dashcam record` on a fresh port; occasionally a just-dropped
    // ephemeral port is refused on rebind, so retry with a new port.
    let mut child_opt: Option<std::process::Child> = None;
    let mut port = 0u16;
    for attempt in 0..5 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = listener.local_addr().unwrap().port();
        drop(listener);
        port = p;
        let mut c = Command::new(env!("CARGO_BIN_EXE_dashcam"))
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
            .expect("spawn dashcam record");
        let stdout = c.stdout.take().unwrap();
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
        if ready {
            child_opt = Some(c);
            break;
        }
        let mut err = String::new();
        if let Some(e) = c.stderr.take() {
            let _ = BufReader::new(e).read_to_string(&mut err);
        }
        let _ = c.kill();
        let _ = c.wait();
        if attempt == 4 {
            panic!("dashcam record never announced readiness after 5 attempts; last stdout: {seen:?}; stderr: {err}");
        }
    }
    let mut child = child_opt.expect("dashcam record spawned");

    // the harness points its API base URL at the proxy; on a loaded box the
    // first few connections can fail while the listener settles
    let mut resp = None;
    for _ in 0..25 {
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
    let store = dashcam::store::Store::open(&db, Some(key_hex.trim())).unwrap();
    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].2.as_deref(), Some("scripted agent"));
    let events = store.events(1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, dashcam::event::EventKind::ModelTurn);
    assert!(events[0].payload.contains("scripted-agent-ping"));
    // the db on disk holds no readable text
    let bytes = std::fs::read(&db).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("scripted-agent-ping"));
}
