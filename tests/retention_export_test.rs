use dashcam::event::EventKind;
use dashcam::store::{event_jsonl, RetentionPolicy, Store};

fn plaintext_store(dir: &std::path::Path) -> Store {
    Store::open(&dir.join("t.db"), None).unwrap()
}

#[test]
fn size_cap_prunes_oldest_until_under_cap_and_exports_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = plaintext_store(dir.path());
    let s = store.new_session(Some("size")).unwrap();
    // payloads of distinct lengths; plaintext store keeps lengths exact
    for (n, len) in [(1, 10usize), (2, 100), (3, 100), (4, 100)] {
        store
            .append(s, EventKind::ModelTurn, "hop", &"x".repeat(len))
            .unwrap();
        let _ = n;
    }
    // stored bytes: 4 summaries of 3 + payloads 10+100+100+100 = 322
    // cap 206 means at least 116 bytes must go: hops 1 and 2 cover that
    let export_path = dir.path().join("aged-out.jsonl");
    let policy = RetentionPolicy {
        max_size_bytes: Some(206),
        export_path: Some(export_path.clone()),
        ..Default::default()
    };
    let (removed, _sessions) = store.prune(&policy).unwrap();
    assert_eq!(removed, 2, "oldest two hops go first");

    let remaining = store.events(s).unwrap();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].payload, "x".repeat(100));
    assert_eq!(remaining[1].payload, "x".repeat(100));

    // the aged-out hops landed as readable JSONL, oldest first
    let text = std::fs::read_to_string(&export_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["seq"], 1);
    assert_eq!(first["kind"], "model_turn");
    assert_eq!(first["payload"].as_str().unwrap(), "x".repeat(10));
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["seq"], 2);
}

#[test]
fn count_and_size_rules_export_each_aged_out_hop_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = plaintext_store(dir.path());
    let s = store.new_session(Some("dedup")).unwrap();
    for (n, len) in [(1, 10usize), (2, 10), (3, 10), (4, 10)] {
        store
            .append(s, EventKind::ModelTurn, "hop", &"x".repeat(len))
            .unwrap();
        let _ = n;
    }
    let export_path = dir.path().join("aged-out.jsonl");
    // count rule removes hops 1-2; size rule removes 1-3; union is 1-3 once
    let policy = RetentionPolicy {
        max_events: Some(2),
        max_size_bytes: Some(13),
        export_path: Some(export_path.clone()),
        ..Default::default()
    };
    let (removed, _sessions) = store.prune(&policy).unwrap();
    assert_eq!(removed, 3, "{removed}");
    let text = std::fs::read_to_string(&export_path).unwrap();
    let seqs: Vec<i64> = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3], "no duplicates in the export");
}

#[test]
fn no_export_file_when_nothing_ages_out() {
    let dir = tempfile::tempdir().unwrap();
    let store = plaintext_store(dir.path());
    let export_path = dir.path().join("should-not-exist.jsonl");
    let policy = RetentionPolicy {
        export_path: Some(export_path.clone()),
        ..Default::default()
    };
    store.prune(&policy).unwrap();
    assert!(!export_path.exists());
}

#[test]
fn export_session_roundtrips_decrypted_and_embeds_payload() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some("11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff")).unwrap();
    let s = store.new_session(Some("roundtrip")).unwrap();
    store
        .append(s, EventKind::Prompt, "user prompt", r#"{"text":"hello there"}"#)
        .unwrap();
    store
        .append(
            s,
            EventKind::ToolCall,
            "tool_call read_file",
            r#"{"tool":"read_file","call_id":"c1","arguments":"{}","from_hop":1}"#,
        )
        .unwrap();

    let mut buf: Vec<u8> = Vec::new();
    let n = store.export_session(s, &mut buf).unwrap();
    assert_eq!(n, 2);
    let text = String::from_utf8(buf).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["kind"], "prompt");
    assert_eq!(lines[0]["payload"]["text"], "hello there");
    assert_eq!(lines[1]["kind"], "tool_call");
    assert_eq!(lines[1]["payload"]["tool"], "read_file");
    assert_eq!(lines[0]["session"], s);

    // nonexistent session is an error, not an empty file
    let mut sink: Vec<u8> = Vec::new();
    assert!(store.export_session(999, &mut sink).is_err());
}

#[test]
fn event_jsonl_keeps_unparseable_payloads_as_strings() {
    let e = dashcam::event::Event {
        id: 1,
        session_id: 1,
        seq: 1,
        ts: "2026-11-10 09:00:00".into(),
        kind: EventKind::FileDiff,
        summary: "file_diff a.rs".into(),
        payload: "not json".into(),
    };
    let v: serde_json::Value = serde_json::from_str(&event_jsonl(&e)).unwrap();
    assert_eq!(v["payload"], "not json");
    assert_eq!(v["kind"], "file_diff");
}
