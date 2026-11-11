use dashcam::event::EventKind;
use dashcam::store::Store;
use std::sync::Arc;
use std::thread;

const K: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// Threads appending to one shared session must all get unique, contiguous
/// seqs: the next-seq read and the insert are one atomic step.
#[test]
fn concurrent_appends_get_unique_seqs() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("c.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store.new_session(Some("concurrent")).unwrap();

    const THREADS: usize = 8;
    const PER_THREAD: usize = 25;
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for i in 0..PER_THREAD {
                let payload = format!("{{\"t\":{t},\"i\":{i}}}");
                let seq = s
                    .append_get_seq(session, EventKind::Prompt, "prompt", &payload)
                    .unwrap();
                assert!(seq >= 1 && seq <= (THREADS * PER_THREAD) as i64);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let events = store.events(session).unwrap();
    assert_eq!(events.len(), THREADS * PER_THREAD);
    let mut seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    seqs.sort();
    let expected: Vec<i64> = (1..=(THREADS * PER_THREAD) as i64).collect();
    assert_eq!(seqs, expected, "seqs must be unique and contiguous 1..N");
}

/// Concurrent session creation must not hand one thread another's rowid.
#[test]
fn concurrent_sessions_get_unique_ids() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("c.db");
    let store = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let mut handles = Vec::new();
    for t in 0..8 {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            for i in 0..10 {
                let id = s.new_session(Some("concurrent sessions")).unwrap();
                assert!(id >= 1 && id <= 80, "id {id} out of range (t{t} i{i})");
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let sessions = store.sessions().unwrap();
    assert_eq!(sessions.len(), 80);
    let mut ids: Vec<i64> = sessions.iter().map(|s| s.0).collect();
    ids.sort();
    let expected: Vec<i64> = (1..=80).collect();
    assert_eq!(ids, expected);
}

/// The store opens the database in WAL mode with a busy timeout, so a second
/// dashcam process recording against the same file waits out the writer
/// instead of failing.
#[test]
fn opens_in_wal_mode_with_busy_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("w.db");
    let store = Store::open(&db_path, Some(K)).unwrap();
    assert_eq!(store.busy_timeout_ms(), 5000);
    // WAL persists in the file itself: an independent connection sees it
    let raw = rusqlite::Connection::open(&db_path).unwrap();
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

/// Two Store handles on one database file (what two processes see): writes
/// from both must land, none lost to SQLITE_BUSY.
#[test]
fn two_handles_on_one_database_both_write() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("two.db");

    let store_a = Arc::new(Store::open(&db_path, Some(K)).unwrap());
    let session = store_a.new_session(Some("two handles")).unwrap();
    let store_b = Store::open(&db_path, Some(K)).unwrap();

    let a = Arc::clone(&store_a);
    let writer_a = thread::spawn(move || {
        for i in 0..50 {
            a.append_get_seq(session, EventKind::Prompt, "prompt a", &format!("{{\"i\":{i}}}"))
                .unwrap();
        }
    });
    let writer_b = thread::spawn(move || {
        for i in 0..50 {
            store_b
                .append_get_seq(session, EventKind::Prompt, "prompt b", &format!("{{\"i\":{i}}}"))
                .unwrap();
        }
    });
    writer_a.join().unwrap();
    writer_b.join().unwrap();

    let events = store_a.events(session).unwrap();
    assert_eq!(events.len(), 100);
    let mut seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    seqs.sort();
    let expected: Vec<i64> = (1..=100).collect();
    assert_eq!(seqs, expected, "seqs must stay unique across handles");
}
