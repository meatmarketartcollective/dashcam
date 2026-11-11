//! WebSocket passthrough for realtime and voice agents (model boundary).
//!
//! Some agents never speak request-response HTTP; they hold a WebSocket open
//! (OpenAI Realtime, Gemini Live, and similar). The harness points its
//! WebSocket client at this daemon's ws port instead of the provider. The
//! same zero-code-change trick as the HTTP proxy, a different dialect:
//! dashcam terminates the connection, records every frame in both directions
//! with timestamps, and forwards to the real provider over its own secure
//! connection. Auth headers are forwarded upstream but never recorded.
//!
//! One model_turn hop is written when a connection closes: `transport` is
//! "websocket", `frames` holds the ordered transcript, and `request_body` /
//! `response_body` carry the first text frame each way so the usual replay
//! panels still have something honest to show.

use crate::event::EventKind;
use crate::store::Store;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct WsConfig {
    pub port: u16,
    /// ws://host:port/path or wss://host/path for the real provider
    pub upstream: String,
    pub session: i64,
    pub retention: Option<crate::store::RetentionPolicy>,
    pub redactor: crate::redact::Redactor,
}

const MAX_FRAME: usize = 32 * 1024 * 1024;
const MAX_RECORDED: usize = 8 * 1024 * 1024;

/// Sec-WebSocket-Accept for a client's Sec-WebSocket-Key (RFC 6455 1.3).
pub fn accept_key(key: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    B64.encode(h.finalize())
}

pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn kind(&self) -> &'static str {
        match self.opcode {
            1 => "text",
            2 => "binary",
            8 | 9 | 10 => "control",
            _ => "other",
        }
    }
}

/// Write one WebSocket frame; client-side peers require masking.
pub fn write_frame(w: &mut impl Write, fin: bool, opcode: u8, payload: &[u8], mask: bool) -> Result<(), String> {
    let mut head = vec![(if fin { 0x80 } else { 0 }) | (opcode & 0x0f)];
    let len = payload.len() as u64;
    if len < 126 {
        head.push(if mask { 0x80 } else { 0 } | len as u8);
    } else if len <= u64::from(u16::MAX) {
        head.push(if mask { 0x80 } else { 0 } | 126);
        head.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        head.push(if mask { 0x80 } else { 0 } | 127);
        head.extend_from_slice(&len.to_be_bytes());
    }
    if mask {
        let mut m = [0u8; 4];
        getrandom::getrandom(&mut m).map_err(|e| format!("mask rng: {e}"))?;
        head.extend_from_slice(&m);
        let mut masked = Vec::with_capacity(payload.len());
        for (i, b) in payload.iter().enumerate() {
            masked.push(b ^ m[i % 4]);
        }
        w.write_all(&head).and_then(|_| w.write_all(&masked)).map_err(|e| format!("write: {e}"))
    } else {
        w.write_all(&head).and_then(|_| w.write_all(payload)).map_err(|e| format!("write: {e}"))
    }
}

const FORWARDED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "openai-beta",
    "anthropic-version",
    "x-goog-api-key",
];

/// Read an HTTP head (request or response) up to the blank line.
fn read_http_head(r: &mut impl Read) -> Result<(String, Vec<(String, String)>), String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        r.read_exact(&mut byte).map_err(|e| format!("http head: {e}"))?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err("http head too large".into());
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("").to_string();
    let headers = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Ok((first, headers))
}

/// Parse a complete WebSocket frame from the front of `buf`, consuming its
/// bytes. None while the frame is still incomplete.
fn parse_from_buf(buf: &mut Vec<u8>) -> Option<Frame> {
    if buf.len() < 2 {
        return None;
    }
    let fin = buf[0] & 0x80 != 0;
    let opcode = buf[0] & 0x0f;
    let masked = buf[1] & 0x80 != 0;
    let mut need = 2usize;
    let mut len = (buf[1] & 0x7f) as u64;
    if len == 126 {
        if buf.len() < 4 {
            return None;
        }
        len = u16::from_be_bytes([buf[2], buf[3]]) as u64;
        need = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return None;
        }
        len = u64::from_be_bytes(buf[2..10].try_into().expect("len64"));
        need = 10;
    }
    if len > MAX_FRAME as u64 {
        return None; // frame refused by the caller on overflow; never grows
    }
    let mask_len = if masked { 4 } else { 0 };
    need += mask_len + len as usize;
    if buf.len() < need {
        return None;
    }
    let mut payload = buf[need - len as usize..need].to_vec();
    if masked {
        let m = &buf[need - len as usize - 4..need - len as usize];
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    buf.drain(..need);
    Some(Frame { fin, opcode, payload })
}

/// Blocking frame read (handshake-time and tests).
pub fn read_frame_blocking(r: &mut impl Read) -> Result<Frame, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if let Some(f) = parse_from_buf(&mut buf) {
            return Ok(f);
        }
        let n = r.read(&mut chunk).map_err(|e| format!("frame read: {e}"))?;
        if n == 0 {
            return parse_from_buf(&mut buf)
                .ok_or_else(|| "connection closed mid-frame".to_string());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_FRAME {
            return Err("frame too large".into());
        }
    }
}

/// Nonblocking frame collector: returns the next complete frame or None.
struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    fn new() -> Self {
        FrameReader { buf: Vec::new() }
    }
    fn poll(&mut self, r: &mut impl Read) -> Result<Option<Frame>, String> {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(f) = parse_from_buf(&mut self.buf) {
                return Ok(Some(f));
            }
            if self.buf.len() > MAX_FRAME {
                return Err("frame too large".into());
            }
            match r.read(&mut chunk) {
                Ok(0) => {
                    return parse_from_buf(&mut self.buf)
                        .map(Some)
                        .ok_or_else(|| "connection closed".to_string())
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }
}

/// One end of the upstream connection: raw TCP for ws://, TLS for wss://.
enum Upstream {
    Plain(TcpStream),
    Tls(rustls::StreamOwned<rustls::ClientConnection, TcpStream>),
}

impl Read for Upstream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Upstream::Plain(s) => s.read(out),
            Upstream::Tls(t) => t.read(out),
        }
    }
}

impl Write for Upstream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Upstream::Plain(s) => s.write(buf),
            Upstream::Tls(t) => t.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Upstream::Plain(s) => s.flush(),
            Upstream::Tls(t) => t.flush(),
        }
    }
}

fn set_nonblocking(s: &mut Upstream, on: bool) -> Result<(), String> {
    let r = match s {
        Upstream::Plain(t) => t.set_nonblocking(on),
        Upstream::Tls(t) => t.sock.set_nonblocking(on),
    };
    r.map_err(|e| format!("nonblocking: {e}"))
}

/// Dial the real provider and complete the client side of the upgrade.
fn connect_upstream(
    url: &str,
    path: &str,
    extra_headers: &[(String, String)],
) -> Result<Upstream, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("ws upstream needs ws:// or wss://")?;
    let tls = scheme.eq_ignore_ascii_case("wss");
    let (hostport, _base_path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| "bad ws port")?),
        None => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    let tcp = TcpStream::connect((host.as_str(), port))
        .map_err(|e| format!("upstream connect: {e}"))?;
    let mut stream = if tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| "bad upstream host")?;
        let conn = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls setup: {e}"))?;
        Upstream::Tls(rustls::StreamOwned::new(conn, tcp))
    } else {
        Upstream::Plain(tcp)
    };
    let mut key = [0u8; 16];
    getrandom::getrandom(&mut key).map_err(|e| format!("rng: {e}"))?;
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n",
        path,
        hostport,
        B64.encode(key)
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("upstream handshake: {e}"))?;
    let (status, _resp_headers) = read_http_head(&mut stream)?;
    if !status.contains("101") {
        return Err(format!("upstream refused upgrade: {status}"));
    }
    Ok(stream)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Store one frame in the connection transcript. Text frames are scrubbed
/// at record time like every other body; the cap keeps a runaway session
/// from ballooning the timeline.
fn record_frame(
    dir: u8,
    f: &Frame,
    frames: &Arc<Mutex<Vec<serde_json::Value>>>,
    recorded: &Arc<Mutex<usize>>,
    redactor: &crate::redact::Redactor,
) {
    {
        let mut rec = recorded.lock().expect("recorded");
        if *rec > MAX_RECORDED {
            return;
        }
        *rec += f.payload.len();
    }
    let (kind, data) = match f.kind() {
        "text" => ("text", redactor.scrub(&String::from_utf8_lossy(&f.payload))),
        "binary" => ("binary", B64.encode(&f.payload)),
        "control" => (
            "control",
            if f.payload.is_empty() {
                String::new()
            } else {
                B64.encode(&f.payload)
            },
        ),
        _ => ("other", B64.encode(&f.payload)),
    };
    let ts = now_ms();
    frames.lock().expect("frames").push(serde_json::json!({
        "dir": if dir == 0 { "c2s" } else { "s2c" },
        "kind": kind,
        "ts": ts,
        "data": data,
    }));
}

/// Serve WebSocket passthrough on its own port until `running` goes false.
pub fn serve(store: Arc<Store>, cfg: WsConfig, running: Arc<AtomicBool>) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", cfg.port))
        .map_err(|e| format!("cannot bind ws port: {e}"))?;
    while running.load(Ordering::Relaxed) {
        let (stream, _) = match listener.accept() {
            Ok(x) => x,
            Err(e) => return Err(format!("ws accept: {e}")),
        };
        if let Err(e) = handle_conn(&store, &cfg, stream) {
            eprintln!("ws session: {e}");
        }
    }
    Ok(())
}

fn handle_conn(store: &Arc<Store>, cfg: &WsConfig, mut client: TcpStream) -> Result<(), String> {
    let _ = client.set_nodelay(true);
    let (req_line, headers) = read_http_head(&mut client)?;
    if !headers
        .iter()
        .any(|(k, v)| k == "upgrade" && v.eq_ignore_ascii_case("websocket"))
    {
        return Err("not a websocket handshake".into());
    }
    let key = headers
        .iter()
        .find(|(k, _)| k == "sec-websocket-key")
        .map(|(_, v)| v.clone())
        .ok_or("handshake missing Sec-WebSocket-Key")?;
    let path = req_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let reply = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    client
        .write_all(reply.as_bytes())
        .map_err(|e| format!("harness handshake: {e}"))?;

    let fwd: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| FORWARDED_HEADERS.contains(&k.as_str()))
        .cloned()
        .collect();
    let mut up = connect_upstream(&cfg.upstream, &path, &fwd)?;

    let frames: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let _ = client.set_nonblocking(true);
    set_nonblocking(&mut up, true)?;
    let mut fc = FrameReader::new();
    let mut fs = FrameReader::new();
    let mut last = std::time::Instant::now();
    loop {
        let mut active = false;
        // harness to provider: the provider side needs masked frames
        match fc.poll(&mut client) {
            Ok(Some(f)) => {
                active = true;
                let is_close = f.opcode == 8;
                record_frame(0, &f, &frames, &recorded, &cfg.redactor);
                let _ = write_frame(&mut up, f.fin, f.opcode, &f.payload, true);
                if is_close {
                    break;
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
        // provider to harness: server frames stay unmasked
        match fs.poll(&mut up) {
            Ok(Some(f)) => {
                active = true;
                let is_close = f.opcode == 8;
                record_frame(1, &f, &frames, &recorded, &cfg.redactor);
                let _ = write_frame(&mut client, f.fin, f.opcode, &f.payload, false);
                if is_close {
                    break;
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if active {
            last = std::time::Instant::now();
        } else if last.elapsed() > std::time::Duration::from_secs(600) {
            break;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    write_hop(store, cfg, &path, &frames);
    Ok(())
}

/// One model_turn hop per connection: the full ordered frame transcript.
fn write_hop(
    store: &Arc<Store>,
    cfg: &WsConfig,
    path: &str,
    frames: &Arc<Mutex<Vec<serde_json::Value>>>,
) {
    let frames = frames.lock().expect("frames").clone();
    let first = |dir: &str| {
        frames
            .iter()
            .find(|f| f["dir"] == dir && f["kind"] == "text")
            .and_then(|f| f["data"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let c2s = frames.iter().filter(|f| f["dir"] == "c2s").count();
    let s2c = frames.iter().filter(|f| f["dir"] == "s2c").count();
    let payload = serde_json::json!({
        "transport": "websocket",
        "provider": crate::capture::detect_provider(path),
        "upstream": cfg.upstream,
        "path": path,
        "frames": frames,
        "request_body": first("c2s"),
        "response_body": first("s2c"),
    });
    let summary = format!("websocket {c2s} out / {s2c} in frames");
    let _ = store.append(cfg.session, EventKind::ModelTurn, &summary, &payload.to_string());
    if let Some(policy) = &cfg.retention {
        let _ = store.prune(policy);
    }
}

#[cfg(test)]
mod ws_tests {
    use super::*;

    #[test]
    fn accept_key_matches_rfc_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frames_round_trip_masked_and_extended() {
        for (len, mask) in [(5usize, true), (200, true), (70_000, false), (5, false)] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut wire = Vec::new();
            write_frame(&mut wire, true, 1, &payload, mask).expect("write");
            let mut r: &[u8] = &wire;
            let f = read_frame_blocking(&mut r).expect("read");
            assert!(f.fin);
            assert_eq!(f.opcode, 1);
            assert_eq!(f.payload, payload);
            assert_eq!(f.kind(), "text");
        }
    }

    #[test]
    fn fragmented_frames_keep_their_fin_bit() {
        let mut wire = Vec::new();
        write_frame(&mut wire, false, 1, b"hello ", false).expect("write");
        write_frame(&mut wire, true, 0, b"world", false).expect("write");
        let mut r: &[u8] = &wire;
        let mut fr = FrameReader::new();
        let f1 = fr.poll(&mut r).expect("read1").expect("frame1");
        assert!(!f1.fin);
        assert_eq!(f1.opcode, 1);
        assert_eq!(f1.payload, b"hello ");
        let f2 = fr.poll(&mut r).expect("read2").expect("frame2");
        assert!(f2.fin);
        assert_eq!(f2.opcode, 0);
        assert_eq!(f2.payload, b"world");
    }

    #[test]
    fn parse_from_buf_handles_incremental_arrival() {
        let mut buf = Vec::new();
        assert!(parse_from_buf(&mut buf).is_none());
        buf.extend_from_slice(&[0x81, 0x85]);
        assert!(parse_from_buf(&mut buf).is_none());
        buf.extend_from_slice(&[0, 0, 0, 0]);
        assert!(parse_from_buf(&mut buf).is_none());
        buf.extend_from_slice(b"hello");
        let f = parse_from_buf(&mut buf).expect("frame");
        assert_eq!(f.payload, b"hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn handshake_head_parses() {
        let head = b"GET /v1/realtime HTTP/1.1\r\nHost: x\r\nUpgrade: WebSocket\r\nSec-WebSocket-Key: abc\r\n\r\n" as &[u8];
        let (line, headers) = read_http_head(&mut &head[..]).expect("head");
        assert!(line.starts_with("GET /v1/realtime"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "upgrade" && v.eq_ignore_ascii_case("websocket")));
        assert!(headers.iter().any(|(k, _)| k == "sec-websocket-key"));
    }
}
