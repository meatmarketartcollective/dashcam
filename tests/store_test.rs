use dashcam::event::EventKind;
use dashcam::store::{RetentionPolicy, Store};

const K1: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const K2: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

#[test]
fn session_and_events_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, Some(K1)).unwrap();

    let s = store.new_session(Some("openclaw run 42")).unwrap();
    assert!(s >= 1);

    store.append(s, EventKind::Prompt, "user asked to deploy", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "ran git push --force", "{\"cmd\":\"git push --force\"}").unwrap();
    store.append(s, EventKind::ModelTurn, "model decided to force-push", "{}").unwrap();
    store.append(s, EventKind::FileDiff, "rewrote deploy.sh", "{}").unwrap();

    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].3, 4);
    assert_eq!(sessions[0].2.as_deref(), Some("openclaw run 42"));

    let events = store.events(s).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[3].seq, 4);
    assert_eq!(events[1].kind, EventKind::ToolCall);
    assert!(events[1].payload.contains("force"));
}

#[test]
fn empty_db_lists_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    assert!(store.sessions().unwrap().is_empty());
    assert!(store.events(1).unwrap().is_empty());
}

#[test]
fn wrong_key_yields_no_readables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    {
        let store = Store::open(&db, Some(K1)).unwrap();
        let s = store.new_session(Some("flight label")).unwrap();
        store.append(s, EventKind::Prompt, "secret summary", "secret payload").unwrap();
    }
    let store = Store::open(&db, Some(K2)).unwrap();
    let sessions = store.sessions().unwrap();
    let note = sessions[0].2.clone().unwrap_or_default();
    assert!(!note.contains("flight label"));
    let events = store.events(1).unwrap();
    assert!(!events[0].payload.contains("secret payload"));
    assert!(!events[0].summary.contains("secret summary"));
}

#[test]
fn prune_keeps_the_newest_events_and_drops_emptied_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();

    let a = store.new_session(Some("old run")).unwrap();
    for i in 1..=3 {
        store.append(a, EventKind::Prompt, &format!("old hop {i}"), "{}").unwrap();
    }
    let b = store.new_session(Some("new run")).unwrap();
    for i in 1..=2 {
        store.append(b, EventKind::Prompt, &format!("new hop {i}"), "{}").unwrap();
    }

    let (events_removed, sessions_removed) =
        store.prune(&RetentionPolicy { max_events: Some(2), max_age_days: None, ..Default::default() }).unwrap();
    assert_eq!(events_removed, 3);
    assert_eq!(sessions_removed, 1);

    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].2.as_deref(), Some("new run"));
    let events = store.events(sessions[0].0).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.summary.contains("new hop")));
}

#[test]
fn prune_with_no_limit_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    let s = store.new_session(Some("run")).unwrap();
    store.append(s, EventKind::Prompt, "hop", "{}").unwrap();
    let (events_removed, sessions_removed) =
        store.prune(&RetentionPolicy { max_events: None, max_age_days: None, ..Default::default() }).unwrap();
    assert_eq!((events_removed, sessions_removed), (0, 0));
    assert_eq!(store.events(s).unwrap().len(), 1);
}

#[test]
fn file_on_disk_holds_no_readables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, Some(K1)).unwrap();
    let s = store.new_session(Some("label on disk test")).unwrap();
    store.append(s, EventKind::ToolCall, "cmd summary", "payload body text").unwrap();
    let bytes = std::fs::read(&db).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("payload body text"));
    assert!(!raw.contains("cmd summary"));
    assert!(!raw.contains("label on disk test"));
}
