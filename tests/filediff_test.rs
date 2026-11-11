use dashcam::filediff;
use dashcam::store::Store;
use serde_json::Value;
use std::fs;

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn store() -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K)).unwrap();
    let session = store.new_session(Some("filediff test")).unwrap();
    (dir, store, session)
}

fn payload_of(store: &Store, session: i64, idx: usize) -> Value {
    let events = store.events(session).unwrap();
    serde_json::from_str(&events[idx].payload).unwrap()
}

#[test]
fn new_edit_unchanged_round_trip() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.py");

    fs::write(&path, "line1\nline2\n").unwrap();
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(out.changed);
    assert!(out.summary.contains("(new file"));
    let p = payload_of(&store, s, 0);
    assert_eq!(p["new_file"], true);
    assert_eq!(p["added"], serde_json::json!(["line1", "line2"]));

    fs::write(&path, "line1\nline2 changed\nline3\n").unwrap();
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(out.changed);
    let p = payload_of(&store, s, 1);
    assert_eq!(p["added"], serde_json::json!(["line2 changed", "line3"]));
    assert_eq!(p["removed"], serde_json::json!(["line2"]));

    // third call: content identical, no new hop
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(!out.changed);
    assert_eq!(store.events(s).unwrap().len(), 2);
}

#[test]
fn deletion_records_removed_lines_and_blocks_rerecord() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    filediff::record(&store, s, &path).unwrap();

    fs::remove_file(&path).unwrap();
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(out.changed);
    assert!(out.summary.contains("(deleted)"));
    let p = payload_of(&store, s, 1);
    assert_eq!(p["deleted"], true);
    assert_eq!(p["content"], Value::Null);
    assert_eq!(p["removed"], serde_json::json!(["alpha", "beta"]));

    // recording the still-missing file again is an error, not a fake hop
    assert!(filediff::record(&store, s, &path).is_err());
}

#[test]
fn binary_files_record_size_only() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.bin");
    fs::write(&path, b"PK\x03\x04\x00\x00binary-ish\x00bytes").unwrap();
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(out.changed);
    assert!(out.summary.contains("(binary"));
    let p = payload_of(&store, s, 0);
    assert_eq!(p["binary"], true);
    assert!(p.get("content").is_none());
    assert_eq!(p["bytes"], 22);
}

#[test]
fn secrets_in_file_content_are_masked() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("env.txt");
    fs::write(&path, "key=sk-abcdef0123456789\n").unwrap();
    filediff::record(&store, s, &path).unwrap();
    let raw = store.events(s).unwrap()[0].payload.clone();
    assert!(!raw.contains("sk-abcdef0123456789"));
    assert!(raw.contains("sk-[redacted]"));

    let bytes = fs::read(&_d.path().join("t.db")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("abcdef0123456789"));
}

#[test]
fn non_utf8_text_does_not_panic() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weird.txt");
    fs::write(&path, [0x68, 0x69, 0xFF, 0xFE, 0x0A]).unwrap(); // invalid UTF-8, no NUL
    let out = filediff::record(&store, s, &path).unwrap();
    assert!(out.changed);
    let p = payload_of(&store, s, 0);
    assert!(p["content"].as_str().unwrap().starts_with("hi"));
}

#[test]
fn oversized_content_is_truncated_with_a_flag() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.txt");
    let big = "x".repeat(80 * 1024) + "\n";
    fs::write(&path, &big).unwrap();
    filediff::record(&store, s, &path).unwrap();
    let p = payload_of(&store, s, 0);
    assert_eq!(p["content_truncated"], true);
    assert!(p["content"].as_str().unwrap().len() < big.len());
    assert_eq!(p["added"].as_array().unwrap().len(), 1);
}

#[test]
fn sessions_do_not_share_file_baselines() {
    let (_d, store, _s) = store();
    let s2 = store.new_session(Some("second session")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    fs::write(&path, "one\n").unwrap();
    filediff::record(&store, _s, &path).unwrap();
    // a fresh session has no baseline for the same file: still "new file"
    let out = filediff::record(&store, s2, &path).unwrap();
    assert!(out.summary.contains("(new file"));
    let ev2 = store.events(s2).unwrap();
    assert_eq!(ev2.len(), 1);
}

#[test]
fn interleaved_edit_keeps_common_lines() {
    let (_d, store, s) = store();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("code.rs");
    fs::write(&path, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    filediff::record(&store, s, &path).unwrap();
    fs::write(&path, "fn a() {}\nfn b_new() {}\nfn c() {}\n").unwrap();
    filediff::record(&store, s, &path).unwrap();
    let p = payload_of(&store, s, 1);
    assert_eq!(p["added"], serde_json::json!(["fn b_new() {}"]));
    assert_eq!(p["removed"], serde_json::json!(["fn b() {}"]));
}
