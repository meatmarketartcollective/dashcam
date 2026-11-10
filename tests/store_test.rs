use mayday::event::EventKind;
use mayday::store::Store;

#[test]
fn session_and_events_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db).unwrap();

    let s = store.new_session(Some("openclaw run 42")).unwrap();
    assert!(s >= 1);

    store.append(s, EventKind::Prompt, "user asked to deploy", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "ran git push --force", "{\"cmd\":\"git push --force\"}").unwrap();
    store.append(s, EventKind::ModelTurn, "model decided to force-push", "{}").unwrap();
    store.append(s, EventKind::FileDiff, "rewrote deploy.sh", "{}").unwrap();

    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].3, 4);

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
    let store = Store::open(&dir.path().join("t.db")).unwrap();
    assert!(store.sessions().unwrap().is_empty());
    assert!(store.events(1).unwrap().is_empty());
}
