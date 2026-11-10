use mayday::capture;
use mayday::key;
use mayday::store::{RetentionPolicy, Store};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mayday", version, about = "Local flight recorder for agent sessions")]
struct Cli {
    /// Path to the mayday timeline database (default: ~/.mayday/timeline.db)
    #[arg(short, long)]
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
    },
    /// Apply retention now: keep the newest history, prune the rest
    Prune {
        /// Keep at most this many events in the timeline
        #[arg(long)]
        max_events: Option<i64>,
        /// Drop events from sessions older than this many days
        #[arg(long)]
        max_age_days: Option<i64>,
    },
    /// List sessions and their event counts
    Timeline,
    /// Print every event of one session, oldest first
    Replay {
        /// Session id (see `mayday timeline`)
        #[arg(short, long)]
        session: i64,
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
        Command::Record { port, upstream, note, max_events, max_age_days } => {
            let store = Arc::new(open_store(&db_path));
            let id = store.new_session(note.as_deref()).expect("create session");
            println!("session {id} recording on http://127.0.0.1:{port} -> {upstream}");
            println!(
                "point the harness's API base URL at http://127.0.0.1:{port} (plain HTTP to localhost; mayday makes the secure connection outward)"
            );
            let running = Arc::new(AtomicBool::new(true));
            let retention = RetentionPolicy { max_events, max_age_days };
            if let Err(e) = capture::serve(
                store,
                capture::ProxyConfig { port, upstream, session: id, retention: Some(retention) },
                running,
            ) {
                eprintln!("capture stopped: {e}");
                std::process::exit(1);
            }
        }
        Command::Prune { max_events, max_age_days } => {
            let store = open_store(&db_path);
            let policy = RetentionPolicy { max_events, max_age_days };
            let (events, sessions) = store.prune(&policy).expect("prune timeline");
            println!("pruned {events} events, {sessions} emptied sessions");
        }
        Command::Timeline => {
            let store = open_store(&db_path);
            for (id, started, note, events) in store.sessions().expect("list sessions") {
                println!("{id}\t{started}\t{events}\t{}", note.unwrap_or_default());
            }
        }
        Command::Replay { session } => {
            let store = open_store(&db_path);
            for e in store.events(session).expect("read events") {
                println!("[{}] {:?} {}", e.ts, e.kind, e.summary);
                if e.payload.len() > 2 && e.payload != "{}" {
                    println!("    {}", e.payload);
                }
            }
        }
    }
}
