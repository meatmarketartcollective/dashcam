use crate::event::EventKind;
use crate::store::Store;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct ProxyConfig {
    pub port: u16,
    pub upstream: String,
    pub session: i64,
    /// Applied after every recorded hop when set.
    pub retention: Option<crate::store::RetentionPolicy>,
}

/// Forward every request to the upstream provider and record both directions
/// as one model_turn event per exchange.
///
/// Redaction is always on: request and response bodies are scrubbed of
/// token-shaped secrets at write time, and headers are never recorded at all,
/// so an API key has nowhere to land in the timeline.
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

        let (status, resp_body) =
            forward(&cfg.upstream, &method, &path, &content_type, &auth_header, &body);
        let payload = serde_json::json!({
            "method": method,
            "path": path,
            "request_body": scrub(&String::from_utf8_lossy(&body)),
            "status": status,
            "response_body": scrub(&String::from_utf8_lossy(&resp_body)),
        })
        .to_string();
        let summary = format!("{method} /{path} -> {status}");
        let _ = store.append(cfg.session, EventKind::ModelTurn, &summary, &payload);
        if let Some(policy) = &cfg.retention {
            let _ = store.prune(policy);
        }

        let response = tiny_http::Response::from_data(resp_body)
            .with_status_code(status)
            .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
        let _ = rq.respond(response);
    }
    Ok(())
}

fn forward(
    upstream: &str,
    method: &str,
    path: &str,
    content_type: &str,
    auth: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let url = format!("{}/{}", upstream.trim_end_matches('/'), path);
    let req = ureq::request(method, &url);
    let req = if content_type.is_empty() {
        req
    } else {
        req.set("Content-Type", content_type)
    };
    // The auth header is forwarded upstream but never recorded: the stored
    // payload below holds bodies only, so a key still has nowhere to land.
    let req = if auth.is_empty() {
        req
    } else {
        req.set("Authorization", auth)
    };
    match req.send_bytes(body) {
        Ok(resp) => {
            let status = resp.status();
            let mut out = Vec::new();
            let _ = resp.into_reader().read_to_end(&mut out);
            (status, out)
        }
        Err(e) => {
            let msg = format!("{{\"mayday_upstream_error\":\"{}\"}}", e);
            (502, msg.into_bytes())
        }
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
            out.extend_from_slice(&b[i..i + 1]);
            i += 1;
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
