import sys
p = sys.argv[1]
s = open(p).read()

def rep(old, new, count=1):
    global s
    n = s.count(old)
    assert n == count, f"expected {count} got {n}: {old[:70]!r}"
    s = s.replace(old, new)

if "capture/mod.rs" in p:
    rep("""    provider: &str,
    agent: &str,
) {""", """    provider: &str,
    agent: &str,
    parent: &str,
    run: &str,
) {""")
    rep("""    if !agent.is_empty() {
        payload["agent"] = serde_json::Value::String(agent.to_string());
    }""", """    if !agent.is_empty() {
        payload["agent"] = serde_json::Value::String(agent.to_string());
    }
    if !parent.is_empty() {
        payload["parent"] = serde_json::Value::String(parent.to_string());
    }
    if !run.is_empty() {
        payload["run"] = serde_json::Value::String(run.to_string());
    }""")
    rep("""        if !agent.is_empty() {
            tc_payload["agent"] = serde_json::Value::String(agent.to_string());
        }""", """        if !agent.is_empty() {
            tc_payload["agent"] = serde_json::Value::String(agent.to_string());
        }
        if !parent.is_empty() {
            tc_payload["parent"] = serde_json::Value::String(parent.to_string());
        }
        if !run.is_empty() {
            tc_payload["run"] = serde_json::Value::String(run.to_string());
        }""")
    rep("""        let mut agent = String::new();""", """        let mut agent = String::new();
        let mut parent = String::new();
        let mut run = String::new();""")
    rep("""            } else if name.eq_ignore_ascii_case("x-dashcam-agent") {
                // multi-agent trace propagation: which agent produced this hop
                agent = h.value.as_str().trim().to_string();
            }""", """            } else if name.eq_ignore_ascii_case("x-dashcam-agent") {
                // multi-agent trace propagation: which agent produced this hop
                agent = h.value.as_str().trim().to_string();
            } else if name.eq_ignore_ascii_case("x-dashcam-parent") {
                // spawn-tree attribution: the agent that spawned this one
                parent = h.value.as_str().trim().to_string();
            } else if name.eq_ignore_ascii_case("x-dashcam-run") {
                // swarm grouping: every agent of one run shares this id
                run = h.value.as_str().trim().to_string();
            }""")
    rep("""        // Machine door for language shims: a shim forwards an exchange it""",
"""        if agent.is_empty() {
            agent = std::env::var("DASHCAM_AGENT_ID").unwrap_or_default();
        }
        if parent.is_empty() {
            parent = std::env::var("DASHCAM_PARENT").unwrap_or_default();
        }
        if run.is_empty() {
            run = std::env::var("DASHCAM_RUN").unwrap_or_default();
        }
        // Machine door for language shims: a shim forwards an exchange it""")
    rep("""                        true,
                        &provider,
                        &agent,
                    );""", """                        true,
                        &provider,
                        &agent,
                        &parent,
                        &run,
                    );""")
    rep("""                        false,
                        &provider,
                        &agent,
                    );""", """                        false,
                        &provider,
                        &agent,
                        &parent,
                        &run,
                    );""")
    rep("""    let agent = v["agent"].as_str().unwrap_or("").to_string();
    record_turn(
        store,
        cfg,
        &method,
        &path,
        status,
        &request_body,
        &response_body,
        streamed,
        provider,
        &agent,
    );""", """    let agent = v["agent"].as_str().unwrap_or("").to_string();
    let mut parent = v["parent"].as_str().unwrap_or("").to_string();
    let mut run = v["run"].as_str().unwrap_or("").to_string();
    let agent = if agent.is_empty() { std::env::var("DASHCAM_AGENT_ID").unwrap_or_default() } else { agent };
    if parent.is_empty() {
        parent = std::env::var("DASHCAM_PARENT").unwrap_or_default();
    }
    if run.is_empty() {
        run = std::env::var("DASHCAM_RUN").unwrap_or_default();
    }
    record_turn(
        store,
        cfg,
        &method,
        &path,
        status,
        &request_body,
        &response_body,
        streamed,
        provider,
        &agent,
        &parent,
        &run,
    );""")

elif "ws.rs" in p:
    rep("""    let path = req_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();""", """    let path = req_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let mut agent = String::new();
    let mut parent = String::new();
    let mut run = String::new();
    for (k, v) in &headers {
        match k.as_str() {
            "x-dashcam-agent" => agent = v.trim().to_string(),
            "x-dashcam-parent" => parent = v.trim().to_string(),
            "x-dashcam-run" => run = v.trim().to_string(),
            _ => {}
        }
    }
    if agent.is_empty() {
        agent = std::env::var("DASHCAM_AGENT_ID").unwrap_or_default();
    }
    if parent.is_empty() {
        parent = std::env::var("DASHCAM_PARENT").unwrap_or_default();
    }
    if run.is_empty() {
        run = std::env::var("DASHCAM_RUN").unwrap_or_default();
    }""")
    rep("""fn write_hop(
    store: &Arc<Store>,
    cfg: &WsConfig,
    path: &str,
    frames: &Arc<Mutex<Vec<serde_json::Value>>>,
) {""", """fn write_hop(
    store: &Arc<Store>,
    cfg: &WsConfig,
    path: &str,
    frames: &Arc<Mutex<Vec<serde_json::Value>>>,
    agent: &str,
    parent: &str,
    run: &str,
) {""")
    rep("""    let payload = serde_json::json!({
        "transport": "websocket",
        "provider": crate::capture::detect_provider(path),
        "upstream": cfg.upstream,
        "path": path,
        "frames": frames,
        "request_body": first("c2s"),
        "response_body": first("s2c"),
    });""", """    let mut payload = serde_json::json!({
        "transport": "websocket",
        "provider": crate::capture::detect_provider(path),
        "upstream": cfg.upstream,
        "path": path,
        "frames": frames,
        "request_body": first("c2s"),
        "response_body": first("s2c"),
    });
    if !agent.is_empty() {
        payload["agent"] = serde_json::Value::String(agent.to_string());
    }
    if !parent.is_empty() {
        payload["parent"] = serde_json::Value::String(parent.to_string());
    }
    if !run.is_empty() {
        payload["run"] = serde_json::Value::String(run.to_string());
    }""")
    rep("""    write_hop(store, cfg, &path, &frames);""", """    write_hop(store, cfg, &path, &frames, &agent, &parent, &run);""")

elif "store.rs" in p:
    rep("""    pub fn events(&self, session: i64) -> rusqlite::Result<Vec<Event>> {""",
"""    /// Sessions whose events carry `"field":"value"` in their hop payload.
    /// `field` is caller-whitelisted (agent or run).
    pub fn sessions_with_tag(
        &self,
        field: &str,
        value: &str,
    ) -> rusqlite::Result<Vec<(i64, String, Option<String>, i64)>> {
        let conn = self.conn();
        let pat = format!("%\\"{field}\\":\\"{value}\\"%");
        let mut stmt = conn.prepare(
            "SELECT s.id, s.started_at, s.note,
                    (SELECT COUNT(*) FROM events e WHERE e.session_id = s.id) AS n
             FROM sessions s
             WHERE EXISTS (SELECT 1 FROM events e WHERE e.session_id = s.id AND e.payload LIKE ?1)
             ORDER BY s.id",
        )?;
        let rows = stmt.query_map([&pat], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, started, note, n) = row?;
            out.push((id, started, note.map(|n| self.dec(&n)), n));
        }
        Ok(out)
    }

    pub fn events(&self, session: i64) -> rusqlite::Result<Vec<Event>> {""")

elif "main.rs" in p:
    rep("""    Timeline,""", """    Timeline {
        /// Only sessions with hops from this agent (X-Dashcam-Agent or DASHCAM_AGENT_ID)
        #[arg(long)]
        agent: Option<String>,
        /// Only sessions sharing this run id (X-Dashcam-Run or DASHCAM_RUN)
        #[arg(long)]
        run: Option<String>,
    },""")
    rep("""        Command::Timeline => {
            let store = open_store(&db_path);
            for (id, started, note, events) in store.sessions().expect("list sessions") {
                println!("{id}\\t{started}\\t{events}\\t{}", note.unwrap_or_default());
            }
        }""", """        Command::Timeline { agent, run } => {
            let store = open_store(&db_path);
            let sessions = match (agent.as_deref(), run.as_deref()) {
                (None, None) => store.sessions().expect("list sessions"),
                (a, r) => {
                    let (field, value) = match (a, r) {
                        (Some(a), None) => ("agent", a),
                        (None, Some(r)) => ("run", r),
                        (Some(_), Some(_)) => {
                            eprintln!("pass --agent or --run, not both");
                            std::process::exit(2);
                        }
                    };
                    store.sessions_with_tag(field, value).expect("filter sessions")
                }
            };
            for (id, started, note, events) in sessions {
                println!("{id}\\t{started}\\t{events}\\t{}", note.unwrap_or_default());
            }
        }""")

elif "replay.rs" in p:
    rep("""        let e = self.current();
        let mut out = String::new();
        out.push_str(&format!(
            "hop {} of {} - {:?} - {}\\n{}\\n",
            e.seq,
            self.events.len(),
            e.kind,
            e.summary,
            e.ts
        ));""", """        let e = self.current();
        let mut out = String::new();
        let mut tags = String::new();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) {
            if let Some(a) = v.get("agent").and_then(|x| x.as_str()) {
                tags.push_str(&format!(" [agent {a}]"));
            }
            if let Some(p) = v.get("parent").and_then(|x| x.as_str()) {
                tags.push_str(&format!(" [parent {p}]"));
            }
        }
        out.push_str(&format!(
            "hop {} of {} - {:?} - {}{}\\n{}\\n",
            e.seq,
            self.events.len(),
            e.kind,
            e.summary,
            tags,
            e.ts
        ));""")

else:
    raise SystemExit("unknown file")

open(p, "w").write(s)
print("patched", p)
