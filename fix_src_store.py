old = """        let conn = self.conn();
        let pat = format!("%\"{field}\":\"{value}\"%");
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
        Ok(out)"""
new = """        let needle = format!("\"{field}\":\"{value}\"");
        let mut out = Vec::new();
        for (id, started, note, n) in self.sessions()? {
            let has = self.events(id)?.iter().any(|e| e.payload.contains(&needle));
            if has {
                out.push((id, started, note, n));
            }
        }
        Ok(out)"""
s = open("src/store.rs").read()
assert s.count(old) == 1, "filter anchor missing"
open("src/store.rs", "w").write(s.replace(old, new, 1))
print("store filter fixed")