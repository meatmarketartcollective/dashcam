use crate::event::EventKind;
use crate::store::Store;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct ProxyConfig {
    pub port: u16,
    pub upstream: String,
    pub session: i64,
    /// Applied after every recorded hop when set.
    pub retention: Option<crate::store::RetentionPolicy>,
}

/// Record one model turn hop, then derive tool_call hops from its response.
///
/// A model turn that carries tool calls gets one extra hop per call, written
/// right after the turn so the timeline order matches the run's execution
/// order. Arguments are scrubbed like every other recorded body, and each
/// derived hop links back to the turn that produced it.
fn record_turn(
    store: &std::sync::Arc<crate::store::Store>,
    cfg: &ProxyConfig,
    method: &str,
    path: &str,
    status: u16,
    request_body: &str,
    response_body: &str,
    streamed: bool,
) {
    let mut payload = serde_json::json!({
        "method": method,
        "path": path,
        "request_body": scrub(request_body),
        "status": status,
        "response_body": scrub(response_body),
    });
    if streamed {
        payload["streamed"] = serde_json::Value::Bool(true);
    }
    let payload = payload.to_string();
    let summary = if streamed {
        format!("{method} /{path} -> {status} (streamed)")
    } else {
        format!("{method} /{path} -> {status}")
    };
    let turn_seq = store
        .append_get_seq(cfg.session, EventKind::ModelTurn, &summary, &payload)
        .unwrap_or(0);
    for tc in crate::derive::tool_calls_from_response(response_body, streamed) {
        let tc_payload = serde_json::json!({
            "tool": tc.name,
            "call_id": tc.id,
            "arguments": scrub(&tc.arguments),
            "from_hop": turn_seq,
        })
        .to_string();
        let tc_summary = format!("tool_call {}", tc.name);
        let _ = store.append(
            cfg.session,
            EventKind::ToolCall,
            &tc_summary,
            &tc_payload,
        );
    }
    if let Some(policy) = &cfg.retention {
        let _ = store.prune(policy);
    }
}

/// Forward every request to the upstream provider and record both directions
/// as one model_turn event per exchange.
///
/// Streaming (text/event-stream) responses are passed through chunk by chunk
/// while the assembled body is still recorded as one hop, so replay shows the
/// full turn. Redaction is always on: request and response bodies are scrubbed
/// of token-shaped secrets at write time, and headers are never recorded at
/// all, so an API key has nowhere to land in the timeline.
pub fn serve(store: Arc<Store>, cfg: ProxyConfig, running: Arc<AtomicBool>) -> Result<(), String> {
    let server = tiny_http::Server::http(("127.0.0.1", cfg.port))
        .map_err(|e| format!("cannot bind capture port: {e}"))?;
    while running.load(Ordering::Relaxed) {
        let mut rq = match server.recv() {
            Ok(r) => r,
            Err(e) => return Err(format!("capture loop error: {e}")),
        };
        let mut body = Vec::new();
        let _ = rq.as_reader().read_to_end(&mut body);
        let method = rq.method().as_str().to_string();
        let path = rq.url().trim_start_matches('/').to_string();
        let mut content_type = String::new();
        let mut auth_header = String::new();
        for h in rq.headers() {
            let name = h.field.as_str().as_str();
            if name.eq_ignore_ascii_case("content-type") {
                content_type = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("authorization") {
                auth_header = h.value.as_str().to_string();
            }
        }

        match send_upstream(
            &cfg.upstream, &method, &path, &content_type, &auth_header, &body,
        ) {
            Ok(resp) => {
                let status = resp.status();
                let upstream_ct = resp.content_type().to_string();
                if upstream_ct.contains("text/event-stream") {
                    // Stream mode: forward chunks live to the client while a
                    // shared buffer assembles the full body for the timeline.
                    let acc: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
                    let reader = RecordingReader {
                        inner: resp.into_reader(),
                        buf: Arc::clone(&acc),
                    };
                    let ct_header =
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], upstream_ct.as_bytes())
                            .unwrap();
                    // data_length None: the body length is unknown while it
                    // streams, so tiny_http answers HTTP/1.1 clients chunked.
                    let response = tiny_http::Response::new(
                        tiny_http::StatusCode(status),
                        vec![ct_header],
                        reader,
                        None,
                        None,
                    );
                    let _ = rq.respond(response);
                    let captured = acc.lock().expect("stream buffer").clone();
                    record_turn(
                        &store,
                        &cfg,
                        &method,
                        &path,
                        status,
                        &String::from_utf8_lossy(&body),
                        &String::from_utf8_lossy(&captured),
                        true,
                    );
                } else {
                    let mut resp_body = Vec::new();
                    let _ = resp.into_reader().read_to_end(&mut resp_body);
                    record_turn(
                        &store,
                        &cfg,
                        &method,
                        &path,
                        status,
                        &String::from_utf8_lossy(&body),
                        &String::from_utf8_lossy(&resp_body),
                        false,
                    );
                    let response = tiny_http::Response::from_data(resp_body)
                        .with_status_code(status)
                        .with_header(tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap());
                    let _ = rq.respond(response);
                }
            }
            Err(e) => {
                let msg = format!("{{\"mayday_upstream_error\":\"{e}\"}}");
                let payload = serde_json::json!({
                    "method": method,
                    "path": path,
                    "request_body": scrub(&String::from_utf8_lossy(&body)),
                    "status": 502,
                    "response_body": &msg,
                })
                .to_string();
                let summary = format!("{method} /{path} -> 502");
                let _ = store.append(cfg.session, EventKind::ModelTurn, &summary, &payload);
                if let Some(policy) = &cfg.retention {
                    let _ = store.prune(policy);
                }
                let response = tiny_http::Response::from_string(msg).with_status_code(502);
                let _ = rq.respond(response);
            }
        }
    }
    Ok(())
}

fn send_upstream(
    upstream: &str,
    method: &str,
    path: &str,
    content_type: &str,
    auth: &str,
    body: &[u8],
) -> Result<ureq::Response, String> {
    let url = format!("{}/{}", upstream.trim_end_matches('/'), path);
    let req = ureq::request(method, &url);
    let req = if content_type.is_empty() {
        req
    } else {
        req.set("Content-Type", content_type)
    };
    // The auth header is forwarded upstream but never recorded: the stored
    // payload holds bodies only, so a key still has nowhere to land.
    let req = if auth.is_empty() {
        req
    } else {
        req.set("Authorization", auth)
    };
    req.send_bytes(body).map_err(|e| e.to_string())
}

/// A Read that forwards upstream bytes to the HTTP client as they arrive
/// while accumulating everything for the timeline write after the response.
struct RecordingReader<R: Read> {
    inner: R,
    buf: Arc<Mutex<Vec<u8>>>,
}

impl<R: Read> Read for RecordingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        self.buf.lock().expect("stream buffer").extend_from_slice(&out[..n]);
        Ok(n)
    }
}

/// Mask token-shaped secrets in recorded text. Always on; no opt-out in v1.
pub fn scrub(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let prefix_len = if s[i..].starts_with("sk-") {
            3
        } else if s[i..].starts_with("Bearer ") {
            7
        } else {
            // advance a full character: byte-by-byte would land i inside a
            // multi-byte UTF-8 char and panic on the slice above
            let ch_len = s[i..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            out.extend_from_slice(&b[i..i + ch_len]);
            i += ch_len;
            continue;
        };
        out.extend_from_slice(&b[i..i + prefix_len]);
        let mut j = i + prefix_len;
        while j < b.len()
            && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.' || b[j] == b'-')
        {
            j += 1;
        }
        if j - (i + prefix_len) >= 8 {
            out.extend_from_slice(b"[redacted]");
        } else {
            out.extend_from_slice(&b[i + prefix_len..j]);
        }
        i = j;
    }
    String::from_utf8_lossy(&out).into_owned()
}
