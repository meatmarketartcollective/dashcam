use dashcam::event::EventKind;
use dashcam::store::Store;
use dashcam::web;

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
    // each hop links to its step-through page
    assert!(page.contains("/session/"));
    assert!(page.contains("/hop/"));

    let seqs: Vec<i64> = store.events(s).unwrap().iter().map(|e| e.seq).collect();
    let hop = web::render_hop(&store, s, seqs[1]);
    assert!(hop.contains("git push --force")); // state at this hop includes hop 1
    assert!(hop.contains("model decided to force-push"));
    assert!(hop.contains("next &rarr;"));
    assert!(!hop.contains("hop 3"));
    // payload markup is escaped, not executed (payloads live on the step page)
    let hop1 = web::render_hop(&store, s, seqs[0]);
    assert!(hop1.contains("&quot;cmd&quot;"));
    let last = web::render_hop(&store, s, seqs[1]);
    assert!(last.contains("next &rarr;"));

    let empty = web::render_session(&store, 9999);
    assert!(empty.contains("no hops recorded"));
}

#[test]
fn step_through_nav_and_state_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    let s = store.new_session(Some("nav")).unwrap();
    store.append(s, EventKind::Prompt, "step one", "{}").unwrap();
    store.append(s, EventKind::ToolCall, "step two", "{}").unwrap();
    store.append(s, EventKind::ModelTurn, "step three", "{}").unwrap();
    let seqs: Vec<i64> = store.events(s).unwrap().iter().map(|e| e.seq).collect();

    let mid = web::render_hop(&store, s, seqs[1]);
    assert!(mid.contains("&laquo; first"));
    assert!(mid.contains("next &rarr;"));
    assert!(mid.contains("state at this hop"));
    // state at hop 2 holds steps 1 and 2, not 3
    assert!(mid.contains("step one"));
    assert!(mid.contains("step two"));
    assert!(!mid.contains("step three"));

    let first = web::render_hop(&store, s, seqs[0]);
    assert!(first.contains("next &rarr;"));
    assert!(!first.contains("&larr; prev</a>"));

    let last = web::render_hop(&store, s, seqs[2]);
    assert!(!last.contains("next &rarr;</a>"));
    assert!(last.contains("last &raquo;"));

    let missing = web::render_hop(&store, s, 99);
    assert!(missing.contains("not found"));
}

#[test]
fn search_finds_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K1)).unwrap();
    let s1 = store.new_session(Some("run a")).unwrap();
    store.append(s1, EventKind::ToolCall, "ran git push --force", "{}").unwrap();
    let s2 = store.new_session(Some("run b")).unwrap();
    store.append(s2, EventKind::ModelTurn, "model chose force-push", "{}").unwrap();

    let page = web::render_search(&store, "force");
    assert!(page.contains("session 1"));
    assert!(page.contains("session 2"));
    assert!(page.contains("ran git push --force"));

    let none = web::render_search(&store, "zzz-nothing");
    assert!(none.contains("no hops matched"));
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
