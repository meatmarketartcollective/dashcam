use dashcam::event::EventKind;
use dashcam::mcp::Mcp;
use dashcam::store::Store;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

/// A store with one session holding the three hop kinds.
fn seeded_store() -> (tempfile::TempDir, Arc<Store>, i64) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timeline.db");
    let store = Store::open(&db, Some("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff")).unwrap();
    let _s1 = store.new_session(Some("scripted agent")).unwrap();
    store
        .append(
            s1,
            EventKind::ModelTurn,
            "POST /v1/chat/completions -> 200",
            r#"{"request_body":{"messages":[{"role":"user","content":"hi"}]},"response_body":{"choices":[]}}"#,
        )
        .unwrap();
    store
        .append(
            s1,
            EventKind::ToolCall,
            "tool_call get_weather",
            r#"{"tool":"get_weather","call_id":"c1","arguments":"{\"city\":\"Denver\"}","from_hop":1}"#,
        )
        .unwrap();
    store
        .append(
            s1,
            EventKind::FileDiff,
            "file_diff src/main.rs +2 -1 lines",
            r#"{"file":"src/main.rs","added":["a","b"],"removed":["z"],"content":"a\nb\n"}"#,
        )
        .unwrap();
    (dir, Arc::new(store), s1)
}

#[test]
fn get_hop_returns_one_decrypted_hop_or_none() {
    let (_dir, store, _s1) = seeded_store();
    let hop = store.get_hop(s1, 2).unwrap().expect("hop 2 exists");
    assert_eq!(hop.kind, EventKind::ToolCall);
    assert_eq!(hop.summary, "tool_call get_weather");
    assert!(hop.payload.contains("get_weather"));
    assert!(store.get_hop(s1, 99).unwrap().is_none());
    assert!(store.get_hop(999, 1).unwrap().is_none());
}

#[test]
fn get_diff_parses_a_file_diff_and_rejects_other_kinds() {
    let (_dir, store, _s1) = seeded_store();
    let diff = store.get_diff(s1, 3).unwrap().expect("hop 3 is a diff");
    assert_eq!(diff["file"], "src/main.rs");
    assert_eq!(diff["added"].as_array().unwrap().len(), 2);
    assert_eq!(diff["removed"].as_array().unwrap().len(), 1);
    // a model_turn hop is not a diff
    assert!(store.get_diff(s1, 1).unwrap().is_none());
}

#[test]
fn summarize_session_counts_kinds_tools_files_and_span() {
    let (_dir, store, _s1) = seeded_store();
    let text = store.summarize_session(s1).unwrap().expect("summary");
    assert!(text.contains("\"scripted agent\""), "{text}");
    assert!(text.contains("3 hops"), "{text}");
    assert!(text.contains("1 model_turn"), "{text}");
    assert!(text.contains("1 tool_call"), "{text}");
    assert!(text.contains("1 file_diff"), "{text}");
    assert!(text.contains("tools: get_weather"), "{text}");
    assert!(text.contains("src/main.rs (+2 -1)"), "{text}");
    assert!(text.contains(" to "), "{text}");
    // nonexistent session
    assert!(store.summarize_session(999).unwrap().is_none());
}

#[test]
fn summarize_session_flags_diff_edges() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some("ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100")).unwrap();
    let s = store.new_session(Some("edges")).unwrap();
    store
        .append(
            s,
            EventKind::FileDiff,
            "file_diff notes.txt (deleted)",
            r#"{"file":"notes.txt","deleted":true,"content":null}"#,
        )
        .unwrap();
    store
        .append(
            s,
            EventKind::FileDiff,
            "file_diff blob.bin (binary, 4096 bytes)",
            r#"{"file":"blob.bin","binary":true,"bytes":4096}"#,
        )
        .unwrap();
    store
        .append(
            s,
            EventKind::FileDiff,
            "file_diff new.rs (new file, +5 lines)",
            r#"{"file":"new.rs","added":["1","2","3","4","5"],"removed":[],"content":"","new_file":true}"#,
        )
        .unwrap();
    let text = store.summarize_session(s).unwrap().unwrap();
    assert!(text.contains("notes.txt (deleted)"), "{text}");
    assert!(text.contains("blob.bin (binary, 4096 bytes)"), "{text}");
    assert!(text.contains("new.rs (new file, +5)"), "{text}");
}

fn rpc(mcp: &Mcp, line: &str) -> Value {
    let resp = mcp.handle_line(line).expect("response line");
    serde_json::from_str(&resp).expect("response parses as JSON")
}

#[test]
fn mcp_initialize_and_tools_list() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    let init = rpc(
        &mcp,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    );
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["serverInfo"]["name"], "dashcam");
    // the initialized notification owes no response
    assert!(mcp
        .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .is_none());
    let tools = rpc(&mcp, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "list_sessions",
            "list_hops",
            "search_timeline",
            "get_hop",
            "get_diff",
            "summarize_session",
        ]
    );
    // unknown method is a protocol error
    let unknown = rpc(&mcp, r#"{"jsonrpc":"2.0","id":3,"method":"prompts/list"}"#);
    assert_eq!(unknown["error"]["code"], -32601);
}

fn call_tool(mcp: &Mcp, id: i64, name: &str, args: Value) -> Value {
    rpc(
        mcp,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        })
        .to_string(),
    )
}

#[test]
fn mcp_tool_calls_dispatch_to_the_store() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);

    let sessions = call_tool(&mcp, 10, "list_sessions", json!({}));
    assert!(sessions["error"].is_null());
    let text = sessions["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("scripted agent"), "{text}");

    let hops = call_tool(&mcp, 11, "list_hops", json!({ "session": s1 }));
    let text = hops["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 3);
    assert_eq!(parsed[0]["seq"], 1);
    assert_eq!(parsed[1]["kind"], "tool_call");

    let hop = call_tool(&mcp, 12, "get_hop", json!({ "session": s1, "seq": 2 }));
    let text = hop["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["kind"], "tool_call");
    assert_eq!(parsed["payload"]["tool"], "get_weather");

    let diff = call_tool(&mcp, 13, "get_diff", json!({ "session": s1, "seq": 3 }));
    let text = diff["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["file"], "src/main.rs");

    let search = call_tool(&mcp, 14, "search_timeline", json!({ "query": "get_weather" }));
    let text = search["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("tool_call"), "{text}");

    let summary = call_tool(&mcp, 15, "summarize_session", json!({ "session": s1 }));
    let text = summary["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("3 hops"), "{text}");
}

#[test]
fn mcp_tool_errors_are_reported_not_crashes() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);

    // missing arguments
    for (id, name, args) in [
        (20, "list_hops", json!({})),
        (21, "get_hop", json!({ "session": s1 })),
        (22, "search_timeline", json!({})),
        (23, "summarize_session", json!({})),
    ] {
        let resp = call_tool(&mcp, id, name, args);
        assert_eq!(resp["result"]["isError"], true, "{name}");
    }
    // unknown tool
    let resp = call_tool(&mcp, 24, "delete_everything", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("unknown tool"));
    // missing hop and missing session are clean errors
    let resp = call_tool(&mcp, 25, "get_hop", json!({ "session": s1, "seq": 42 }));
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no such hop"));
    let resp = call_tool(&mcp, 26, "summarize_session", json!({ "session": 424242 }));
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no such session"));
    // unparseable line is a parse error
    let resp = rpc_parse_err(&mcp);
    assert_eq!(resp["error"]["code"], -32700);
}

fn rpc_parse_err(mcp: &Mcp) -> Value {
    let resp = mcp.handle_line("not json at all").unwrap();
    serde_json::from_str(&resp).unwrap()
}

/// The real binary end to end: an MCP client speaks JSON-RPC lines on stdio
/// and the scan tools answer from an encrypted on-disk timeline.
#[test]
fn mcp_stdio_binary_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timeline.db");
    // seed the encrypted db and its key file exactly as dashcam leaves them
    let store = Store::open(&db, Some("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899")).unwrap();
    let _s1 = store.new_session(Some("mcp e2e")).unwrap();
    store
        .append(s1, EventKind::ModelTurn, "turn one", r#"{"request_body":{}}"#)
        .unwrap();
    store
        .append(
            s1,
            EventKind::ToolCall,
            "tool_call list_files",
            r#"{"tool":"list_files","call_id":"c9","arguments":"{}","from_hop":1}"#,
        )
        .unwrap();
    drop(store);
    // the binary loads its key from <db dir>/key, exactly as dashcam leaves it
    std::fs::write(
        dir.path().join("key"),
        "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_dashcam"))
        .args(["--db", db.to_str().unwrap(), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dashcam mcp");

    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"summarize_session","arguments":{{"session":{s1}}}}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"get_diff","arguments":{{"session":{s1},"seq":1}}}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let lines: Vec<String> = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .collect();
    // exactly three responses: the notification owed none
    assert_eq!(lines.len(), 3, "{lines:?}");
    let r1: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(r1["result"]["serverInfo"]["name"], "dashcam");
    let r2: Value = serde_json::from_str(&lines[1]).unwrap();
    let text = r2["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("mcp e2e") && text.contains("2 hops"), "{text}");
    let r3: Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(r3["result"]["isError"], true, "hop 1 is not a file_diff");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn resources_list_names_every_session() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    let resp = rpc(&mcp, r#"{"jsonrpc":"2.0","id":30,"method":"resources/list"}"#);
    let resources = resp["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0]["uri"],
        format!("dashcam://session/{s1}")
    );
    assert_eq!(resources[0]["name"], "scripted agent");
    assert!(resources[0]["description"]
        .as_str()
        .unwrap()
        .contains("3 hops"));
}

#[test]
fn reading_a_session_resource_returns_the_summary() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    let line = json!({
        "jsonrpc": "2.0",
        "id": 31,
        "method": "resources/read",
        "params": { "uri": format!("dashcam://session/{s1}") },
    })
    .to_string();
    let resp = rpc(&mcp, &line);
    let contents = resp["result"]["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["mimeType"], "text/plain");
    let text = contents[0]["text"].as_str().unwrap();
    assert!(text.contains("3 hops") && text.contains("get_weather"), "{text}");
}

#[test]
fn reading_a_hop_resource_returns_its_full_record() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    let line = json!({
        "jsonrpc": "2.0",
        "id": 32,
        "method": "resources/read",
        "params": { "uri": format!("dashcam://session/{s1}/hops/2") },
    })
    .to_string();
    let resp = rpc(&mcp, &line);
    let contents = resp["result"]["contents"].as_array().unwrap();
    assert_eq!(contents[0]["mimeType"], "application/json");
    let parsed: Value = serde_json::from_str(contents[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["kind"], "tool_call");
    assert_eq!(parsed["payload"]["tool"], "get_weather");
}

#[test]
fn unknown_or_malformed_resource_uris_are_rejected() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    for (id, uri) in [
        (40, "dashcam://other/thing"),
        (41, "dashcam://session/999"),
        (42, "dashcam://session/notanumber"),
        (43, "dashcam://session/1/branch/2"),
        (44, "dashcam://session/1/hops/notanumber"),
    ] {
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "resources/read",
            "params": { "uri": uri },
        })
        .to_string();
        let resp = rpc(&mcp, &line);
        assert_eq!(resp["error"]["code"], -32002, "{uri}");
    }
}

#[test]
fn notifications_owe_no_response() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    assert!(mcp
        .handle_line(r#"{"jsonrpc":"2.0","method":"ping"}"#)
        .is_none());
    assert!(mcp
        .handle_line(r#"{"jsonrpc":"2.0","method":"anything/unknown"}"#)
        .is_none());
    // a parse error has no id to honor, and still answers
    let resp = rpc_parse_err(&mcp);
    assert_eq!(resp["error"]["code"], -32700);
}

#[test]
fn malformed_requests_get_clean_errors_not_crashes() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = Mcp::new(store);
    let big = "x".repeat(1_000_000);
    let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
    let cases = vec![
        // method not a string
        r#"{"jsonrpc":"2.0","id":60,"method":123}"#.to_string(),
        // tools/call with no params at all
        r#"{"jsonrpc":"2.0","id":61,"method":"tools/call"}"#.to_string(),
        // arguments of the wrong shape
        r#"{"jsonrpc":"2.0","id":62,"method":"tools/call","params":{"name":"list_hops","arguments":"nope"}}"#.to_string(),
        // wrong-typed tool arguments
        r#"{"jsonrpc":"2.0","id":63,"method":"tools/call","params":{"name":"get_hop","arguments":{"session":"one","seq":1}}}"#.to_string(),
        // deeply nested json exceeds the parser's depth limit
        format!(r#"{{"jsonrpc":"2.0","id":64,"method":"ping","params":{{"a":{deep}}}}}"#),
        // resources/read on a garbage uri
        r#"{"jsonrpc":"2.0","id":66,"method":"resources/read","params":{"uri":"http://x"}}"#.to_string(),
    ];
    for (i, line) in cases.iter().enumerate() {
        let resp = rpc(&mcp, line);
        let has_error = resp.get("error").is_some();
        let is_tool_error = resp["result"]["isError"] == json!(true);
        assert!(
            has_error || is_tool_error,
            "case {i} got neither error nor isError: {resp}"
        );
    }
    // a one-megabyte argument value is answered normally, not choked on
    let huge = format!(
        r#"{{"jsonrpc":"2.0","id":65,"method":"tools/call","params":{{"name":"search_timeline","arguments":{{"session":{s1},"query":"{big}"}}}}}}"#
    );
    let resp = rpc(&mcp, &huge);
    assert!(resp["result"]["content"][0]["text"].is_string(), "{resp}");
}

#[test]
fn concurrent_readers_and_a_writer_do_not_deadlock() {
    let (_dir, store, _s1) = seeded_store();
    let mcp = std::sync::Arc::new(Mcp::new(store.clone()));

    // one writer keeps recording while readers hammer every surface
    let writer_store = store.clone();
    let writer = std::thread::spawn(move || {
        for i in 0..60 {
            writer_store
                .append(s1, EventKind::ToolCall, &format!("stress {i}"), r#"{"tool":"probe","arguments":"{}"}"#)
                .unwrap();
        }
    });

    let readers: Vec<_> = (0..4)
        .map(|r| {
            let mcp = mcp.clone();
            std::thread::spawn(move || {
                for i in 0..40 {
                    for line in [
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string(),
                        r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#.to_string(),
                        format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"search_timeline","arguments":{{"session":{s1},"query":"stress"}}}}}}"#),
                        format!(r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"summarize_session","arguments":{{"session":{s1}}}}}}}"#),
                        format!(r#"{{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{{"uri":"dashcam://session/{s1}/hops/1"}}}}"#),
                    ] {
                        let resp = mcp.handle_line(&line).expect("readers get responses");
                        let v: Value = serde_json::from_str(&resp).unwrap();
                        assert!(v.get("result").is_some() || v.get("error").is_some(), "reader {r} iter {i}: {resp}");
                    }
                }
            })
        })
        .collect();

    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
    let n: usize = {
        let events = store.events(s1).unwrap();
        events.len()
    };
    assert!(n >= 60, "writer events missing: {n}");
}

#[test]
fn invalid_utf8_on_stdio_does_not_kill_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("timeline.db");
    let store = Store::open(&db, Some("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899")).unwrap();
    drop(store);
    std::fs::write(dir.path().join("key"), "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_dashcam"))
        .args(["--db", db.to_str().unwrap(), "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn dashcam mcp");

    use std::io::Write;
    let mut stdin = child.stdin.take().unwrap();
    // garbage bytes that are not valid utf-8, then a perfectly good request
    stdin.write_all(&[0xff, 0xfe, b'\n']).unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#).unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let lines: Vec<String> = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    let r1: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(r1["error"]["code"], -32700, "the bad bytes owe a parse error");
    let r2: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(r2["result"]["serverInfo"]["name"], "dashcam", "the server kept serving");

    let _ = child.kill();
    let _ = child.wait();
}
