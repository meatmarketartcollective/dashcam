//! Practical end-to-end coverage for v0.1.2 usage tracking: a recorded
//! model_turn hop must carry the token counts parsed from the provider
//! response, and `dashcam costs` must sum and price them correctly.

use dashcam::capture::{self, ProxyConfig};
use dashcam::event::EventKind;
use dashcam::store::Store;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn wait_port(port: u16) {
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("proxy port {port} never came up");
}

/// A plain-JSON OpenAI-shaped upstream: answers every POST with a body that
/// carries a model and a usage block, the way real providers do.
fn spawn_usage_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
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
            let body =
                r#"{"model":"gpt-5-mini","usage":{"prompt_tokens":120,"completion_tokens":45},"choices":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

/// The proxy must parse the provider response on a recorded model_turn and
/// attach the tokens to the stored hop. A unit test on the parser alone does
/// not prove the hop actually carries usage; this one drives the real proxy.
#[test]
fn model_turn_hops_carry_token_usage() {
    let upstream_port = spawn_usage_upstream();

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store.new_session(Some("usage harness")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    drop(listener);

    let running = Arc::new(AtomicBool::new(true));
    let cfg = ProxyConfig {
        port: proxy_port,
        upstream: format!("http://127.0.0.1:{upstream_port}"),
        session,
        retention: Some(dashcam::store::RetentionPolicy {
            max_events: None,
            max_age_days: None,
            ..Default::default()
        }),
        redactor: dashcam::redact::Redactor::builtin(),
    };
    let s2 = store.clone();
    let r2 = running.clone();
    thread::spawn(move || {
        let _ = capture::serve(s2, cfg, r2);
    });
    wait_port(proxy_port);

    let resp = ureq::post(&format!("http://127.0.0.1:{proxy_port}/v1/chat/completions"))
        .set("Content-Type", "application/json")
        .send_string(r#"{"model":"gpt-5-mini","messages":[{"role":"user","content":"ping"}],"usage":{"prompt_tokens":120,"completion_tokens":45}}"#)
        .unwrap();
    assert_eq!(resp.status(), 200);

    // the proxy writes the hop after the response is on the wire; poll
    for _ in 0..500 {
        if store.events(session).unwrap().len() >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    running.store(false, Ordering::Relaxed);

    let events = store.events(session).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].kind, EventKind::ModelTurn));
    let payload: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
    let usage = payload.get("usage").expect("model_turn hop must carry usage");
    assert_eq!(usage["model"], "gpt-5-mini");
    assert_eq!(usage["prompt_tokens"], 120);
    assert_eq!(usage["completion_tokens"], 45);
}

fn priced_store(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("t.db");
    let store = Store::open(&db, Some(K)).unwrap();
    std::fs::write(dir.join("key"), K).unwrap();
    let session = store.new_session(Some("cost harness")).unwrap();
    let hop = |model: &str, pt: u64, ct: u64| {
        serde_json::json!({"usage": {"model": model, "prompt_tokens": pt, "completion_tokens": ct}})
            .to_string()
    };
    store
        .append_get_seq(session, EventKind::ModelTurn, "POST /v1/chat/completions -> 200", &hop("gpt-5-mini", 1_000_000, 500_000))
        .unwrap();
    store
        .append_get_seq(session, EventKind::ModelTurn, "POST /v1/chat/completions -> 200", &hop("gpt-5-mini-2026-01", 1_000_000, 1_000_000))
        .unwrap();
    store
        .append_get_seq(session, EventKind::ModelTurn, "POST /v1/chat/completions -> 200", &hop("mystery-model", 10, 5))
        .unwrap();
    drop(store);
    db
}

/// `dashcam costs` must sum usage across hops per model, longest-prefix-match
/// the price CSV, and compute dollars from the user-supplied prices.
#[test]
fn costs_command_sums_tokens_and_prices() {
    let dir = tempfile::tempdir().unwrap();
    let db = priced_store(dir.path());

    let prices = dir.path().join("prices.csv");
    std::fs::write(&prices, "model,in,out\ngpt-5-mini,0.25,2.00\ngpt-5-mini-2026-01,0.30,2.40\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_dashcam"))
        .args(["--db", db.to_str().unwrap(), "costs", "--prices", prices.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // the dated variant is priced off the longer (more specific) prefix row
    assert!(text.contains("gpt-5-mini\t1 turns\t1000000 in\t500000 out\t$1.2500"), "got: {text}");
    assert!(text.contains("gpt-5-mini-2026-01\t1 turns\t1000000 in\t1000000 out\t$2.7000"), "got: {text}");
    assert!(text.contains("mystery-model\t1 turns\t10 in\t5 out\t(no price)"), "got: {text}");
    assert!(text.contains("total\t2000010 in\t1500005 out\t$3.9500"), "got: {text}");
}

/// A store with no usage yet prints the guidance line instead of a report.
#[test]
fn costs_on_a_fresh_store_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, Some(K)).unwrap();
    let session = store.new_session(Some("empty harness")).unwrap();
    store
        .append_get_seq(session, EventKind::ModelTurn, "POST /v1/chat/completions -> 200", r#"{"choices":[]}"#)
        .unwrap();
    drop(store);
    std::fs::write(dir.path().join("key"), K).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_dashcam"))
        .args(["--db", db.to_str().unwrap(), "costs"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("no token usage recorded on model_turn hops yet"), "got: {text}");
}
