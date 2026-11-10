use mayday::capture;
use mayday::key;
use mayday::store::{RetentionPolicy, Store};
use mayday::web;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mayday", version, about = "Local flight recorder for agent sessions")]
struct Cli {
    /// Path to the mayday timeline database (default: ~/.mayday/timeline.db)
    #[arg(short, long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record a session by proxying the model API boundary
    Record {
        /// Local port to listen on (the harness points its API base URL here)
        #[arg(short, long, default_value = "8099")]
        port: u16,
        /// Real provider base URL, e.g. https://api.openai.com
        #[arg(short, long)]
        upstream: String,
        /// Human label for the session, e.g. the harness and task
        #[arg(short, long)]
        note: Option<String>,
        /// Keep at most this many events in the timeline (oldest pruned)
        #[arg(long)]
        max_events: Option<i64>,
        /// Drop events from sessions older than this many days
        #[arg(long)]
        max_age_days: Option<i64>,
        /// Keep the timeline under this many stored bytes (oldest pruned)
        #[arg(long)]
        max_size: Option<i64>,
        /// Before retention ages hops out, append them to this JSONL file
        #[arg(long)]
        export: Option<PathBuf>,
        /// Extra redaction rules, one regex per line; built-ins always on
        #[arg(short, long)]
        rules: Option<PathBuf>,
    },
    /// Apply retention now: keep the newest history, prune the rest
    Prune {
        /// Keep at most this many events in the timeline
        #[arg(long)]
        max_events: Option<i64>,
        /// Drop events from sessions older than this many days
        #[arg(long)]
        max_age_days: Option<i64>,
        /// Keep the timeline under this many stored bytes (oldest pruned)
        #[arg(long)]
        max_size: Option<i64>,
        /// Before retention ages hops out, append them to this JSONL file
        #[arg(long)]
        export: Option<PathBuf>,
    },
    /// Export one session's hops as JSON lines (readable plaintext)
    Export {
        /// Session id (see `mayday timeline`)
        #[arg(short, long)]
        session: i64,
        /// Write to this file instead of stdout
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Browse the timeline in a local web page (read-only, 127.0.0.1 only)
    Serve {
        /// Local port for the viewer
        #[arg(short, long, default_value = "8098")]
        port: u16,
    },
    /// Record a file change into the session timeline (file boundary)
    Diff {
        /// File to record; a missing file with a prior snapshot records the deletion
        path: PathBuf,
        /// Session id (see `mayday timeline`)
        #[arg(short, long)]
        session: i64,
    },
    /// List sessions and their event counts
    Timeline,
    /// Serve the scan tools over the MCP stdio protocol (read-only)
    Mcp,
    /// Print every event of one session, oldest first
    Replay {
        /// Session id (see `mayday timeline`)
        #[arg(short, long)]
        session: i64,
        /// Step through one hop at a time instead of dumping the session
        #[arg(short, long)]
        interactive: bool,
    },
    /// Follow a live session, printing each hop as it lands (ctrl-c to stop)
    Watch {
        /// Session id (see `mayday timeline`)
        #[arg(short, long)]
        session: i64,
        /// Seconds between polls
        #[arg(long, default_value_t = 0.5)]
        interval: f64,
    },
    /// Walk the timeline and report any hop that will not decrypt with the current key
    Verify,
    /// Scan the timeline for redaction leaks: secret-shaped text that should have been scrubbed
    Audit {
        /// Extra redaction rules, one regex per line; built-ins always on
        #[arg(short, long)]
        rules: Option<PathBuf>,
    },
    /// Compare two sessions' tool-call sequences to spot a changed path
    DiffSessions {
        /// First session id (see `mayday timeline`)
        #[arg(short = 'a')]
        a: i64,
        /// Second session id
        #[arg(short = 'b')]
        b: i64,
    },
}

fn default_db() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".mayday");
        p.push("timeline.db");
        return p;
    }
    PathBuf::from("timeline.db")
}

fn open_store(db_path: &std::path::Path) -> Store {
    let mut key_path = db_path.to_path_buf();
    key_path.set_file_name("key");
    let hex = key::load_or_create(&key_path).expect("load or create timeline key");
    Store::open(db_path, Some(&hex)).expect("open timeline db")
}

fn main() {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db);

    match cli.command {
        Command::Record { port, upstream, note, max_events, max_age_days, max_size, export, rules } => {
            let store = Arc::new(open_store(&db_path));
            let id = store.new_session(note.as_deref()).expect("create session");
            println!("session {id} recording on http://127.0.0.1:{port} -> {upstream}");
            println!(
                "point the harness's API base URL at http://127.0.0.1:{port} (plain HTTP to localhost; mayday makes the secure connection outward)"
            );
            let running = Arc::new(AtomicBool::new(true));
            let retention = RetentionPolicy {
                max_events,
                max_age_days,
                max_size_bytes: max_size,
                export_path: export,
            };
            let redactor = match &rules {
                Some(path) => mayday::redact::Redactor::load(path),
                None => mayday::redact::Redactor::builtin(),
            };
            if let Err(e) = capture::serve(
                store,
                capture::ProxyConfig { port, upstream, session: id, retention: Some(retention), redactor },
                running,
            ) {
                eprintln!("capture stopped: {e}");
                std::process::exit(1);
            }
        }
        Command::Prune { max_events, max_age_days, max_size, export } => {
            let store = open_store(&db_path);
            let policy = RetentionPolicy {
                max_events,
                max_age_days,
                max_size_bytes: max_size,
                export_path: export,
            };
            let (events, sessions) = store.prune(&policy).expect("prune timeline");
            println!("pruned {events} events, {sessions} emptied sessions");
        }
        Command::Serve { port } => {
            let store = Arc::new(open_store(&db_path));
            println!("viewing http://127.0.0.1:{port}");
            let running = Arc::new(AtomicBool::new(true));
            if let Err(e) = web::serve(store, web::WebConfig { port }, running) {
                eprintln!("viewer stopped: {e}");
                std::process::exit(1);
            }
        }
        Command::Diff { path, session } => {
            let store = open_store(&db_path);
            match mayday::filediff::record(&store, session, &path) {
                Ok(out) => {
                    if out.changed {
                        println!("{}", out.summary);
                    } else {
                        println!("{} (unchanged, nothing recorded)", path.display());
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Watch { session, interval } => {
            let store = open_store(&db_path);
            let exists = store
                .sessions()
                .expect("list sessions")
                .iter()
                .any(|(id, _, _, _)| *id == session);
            if !exists {
                eprintln!("no session {session}");
                std::process::exit(1);
            }
            let existing = store.events(session).expect("read session");
            for e in &existing {
                println!("{}", mayday::watch::render_line(e));
            }
            let mut last_seq = existing.last().map(|e| e.seq).unwrap_or(0);
            println!("watching session {session} (ctrl-c to stop)");
            let gap = std::time::Duration::from_secs_f64(interval.max(0.1));
            loop {
                std::thread::sleep(gap);
                match mayday::watch::poll_once(&store, session, last_seq) {
                    Ok(events) => {
                        for e in events {
                            println!("{}", mayday::watch::render_line(&e));
                            last_seq = e.seq;
                        }
                    }
                    Err(e) => {
                        eprintln!("watch: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::Verify => {
            let store = open_store(&db_path);
            match store.verify() {
                Ok(report) => {
                    if !report.encrypted {
                        println!(
                            "plaintext store (no key): nothing to decrypt, {} hops on file",
                            report.checked
                        );
                    } else if report.failed.is_empty() {
                        println!(
                            "ok: every encrypted field decrypts ({} hops checked)",
                            report.checked
                        );
                    } else {
                        println!(
                            "{} of {} hops will not decrypt with this key:",
                            report.failed.len(),
                            report.checked
                        );
                        for f in &report.failed {
                            println!("  session {} seq {} ({})", f.session_id, f.seq, f.field);
                        }
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("verify: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Audit { rules } => {
            let store = open_store(&db_path);
            let redactor = match rules {
                Some(path) => mayday::redact::Redactor::load(&path),
                None => mayday::redact::Redactor::builtin(),
            };
            match store.audit(&redactor) {
                Ok(report) => {
                    if report.leaks.is_empty() {
                        println!(
                            "clean: {} hops scanned, no scrub-shaped secrets found",
                            report.scanned
                        );
                        if report.undecryptable > 0 {
                            println!(
                                "{} fields could not be decrypted and were skipped (run mayday verify)",
                                report.undecryptable
                            );
                        }
                    } else {
                        println!(
                            "{} leak(s) across {} hops scanned:",
                            report.leaks.len(),
                            report.scanned
                        );
                        for l in &report.leaks {
                            println!(
                                "  session {} seq {} ({}): {}",
                                l.session_id, l.seq, l.field, l.preview
                            );
                        }
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("audit: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::DiffSessions { a, b } => {
            let store = open_store(&db_path);
            match (store.tool_call_names(a), store.tool_call_names(b)) {
                (Ok(ac), Ok(bc)) => {
                    let names_a: Vec<String> = ac.into_iter().map(|(_, n)| n).collect();
                    let names_b: Vec<String> = bc.into_iter().map(|(_, n)| n).collect();
                    let diff = mayday::compare::diff_tool_sequences(&names_a, &names_b);
                    println!(
                        "session {a}: {} tool calls, session {b}: {} tool calls, {} aligned",
                        diff.a_calls, diff.b_calls, diff.common
                    );
                    match diff.first_divergence {
                        None => println!("same path: every call lines up"),
                        Some((ia, ib)) => {
                            println!(
                                "first divergence: session {a} step {} / session {b} step {}",
                                ia + 1,
                                ib + 1
                            );
                            for (step, name) in &diff.only_in_a {
                                println!("  only in {a}: step {} {name}", step + 1);
                            }
                            for (step, name) in &diff.only_in_b {
                                println!("  only in {b}: step {} {name}", step + 1);
                            }
                        }
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("diff: {e}");
                    std::process::exit(1);
                }
            }
        }
        Command::Timeline => {
            let store = open_store(&db_path);
            for (id, started, note, events) in store.sessions().expect("list sessions") {
                println!("{id}\t{started}\t{events}\t{}", note.unwrap_or_default());
            }
        }
        Command::Export { session, out } => {
            let store = open_store(&db_path);
            let result = match &out {
                Some(path) => std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|e| format!("{}: {e}", path.display()))
                    .and_then(|mut f| store.export_session(session, &mut f)),
                None => store.export_session(session, &mut std::io::stdout()),
            };
            if let Err(e) = result {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Command::Mcp => {
            let store = Arc::new(open_store(&db_path));
            if let Err(e) = mayday::mcp::run(store) {
                eprintln!("mcp stopped: {e}");
                std::process::exit(1);
            }
        }
        Command::Replay { session, interactive } => {
            let store = open_store(&db_path);
            let events = store.events(session).expect("read events");
            if !interactive {
                for e in events {
                    println!("[{}] {:?} {}", e.ts, e.kind, e.summary);
                    if e.payload.len() > 2 && e.payload != "{}" {
                        println!("    {}", e.payload);
                    }
                }
                return;
            }
            if events.is_empty() {
                println!("session {session} has no hops");
                return;
            }
            let mut r = mayday::replay::Replay::new(events);
            println!("{}", r.render_current());
            let stdin = std::io::stdin();
            loop {
                print!(
                    "[{}/{}] enter=next b=back f=first l=last q=quit > ",
                    r.position() + 1,
                    r.len()
                );
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                match stdin.read_line(&mut line) {
                    Ok(0) => break, // EOF: stop cleanly
                    Ok(_) => {}
                    Err(_) => break,
                }
                match line.trim() {
                    "" => {
                        if !r.next() {
                            println!("(last hop)");
                        } else {
                            println!("{}", r.render_current());
                        }
                    }
                    "b" => {
                        if !r.back() {
                            println!("(first hop)");
                        } else {
                            println!("{}", r.render_current());
                        }
                    }
                    "f" => {
                        r.first();
                        println!("{}", r.render_current());
                    }
                    "l" => {
                        r.last();
                        println!("{}", r.render_current());
                    }
                    "q" => break,
                    "?" | "help" => println!("enter=next, b=back, f=first, l=last, q=quit"),
                    _ => println!("enter=next, b=back, f=first, l=last, q=quit"),
                }
            }
        }
    }
}
