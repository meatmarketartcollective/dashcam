use dashcam::replay::Replay;
use dashcam::store::Store;

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn session_with_hops(n: usize) -> (tempfile::TempDir, Store, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("t.db"), Some(K)).unwrap();
    let s = store.new_session(Some("replay test")).unwrap();
    for i in 0..n {
        store
            .append(s, dashcam::event::EventKind::ModelTurn, &format!("hop {i}"), &format!("{{\"n\":{i}}}"))
            .unwrap();
    }
    (dir, store, s)
}

#[test]
fn walk_forward_back_and_bounds() {
    let (_d, store, s) = session_with_hops(3);
    let events = store.events(s).unwrap();
    let mut r = Replay::new(events);
    assert_eq!(r.len(), 3);
    assert!(r.current().summary.contains("hop 0"));
    assert!(r.next());
    assert!(r.next());
    assert!(!r.next(), "next past the last hop must not move");
    assert!(r.current().summary.contains("hop 2"));
    assert!(r.back());
    assert!(r.current().summary.contains("hop 1"));
    r.first();
    assert!(!r.back(), "back past the first hop must not move");
    r.last();
    assert!(r.current().summary.contains("hop 2"));
}

#[test]
fn render_shows_state_up_to_the_hop() {
    let (_d, store, s) = session_with_hops(3);
    let events = store.events(s).unwrap();
    let mut r = Replay::new(events);
    r.next();
    let out = r.render_current();
    assert!(out.contains("hop 2 of 3"));
    assert!(out.contains("\"n\": 1"), "payload must render pretty");
    // state lists every summary up to and including the current hop only
    assert!(out.contains("hop 0"));
    assert!(out.contains("hop 1"));
    assert!(!out.contains("hop 2\n"), "future hops must not leak into state");
}

#[test]
fn empty_session_is_handled() {
    let (_d, store, s) = session_with_hops(0);
    let events = store.events(s).unwrap();
    let mut r = Replay::new(events);
    assert_eq!(r.len(), 0);
    assert!(!r.next());
    assert!(!r.back());
}
