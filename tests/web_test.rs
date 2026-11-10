use mayday::event::EventKind;
use mayday::store::Store;
use mayday::web;

const K1: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

#[test]
fn index_lists_sessions_and_session_page_shows_hops() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    let s = store.new_session(Some("openclaw run 42")).unwrap();
    store.append(s, EventKind::ToolCall, "ran git push --force", "{\"cmd\":\"git push --force\"}").unwrap();
    store.append(s, EventKind::ModelTurn, "model decided to force-push", "{}").unwrap();

    let index = web::render_index(&store);
    assert!(index.contains("/session/"));
    assert!(index.contains("openclaw run 42"));
    assert!(!index.contains("<script"));

    let page = web::render_session(&store, s);
    assert!(page.contains("git push --force"));
    assert!(page.contains("model decided to force-push"));
    // payload markup is escaped, not executed
    assert!(page.contains("&quot;cmd&quot;"));

    let empty = web::render_session(&store, 9999);
    assert!(empty.contains("no hops recorded"));
}

#[test]
fn hostile_payload_is_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    let s = store.new_session(Some("hostile")).unwrap();
    store.append(s, EventKind::Prompt, "<script>alert(1)</script>", "<img src=x onerror=alert(2)>").unwrap();
    let page = web::render_session(&store, s);
    assert!(!page.contains("<script>alert(1)"));
    assert!(!page.contains("<img src=x"));
    assert!(page.contains("&lt;script&gt;"));
}
