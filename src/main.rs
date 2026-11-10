mod event;
mod store;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "mayday", version, about = "Local flight recorder for agent sessions")]
struct Cli {
    /// Path to the mayday timeline database (default: ~/.mayday/timeline.db)
    #[arg(short, long, default_value = None)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record a session (capture transports land here in the MVP+1)
    Record {
        /// Human label for the session, e.g. the harness and task
        #[arg(short, long)]
        note: Option<String>,
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
    dirs_or_cwd()
}

fn dirs_or_cwd() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".mayday");
        p.push("timeline.db");
        return p;
    }
    PathBuf::from("timeline.db")
}

fn main() {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db);

    match cli.command {
        Command::Record { note } => {
            let store = store::Store::open(&db_path).expect("open timeline db");
            let id = store
                .new_session(note.as_deref())
                .expect("create session");
            println!("session {id} opened at {}", db_path.display());
            println!("capture transports are not wired yet; this session records nothing.");
        }
        Command::Timeline => {
            let store = store::Store::open(&db_path).expect("open timeline db");
            for (id, started, note, events) in store.sessions().expect("list sessions") {
                println!("{id}\t{started}\t{events}\t{}", note.unwrap_or_default());
            }
        }
        Command::Replay { session } => {
            let store = store::Store::open(&db_path).expect("open timeline db");
            for e in store.events(session).expect("read events") {
                println!(
                    "[{}] {:?} {}",
                    e.ts, e.kind, e.summary
                );
            }
        }
    }
}
