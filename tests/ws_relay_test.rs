//! End-to-end coverage for the websocket relay: a real WS client through the
//! live relay must reach a real upstream, get frames back, leave one
//! model_turn hop with the frame transcript, and a garbage connection must
//! not take the server down.

use dashcam::event::EventKind;
use dashcam::redact::Redactor;
use dashcam::store::Store;
use dashcam::ws::{self, accept_key, read_frame_blocking, write_frame, WsConfig};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const HARNESS_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const PING: &str = r#"{"type":"ping","seq":1}"#;
const PONG: &str = r#"{"type":"pong","v":42}"#;

fn wait_port(port: u16) {
    for _ in 0..250 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("ws port {port} never came up");
}

/// A real websocket upstream: completes the upgrade, then answers every text
/// frame with a fixed provider reply and honors close frames.
fn spawn_ws_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if String::from_utf8_lossy(&buf).contains("\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let key = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("sec-websocket-key:"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
            assert!(!key.is_empty(), "upstream saw no Sec-WebSocket-Key");
            let reply = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(&key)
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
            loop {
                match read_frame_blocking(&mut stream) {
                    Ok(f) => {
                        if f.opcode == 8 {
                            break;
                        }
                        if f.opcode == 1 {
                            write_frame(&mut stream, true, 1, PONG.as_bytes(), false).unwrap();
                            stream.flush().unwrap();
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    port
}

fn start_relay(store: &Arc<Store>, session: i64, upstream_port: u16) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let running = Arc::new(AtomicBool::new(true));
    let cfg = WsConfig {
        port,
        upstream: format!("ws://127.0.0.1:{upstream_port}/v1/realtime"),
        session,
        retention: None,
        redactor: Redactor::builtin(),
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = ws::serve(s2, cfg, r2);
    });
    wait_port(port);
    (port, running)
}

/// One clean session: upgrade, one masked text frame each way, close.
fn run_clean_session(port: u16) {
    let mut c = loop {
        if let Ok(c) = TcpStream::connect(("127.0.0.1", port)) {
            break c;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let req = format!(
        "GET /v1/realtime HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {HARNESS_KEY}\r\nSec-WebSocket-Version: 13\r\nx-dashcam-agent: test-agent\r\n\r\n"
    );
    c.write_all(req.as_bytes()).unwrap();
    let mut resp = Vec::new();
    let mut b = [0u8; 1];
    loop {
        c.read_exact(&mut b).unwrap();
        resp.push(b[0]);
        if resp.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&resp).to_string();
    assert!(text.contains("101"), "handshake failed: {text}");
    assert!(
        text.contains(accept_key(HARNESS_KEY).as_str()),
        "accept key mismatch: {text}"
    );

    write_frame(&mut c, true, 1, PING.as_bytes(), true).unwrap();
    c.flush().unwrap();
    let f = read_frame_blocking(&mut c).expect("relayed provider frame");
    assert_eq!(f.opcode, 1);
    assert_eq!(String::from_utf8_lossy(&f.payload), PONG);

    write_frame(&mut c, true, 8, b"", true).unwrap();
    drop(c);
}

fn wait_for_hops(store: &Store, session: i64, want: usize) -> Vec<dashcam::event::Event> {
    for _ in 0..500 {
        let events = store.events(session).unwrap();
        if events.len() >= want {
            return events;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timeline never reached {want} hops (has {})",
        store.events(session).unwrap().len()
    );
}

#[test]
fn full_relay_records_a_model_turn_hop() {
    let upstream_port = spawn_ws_upstream();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db, Some(K)).unwrap());
    let session = store.new_session(Some("ws harness")).unwrap();
    let (port, running) = start_relay(&store, session, upstream_port);

    run_clean_session(port);
    running.store(false, Ordering::Relaxed);

    let events = wait_for_hops(&store, session, 1);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].kind, EventKind::ModelTurn));
    let payload: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
    assert_eq!(payload["transport"], "websocket");
    assert_eq!(payload["agent"], "test-agent");
    assert_eq!(payload["request_body"], PING);
    assert_eq!(payload["response_body"], PONG);
    let frames = payload["frames"].as_array().expect("frames transcript");
    assert!(frames.len() >= 2, "expected both directions recorded");
    assert_eq!(frames[0]["dir"], "c2s");
    assert!(events[0].summary.contains("websocket"));
}

#[test]
fn garbage_connection_does_not_kill_the_relay() {
    let upstream_port = spawn_ws_upstream();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db, Some(K)).unwrap());
    let session = store.new_session(Some("ws harness")).unwrap();
    let (port, running) = start_relay(&store, session, upstream_port);

    // a connection that never speaks websocket must fail its handshake and
    // leave the accept loop alive for the next client
    let mut bad = TcpStream::connect(("127.0.0.1", port)).unwrap();
    bad.write_all(b"not a websocket request\r\n\r\n").unwrap();
    drop(bad);
    thread::sleep(Duration::from_millis(100));

    run_clean_session(port);
    running.store(false, Ordering::Relaxed);

    let events = wait_for_hops(&store, session, 1);
    assert_eq!(events.len(), 1, "only the clean session records a hop");
    let payload: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
    assert_eq!(payload["response_body"], PONG);
}
