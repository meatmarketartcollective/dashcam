use dashcam::redact::Redactor;
use std::io::Write;

/// Built-ins: the prefixes the hand-rolled scanner used, same behavior.
#[test]
fn builtins_mask_token_runs_and_keep_the_prefix() {
    let r = Redactor::builtin();
    assert_eq!(r.scrub("use sk-abcdef0123456789 here"), "use sk-[redacted] here");
    assert_eq!(r.scrub("sk-short"), "sk-short");
    assert_eq!(r.scrub("Bearer abcdefghijklmnop"), "Bearer [redacted]");
    assert_eq!(r.scrub("no secrets here"), "no secrets here");
    // nvapi- keys are masked too
    let nv = format!("nvapi-{}", "a1B2c3D4e5F6g7H8i9J0");
    let out = r.scrub(&nv);
    assert!(!out.contains("a1B2c3D4e5F6g7H8i9J0"));
    assert!(out.contains("[redacted]"));
}

/// User rules add on top of the built-ins, never remove them.
#[test]
fn rules_file_adds_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "# one regex per line; comments and blanks ignored").unwrap();
    writeln!(f).unwrap();
    writeln!(f, "ACME-[A-Z0-9]{{8,}}").unwrap();
    writeln!(f, "password=\\S+").unwrap();
    // a line that is not a valid regex is skipped, not fatal
    writeln!(f, "[invalid(regex").unwrap();
    drop(f);

    let r = Redactor::load(&path);
    let out = r.scrub("key is ACME-1234567890ABC and password=hunter2 and sk-abcdefghijkl");
    assert!(!out.contains("ACME-1234567890ABC"));
    assert!(!out.contains("hunter2"));
    // built-in still fires: user rules cannot weaken redaction
    assert!(out.contains("sk-[redacted]"));
    assert_eq!(out.matches("[redacted]").count(), 3);
}

/// A missing rules file is not an error: built-ins still apply.
#[test]
fn missing_rules_file_still_redacts() {
    let r = Redactor::load(std::path::Path::new("/nonexistent/rules"));
    assert_eq!(r.scrub("sk-abcdef0123456789"), "sk-[redacted]");
}
