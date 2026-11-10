use crate::store::Store;
use serde_json::{json, Value};
use std::sync::Arc;

/// The mayday scan surface over the Model Context Protocol: newline-delimited
/// JSON-RPC 2.0 on stdio, read-only. Every tool maps to a store read, so a
/// developer's coding agent can inspect the local flight recorder's timeline
/// without mayday exposing anything over the network.
pub struct Mcp {
    store: Arc<Store>,
}

impl Mcp {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Handle one incoming JSON-RPC line. Returns the response line to write
    /// back, or None for notifications (no response owed).
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let msg = match serde_json::from_str::<Value>(line) {
            Ok(m) => m,
            Err(_) => return Some(jsonrpc_error(Value::Null, -32700, "parse error")),
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
            "initialize" => Some(jsonrpc_ok(
                id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": {
                        "name": "mayday",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )),
            "notifications/initialized" => None,
            "resources/list" => Some(jsonrpc_ok(
                id,
                json!({ "resources": self.resources_list() }),
            )),
            "resources/read" => {
                let uri = msg
                    .pointer("/params/uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                match self.read_resource(uri) {
                    Ok(contents) => Some(jsonrpc_ok(id, json!({ "contents": [contents] }))),
                    Err(e) => Some(jsonrpc_error(id, -32002, &e)),
                }
            }
            "ping" => Some(jsonrpc_ok(id, json!({}))),
            "tools/list" => Some(jsonrpc_ok(id, json!({ "tools": tool_specs() }))),
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = msg
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Some(jsonrpc_ok(id, self.call(&name, &args)))
            }
            _ => {
                if msg.get("id").is_some() {
                    Some(jsonrpc_error(id, -32601, "method not found"))
                } else {
                    None
                }
            }
        }
    }

    /// One tools/call: dispatch by name, return the MCP result value.
    fn call(&self, name: &str, args: &Value) -> Value {
        let result: Result<String, String> = match name {
            "list_sessions" => self.list_sessions(),
            "list_hops" => self.list_hops(args),
            "search_timeline" => self.search_timeline(args),
            "get_hop" => self.get_hop(args),
            "get_diff" => self.get_diff(args),
            "summarize_session" => self.summarize_session(args),
            _ => Err(format!("unknown tool: {name}")),
        };
        match result {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(e) => json!({
                "content": [{ "type": "text", "text": e }],
                "isError": true,
            }),
        }
    }

    fn list_sessions(&self) -> Result<String, String> {
        let rows = self.store.sessions().map_err(|e| e.to_string())?;
        let out: Vec<Value> = rows
            .into_iter()
            .map(|(id, started, note, hops)| {
                json!({
                    "id": id,
                    "started_at": started,
                    "note": note,
                    "hops": hops,
                })
            })
            .collect();
        to_text(&json!(out))
    }

    fn list_hops(&self, args: &Value) -> Result<String, String> {
        let session = int_arg(args, "session").ok_or("missing argument: session (integer)")?;
        let events = self.store.events(session).map_err(|e| e.to_string())?;
        let out: Vec<Value> = events
            .iter()
            .map(|e| {
                json!({
                    "seq": e.seq,
                    "kind": serde_json::to_value(&e.kind).unwrap_or(json!("unknown")),
                    "ts": e.ts,
                    "summary": e.summary,
                })
            })
            .collect();
        to_text(&json!(out))
    }

    fn search_timeline(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("missing argument: query (string)")?;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_i64())
            .unwrap_or(100)
            .max(0) as usize;
        let hits = self
            .store
            .search_timeline(query)
            .map_err(|e| e.to_string())?;
        let out: Vec<Value> = hits
            .into_iter()
            .take(limit)
            .map(|(session, e)| {
                json!({
                    "session": session,
                    "seq": e.seq,
                    "kind": serde_json::to_value(&e.kind).unwrap_or(json!("unknown")),
                    "ts": e.ts,
                    "summary": e.summary,
                })
            })
            .collect();
        to_text(&json!(out))
    }

    fn get_hop(&self, args: &Value) -> Result<String, String> {
        let session = int_arg(args, "session").ok_or("missing argument: session (integer)")?;
        let seq = int_arg(args, "seq").ok_or("missing argument: seq (integer)")?;
        match self
            .store
            .get_hop(session, seq)
            .map_err(|e| e.to_string())?
        {
            Some(e) => {
                let mut v = serde_json::to_value(&e).map_err(|x| x.to_string())?;
                if let Ok(parsed) = serde_json::from_str::<Value>(&e.payload) {
                    v["payload"] = parsed;
                }
                to_text(&v)
            }
            None => Err(format!("no such hop: session {session} seq {seq}")),
        }
    }

    fn get_diff(&self, args: &Value) -> Result<String, String> {
        let session = int_arg(args, "session").ok_or("missing argument: session (integer)")?;
        let seq = int_arg(args, "seq").ok_or("missing argument: seq (integer)")?;
        match self
            .store
            .get_diff(session, seq)
            .map_err(|e| e.to_string())?
        {
            Some(v) => to_text(&v),
            None => Err(format!("no file_diff hop at session {session} seq {seq}")),
        }
    }

    /// One resource per session: reading it returns the session summary.
    fn resources_list(&self) -> Vec<Value> {
        match self.store.sessions() {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, started, note, hops)| {
                    let name = note.unwrap_or_else(|| format!("session {id}"));
                    json!({
                        "uri": format!("mayday://session/{id}"),
                        "name": name,
                        "description": format!("agent session started {started}, {hops} hops"),
                        "mimeType": "text/plain",
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Read a resource URI: mayday://session/{id} is the session summary,
    /// mayday://session/{id}/hops/{seq} is one hop's full record.
    fn read_resource(&self, uri: &str) -> Result<Value, String> {
        let rest = uri
            .strip_prefix("mayday://session/")
            .ok_or_else(|| format!("unknown resource: {uri}"))?;
        let (id_part, sub) = match rest.split_once('/') {
            Some((id, sub)) => (id, Some(sub)),
            None => (rest, None),
        };
        let id: i64 = id_part
            .parse()
            .map_err(|_| format!("unknown resource: {uri}"))?;
        match sub {
            None => {
                let text = self
                    .store
                    .summarize_session(id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("resource not found: {uri}"))?;
                Ok(json!({ "uri": uri, "mimeType": "text/plain", "text": text }))
            }
            Some(sub) if sub.starts_with("hops/") => {
                let seq: i64 = sub
                    .trim_start_matches("hops/")
                    .parse()
                    .map_err(|_| format!("unknown resource: {uri}"))?;
                match self
                    .store
                    .get_hop(id, seq)
                    .map_err(|e| e.to_string())?
                {
                    Some(e) => {
                        let mut v =
                            serde_json::to_value(&e).map_err(|x| x.to_string())?;
                        if let Ok(parsed) = serde_json::from_str::<Value>(&e.payload) {
                            v["payload"] = parsed;
                        }
                        let text = serde_json::to_string_pretty(&v)
                            .map_err(|x| x.to_string())?;
                        Ok(json!({
                            "uri": uri,
                            "mimeType": "application/json",
                            "text": text,
                        }))
                    }
                    None => Err(format!("resource not found: {uri}")),
                }
            }
            Some(_) => Err(format!("unknown resource: {uri}")),
        }
    }

    fn summarize_session(&self, args: &Value) -> Result<String, String> {
        let session = int_arg(args, "session").ok_or("missing argument: session (integer)")?;
        match self
            .store
            .summarize_session(session)
            .map_err(|e| e.to_string())?
        {
            Some(text) => Ok(text),
            None => Err(format!("no such session: {session}")),
        }
    }
}

fn int_arg(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn to_text(v: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(v).map_err(|e| e.to_string())
}

fn jsonrpc_ok(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn jsonrpc_error(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn tool_specs() -> Vec<Value> {
    let spec = |name: &str, desc: &str, schema: Value| {
        json!({ "name": name, "description": desc, "inputSchema": schema })
    };
    vec![
        spec(
            "list_sessions",
            "List recorded agent sessions: id, start time, note, hop count.",
            json!({ "type": "object", "properties": {} }),
        ),
        spec(
            "list_hops",
            "List one session's hops in order: seq, kind, time, summary.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "integer", "description": "session id" },
                },
                "required": ["session"],
            }),
        ),
        spec(
            "search_timeline",
            "Search every session's summaries and payloads for a case-insensitive substring.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "description": "max hits, default 100" },
                },
                "required": ["query"],
            }),
        ),
        spec(
            "get_hop",
            "Get one hop's full decrypted record: request/response bodies, tool call, or file diff.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "seq": { "type": "integer", "description": "hop number within the session" },
                },
                "required": ["session", "seq"],
            }),
        ),
        spec(
            "get_diff",
            "Get one file_diff hop's parsed payload: file, added/removed lines, content, edge flags.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                    "seq": { "type": "integer" },
                },
                "required": ["session", "seq"],
            }),
        ),
        spec(
            "summarize_session",
            "A compact summary of one session: hop counts, tools called, files touched, time span.",
            json!({
                "type": "object",
                "properties": {
                    "session": { "type": "integer" },
                },
                "required": ["session"],
            }),
        ),
    ]
}

/// Serve the protocol on stdio until the client closes its end.
pub fn run(store: Arc<Store>) -> Result<(), String> {
    use std::io::{BufRead, Write};
    let mcp = Mcp::new(store);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = mcp.handle_line(&line) {
            writeln!(out, "{resp}").map_err(|e| format!("stdout: {e}"))?;
            out.flush().map_err(|e| format!("stdout: {e}"))?;
        }
    }
    Ok(())
}
