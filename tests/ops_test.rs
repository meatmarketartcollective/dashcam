use dashcam::compare::diff_tool_sequences;
use dashcam::event::EventKind;
use dashcam::redact::Redactor;
use dashcam::store::Store;
use dashcam::watch;

const KEY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const KEY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn store(key: Option<&str>) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.db");
    (dir, Store::open(&path, key).unwrap())
}

#[test]
fn events_since_returns_only_newer_hops() {
    let (_dir, store) = store(None);
    let s = store.new_session(None).unwrap();
    store.append(s, EventKind::ModelTurn, "one", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "two", "{}").unwrap();
    store.append(s, EventKind::Prompt, "three", "{}").unwrap();

    let since_1 = store.events_since(s, 1).unwrap();
    assert_eq!(since_1.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
    assert!(store.events_since(s, 3).unwrap().is_empty());
    assert_eq!(store.events_since(s, 0).unwrap().len(), 3);
}

#[test]
fn verify_passes_with_the_right_key_and_fails_with_the_wrong_one() {
    let (dir, store) = store(Some(KEY_A));
    let s = store.new_session(Some("run")).unwrap();
    store.append(s, EventKind::ModelTurn, "hello", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "tool_call get_weather", "{}").unwrap();

    let good = store.verify().unwrap();
    assert!(good.encrypted);
    assert_eq!(good.checked, 2);
    assert!(good.failed.is_empty());

    let wrong = Store::open(&dir.path().join("timeline.db"), Some(KEY_B)).unwrap();
    let bad = wrong.verify().unwrap();
    assert!(bad.encrypted);
    assert_eq!(bad.checked, 2);
    // Both encrypted fields of both hops miss.
    assert_eq!(bad.failed.len(), 4);
    assert!(bad.failed.iter().all(|f| f.session_id == s));
    assert!(bad
        .failed
        .iter()
        .any(|f| f.field == "summary" && f.seq == 2));
}

#[test]
fn verify_on_a_plaintext_store_reports_nothing_to_check() {
    let (_dir, store) = store(None);
    let s = store.new_session(None).unwrap();
    store.append(s, EventKind::ModelTurn, "hello", "{}").unwrap();
    let report = store.verify().unwrap();
    assert!(!report.encrypted);
    assert!(report.failed.is_empty());
}

#[test]
fn audit_finds_unscrubbed_secrets_and_skips_clean_rows() {
    let (_dir, store) = store(None);
    let s = store.new_session(None).unwrap();
    // Appends bypass the redactor: that is exactly the hole audit is for.
    store
        .append(s, EventKind::ModelTurn, "leaky", "using key sk-abcdef12345678")
        .unwrap();
    store
        .append(s, EventKind::ToolCall, "clean", "no secrets here")
        .unwrap();

    let report = store.audit(&Redactor::builtin()).unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.undecryptable, 0);
    assert_eq!(report.leaks.len(), 1);
    assert_eq!(report.leaks[0].session_id, s);
    assert_eq!(report.leaks[0].field, "payload");
    // The preview never carries the secret: scrubbed, so only the prefix.
    assert!(report.leaks[0].preview.starts_with("using key sk-[redacted]"));
    assert!(!report.leaks[0].preview.contains("abcdef12345678"));
}

#[test]
fn audit_scans_encrypted_rows_through_the_key() {
    let (_dir, store) = store(Some(KEY_A));
    let s = store.new_session(None).unwrap();
    store
        .append(s, EventKind::ModelTurn, "hello", "Bearer abcdefghijklmnop")
        .unwrap();
    let report = store.audit(&Redactor::builtin()).unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.leaks.len(), 1);
    assert_eq!(report.leaks[0].field, "payload");
}

#[test]
fn audit_counts_undecryptable_rows_instead_of_flagging_them() {
    let (dir, store) = store(Some(KEY_A));
    let s = store.new_session(None).unwrap();
    store
        .append(s, EventKind::ModelTurn, "hello", "sk-abcdef12345678")
        .unwrap();
    let wrong = Store::open(&dir.path().join("timeline.db"), Some(KEY_B)).unwrap();
    let report = wrong.audit(&Redactor::builtin()).unwrap();
    assert_eq!(report.scanned, 1);
    // Summary + payload both failed to decrypt; neither counted as a leak.
    assert_eq!(report.undecryptable, 2);
    assert!(report.leaks.is_empty());
}

#[test]
fn audit_picks_up_extra_rules_from_a_file() {
    let (_dir, store) = store(None);
    let s = store.new_session(None).unwrap();
    store
        .append(s, EventKind::ModelTurn, "hello", "code 1234-5678-9012")
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let rules = dir.path().join("rules");
    std::fs::write(&rules, "1234-5678-9012\n").unwrap();
    let report = store.audit(&Redactor::load(&rules)).unwrap();
    assert_eq!(report.leaks.len(), 1);
}

#[test]
fn tool_call_names_reads_payload_tool_and_falls_back_to_summary() {
    let (_dir, store) = store(None);
    let s = store.new_session(None).unwrap();
    store
        .append(s, EventKind::ToolCall, "tool_call get_weather", r#"{"tool":"get_weather"}"#)
        .unwrap();
    store
        .append(s, EventKind::ModelTurn, "not a tool call", "{}")
        .unwrap();
    store
        .append(s, EventKind::ToolCall, "tool_call custom_fn", "not json")
        .unwrap();

    let names = store.tool_call_names(s).unwrap();
    assert_eq!(names, vec![(1, "get_weather".into()), (3, "custom_fn".into())]);
}

#[test]
fn diff_spots_identical_runs_and_the_first_divergence() {
    let same = vec!["read".to_string(), "write".to_string()];
    let d = diff_tool_sequences(&same, &same);
    assert_eq!(d.common, 2);
    assert_eq!(d.first_divergence, None);
    assert!(d.only_in_a.is_empty() && d.only_in_b.is_empty());

    let a = vec!["read".to_string(), "write".to_string(), "read".to_string()];
    let b = vec!["read".to_string(), "read".to_string()];
    let d = diff_tool_sequences(&a, &b);
    assert_eq!(d.common, 2);
    assert_eq!(d.first_divergence, Some((1, 1)));
    assert_eq!(d.only_in_a, vec![(1, "write".to_string())]);
    assert!(d.only_in_b.is_empty());
}

#[test]
fn diff_handles_empty_sides() {
    let d = diff_tool_sequences(&[], &["only_b".to_string()]);
    assert_eq!(d.common, 0);
    assert_eq!(d.first_divergence, Some((0, 0)));
    assert_eq!(d.only_in_b, vec![(0, "only_b".to_string())]);
}

#[test]
fn watch_poll_and_render_shape() {
    let (_dir, store) = store(None);
    let s = store.new_session(Some("live")).unwrap();
    store.append(s, EventKind::ModelTurn, "first", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "tool_call ls", "{}").unwrap();

    let fresh = watch::poll_once(&store, s, 1).unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].seq, 2);
    assert_eq!(fresh[0].summary, "tool_call ls");
    let line = watch::render_line(&fresh[0]);
    assert!(line.contains("tool_call ls"));
    assert!(line.contains("ToolCall"));
    assert!(watch::poll_once(&store, s, 2).unwrap().is_empty());
}
