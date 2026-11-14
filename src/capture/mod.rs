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
    /// Redaction applied at write time; built-ins always on, a rules file
    /// (via `--rules`) may add more.
    pub redactor: crate::redact::Redactor,
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
    provider: &str,
    agent: &str,
    parent: &str,
    run: &str,
) {
    let mut payload = serde_json::json!({
        "method": method,
        "path": path,
        "provider": provider,
        "request_body": cfg.redactor.scrub(request_body),
        "status": status,
        "response_body": cfg.redactor.scrub(response_body),
    });
    if !agent.is_empty() {
        payload["agent"] = serde_json::Value::String(agent.to_string());
    }
    if !parent.is_empty() {
        payload["parent"] = serde_json::Value::String(parent.to_string());
    }
    if !run.is_empty() {
        payload["run"] = serde_json::Value::String(run.to_string());
    }
    if streamed {
        payload["streamed"] = serde_json::Value::Bool(true);
    }
    if let Some(u) = crate::usage::usage_from_response(provider, request_body, response_body, streamed) {
        payload["usage"] = serde_json::json!({
            "model": u.model,
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
        });
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
    let derived = match provider {
        "anthropic" => crate::derive::tool_calls_from_anthropic(response_body, streamed),
        "gemini" => crate::derive::tool_calls_from_gemini(response_body, streamed),
        _ => crate::derive::tool_calls_from_response(response_body, streamed),
    };
    for tc in derived {
        let mut tc_payload = serde_json::json!({
            "tool": tc.name,
            "call_id": tc.id,
            "arguments": cfg.redactor.scrub(&tc.arguments),
            "from_hop": turn_seq,
        });
        if !agent.is_empty() {
            tc_payload["agent"] = serde_json::Value::String(agent.to_string());
        }
        if !parent.is_empty() {
            tc_payload["parent"] = serde_json::Value::String(parent.to_string());
        }
        if !run.is_empty() {
            tc_payload["run"] = serde_json::Value::String(run.to_string());
        }
        let tc_payload = tc_payload.to_string();
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

/// Record one exchange a shim already sent straight to the provider.
///
/// The shim POSTs the exchange as JSON (provider, method, path, status,
/// request_body, response_body, streamed, agent) and the daemon writes it
/// through the same path as proxied traffic: redaction at write time, one
/// model_turn hop, derived tool-call hops from the provider dialect. Shims
/// and the proxy therefore produce identical records.
fn handle_ingest(
    store: &std::sync::Arc<crate::store::Store>,
    cfg: &ProxyConfig,
    body: &str,
) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bad json: {e}"))?;
    let provider = match v["provider"].as_str() {
        Some("anthropic") => "anthropic",
        Some("gemini") => "gemini",
        _ => "openai",
    };
    let method = v["method"].as_str().unwrap_or("POST").to_string();
    let path = v["path"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_string();
    let status = v["status"].as_u64().unwrap_or(0) as u16;
    let request_body = v["request_body"].as_str().unwrap_or("").to_string();
    let response_body = v["response_body"].as_str().unwrap_or("").to_string();
    let streamed = v["streamed"].as_bool().unwrap_or(false);
    let agent = v["agent"].as_str().unwrap_or("").to_string();
    let mut parent = v["parent"].as_str().unwrap_or("").to_string();
    let mut run = v["run"].as_str().unwrap_or("").to_string();
    let agent = if agent.is_empty() { std::env::var("DASHCAM_AGENT_ID").unwrap_or_default() } else { agent };
    if parent.is_empty() {
        parent = std::env::var("DASHCAM_PARENT").unwrap_or_default();
    }
    if run.is_empty() {
        run = std::env::var("DASHCAM_RUN").unwrap_or_default();
    }
    record_turn(
        store,
        cfg,
        &method,
        &path,
        status,
        &request_body,
        &response_body,
        streamed,
        provider,
        &agent,
        &parent,
        &run,
    );
    Ok(format!("{{\"ok\":true,\"session\":{}}}", cfg.session))
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
        let mut x_api_key = String::new();
        let mut x_goog_api_key = String::new();
        let mut anthropic_version = String::new();
        let mut agent = String::new();
        let mut parent = String::new();
        let mut run = String::new();
        for h in rq.headers() {
            let name = h.field.as_str().as_str();
            if name.eq_ignore_ascii_case("content-type") {
                content_type = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("authorization") {
                auth_header = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("x-api-key") {
                // Anthropic native auth
                x_api_key = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("x-goog-api-key") {
                // Google Gemini native auth
                x_goog_api_key = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("anthropic-version") {
                anthropic_version = h.value.as_str().to_string();
            } else if name.eq_ignore_ascii_case("x-dashcam-agent") {
                // multi-agent trace propagation: which agent produced this hop
                agent = h.value.as_str().trim().to_string();
            } else if name.eq_ignore_ascii_case("x-dashcam-parent") {
                // spawn-tree attribution: the agent that spawned this one
                parent = h.value.as_str().trim().to_string();
            } else if name.eq_ignore_ascii_case("x-dashcam-run") {
                // swarm grouping: every agent of one run shares this id
                run = h.value.as_str().trim().to_string();
            }
        }
        if agent.is_empty() {
            agent = std::env::var("DASHCAM_AGENT_ID").unwrap_or_default();
        }
        if parent.is_empty() {
            parent = std::env::var("DASHCAM_PARENT").unwrap_or_default();
        }
        if run.is_empty() {
            run = std::env::var("DASHCAM_RUN").unwrap_or_default();
        }
        // Machine door for language shims: a shim forwards an exchange it
        // already sent directly to the provider. Recorded through the same
        // path as proxied traffic, then answered locally, never forwarded.
        if method == "POST" && path == "ingest" {
            match handle_ingest(&store, &cfg, &String::from_utf8_lossy(&body)) {
                Ok(reply) => {
                    let ct = tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"application/json"[..],
                    )
                    .expect("static header");
                    let _ = rq.respond(
                        tiny_http::Response::from_string(reply).with_header(ct),
                    );
                }
                Err(e) => {
                    let _ = rq.respond(
                        tiny_http::Response::from_string(format!(
                            "{{\"error\":\"{e}\"}}"
                        ))
                        .with_status_code(400),
                    );
                }
            }
            continue;
        }

        let provider = detect_provider(&path);

        match send_upstream(
            &cfg.upstream, &method, &path, &content_type, &auth_header, &body,
            &x_api_key, &x_goog_api_key, &anthropic_version,
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
                    let ct_header = tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        upstream_ct.as_bytes(),
                    )
                    .unwrap_or_else(|_| {
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/octet-stream"[..],
                        )
                        .expect("static header")
                    });
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
                        &provider,
                        &agent,
                        &parent,
                        &run,
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
                        &provider,
                        &agent,
                        &parent,
                        &run,
                    );
                    let response = tiny_http::Response::from_data(resp_body)
                        .with_status_code(status)
                        .with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .expect("static header"),
                        );
                    let _ = rq.respond(response);
                }
            }
            Err(e) => {
                let msg = format!("{{\"dashcam_upstream_error\":\"{e}\"}}");
                let payload = serde_json::json!({
                    "method": method,
                    "path": path,
                    "provider": detect_provider(&path),
                    "request_body": cfg.redactor.scrub(&String::from_utf8_lossy(&body)),
                    "status": 502,
                    "response_body": &msg,
                    "agent": if agent.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(agent.clone()) },
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
    x_api_key: &str,
    x_goog_api_key: &str,
    anthropic_version: &str,
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
    let req = if x_api_key.is_empty() {
        req
    } else {
        req.set("x-api-key", x_api_key)
    };
    let req = if x_goog_api_key.is_empty() {
        req
    } else {
        req.set("x-goog-api-key", x_goog_api_key)
    };
    let req = if anthropic_version.is_empty() {
        req
    } else {
        req.set("anthropic-version", anthropic_version)
    };
    req.send_bytes(body).map_err(|e| e.to_string())
}

/// Which provider dialect a request path speaks. The proxy stays a passive
/// recorder: bodies are stored raw either way, the tag only tells the
/// timeline and the tool-call deriver which shape to expect. Paths that
/// match none of the known dialects read as "openai", the OpenAI-compatible
/// shape most harnesses and gateways speak.
pub fn detect_provider(path: &str) -> &'static str {
    if path.contains("/messages") {
        "anthropic"
    } else if path.contains("generateContent") || path.contains("streamGenerateContent") {
        "gemini"
    } else if path.contains("/embeddings") || path.contains("mistral") {
        "openai"
    } else {
        "openai"
    }
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

#[cfg(test)]
mod ingest_tests {
    use super::*;
    use crate::redact::Redactor;

    #[test]
    fn ingest_records_a_model_turn_and_derives_tool_calls() {
        let dir = std::env::temp_dir().join(format!("dashcam-ingest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let db = dir.join("ingest-test.db");
        let store = std::sync::Arc::new(
            crate::store::Store::open(&db, None).expect("store"),
        );
        let session = store.new_session(Some("ingest-test")).expect("session");
        let cfg = ProxyConfig {
            port: 0,
            upstream: "http://127.0.0.1:1".into(),
            session,
            retention: None,
            redactor: Redactor::builtin(),
        };
        let body = serde_json::json!({
            "provider": "openai",
            "method": "POST",
            "path": "/v1/chat/completions",
            "status": 200,
            "request_body": "{\"model\":\"gpt-test\"}",
            "response_body": "{\"choices\":[]}",
            "streamed": false
        })
        .to_string();
        let reply = handle_ingest(&store, &cfg, &body).expect("ingest ok");
        assert!(reply.contains("\"ok\":true"));
        let events = store.events(session).expect("events");
        assert_eq!(
            1,
            events
                .iter()
                .filter(|e| e.kind == crate::event::EventKind::ModelTurn)
                .count()
        );
        // a token-shaped secret in a shim-posted body is scrubbed at write
        let body2 = body.replace("gpt-test", "sk-abcdefghijklmnop1234");
        handle_ingest(&store, &cfg, &body2).expect("ingest ok");
        let events = store.events(session).expect("events");
        assert!(!events.iter().any(|e| e.payload.contains("sk-abcdefghijklmnop1234")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
