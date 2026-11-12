import re
s = open("src/store.rs").read()
pat = re.compile(r"        let conn = self\.conn\(\);\n        let pat = format!.*?        Ok\(out\)\n    \}\n\n    pub fn events", re.S)
new = '''        let needle = format!("\\"{field}\\":\\"{value}\\"");
        let mut out = Vec::new();
        for (id, started, note, n) in self.sessions()? {
            let has = self.events(id)?.iter().any(|e| e.payload.contains(&needle));
            if has {
                out.push((id, started, note, n));
            }
        }
        Ok(out)
    }

    pub fn events'''
s2, n = pat.subn(new, s, count=1)
assert n == 1, "regex did not match"
open("src/store.rs", "w").write(s2)
print("store filter rewritten")
