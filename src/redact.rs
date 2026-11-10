use regex::Regex;

/// Redaction at write time. Built-in rules are always on and cannot be
/// disabled; a rules file may add more, never remove the built-ins.
///
/// Built-in rules mask token-shaped secrets: a known prefix (`sk-`, `Bearer `,
/// `nvapi-`) followed by a run of 8 or more token characters, with the prefix
/// kept so the record still shows what kind of credential it was.
///
/// User rules from a rules file are plain regular expressions: every whole
/// match is replaced with `[redacted]`. One rule per line; blank lines and
/// lines starting with `#` are ignored; a line that does not compile as a
/// regex is skipped rather than failing capture.
pub struct Redactor {
    builtins: Vec<Regex>,
    extra: Vec<Regex>,
}

impl Redactor {
    pub fn builtin() -> Self {
        let builtins = vec![
            // prefix kept, token run masked
            Regex::new(r"((?:sk-|Bearer |nvapi-))[A-Za-z0-9_.\-]{8,}").unwrap(),
        ];
        Self {
            builtins,
            extra: Vec::new(),
        }
    }

    /// Built-in rules plus every valid rule in the file at `path`.
    pub fn load(path: &std::path::Path) -> Self {
        let mut redactor = Self::builtin();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Ok(re) = Regex::new(line) {
                    redactor.extra.push(re);
                }
            }
        }
        redactor
    }

    /// Apply built-ins first, then user rules. Always on; no opt-out in v1.
    pub fn scrub(&self, s: &str) -> String {
        let mut out = s.to_string();
        for re in &self.builtins {
            out = re.replace_all(&out, "${1}[redacted]").into_owned();
        }
        for re in &self.extra {
            out = re.replace_all(&out, "[redacted]").into_owned();
        }
        out
    }
}
