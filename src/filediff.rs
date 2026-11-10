use crate::event::EventKind;
use crate::store::Store;
use serde_json::json;
use std::path::Path;

/// How much of a file's new content is stored per hop. Files bigger than
/// this record a truncated snapshot; the diff lines themselves are capped
/// separately so one huge edit cannot flood the timeline.
const MAX_CONTENT_BYTES: usize = 64 * 1024;
const MAX_DIFF_LINES: usize = 200;
/// LCS DP table cell budget; beyond this the diff falls back to
/// remove-all/add-all for the changed middle.
const MAX_LCS_CELLS: usize = 200_000;

pub struct RecordedDiff {
    pub changed: bool,
    pub summary: String,
}

/// Record one file_diff hop for `path` in `session`.
///
/// Edges handled: new files (everything added), deleted files (everything
/// removed, content null), binary files (NUL in the first 8 KiB: size only,
/// no content), non-UTF-8 text (lossy, never a panic), huge files (truncated
/// snapshot note), huge edits (line detail capped), unchanged files (no hop
/// at all). Content and diff lines pass the always-on redactor before write.
pub fn record(store: &Store, session: i64, path: &Path) -> Result<RecordedDiff, String> {
    let file = path.to_string_lossy().to_string();
    let previous = last_file_content(store, session, &file);

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return match previous {
                Some(prev) => record_deleted(store, session, &file, &prev),
                None => Err(format!(
                    "{} not found and no earlier snapshot in this session",
                    path.display()
                )),
            };
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };

    // binary detection: a NUL in the first 8 KiB marks a binary file
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0u8) {
        let payload = json!({
            "file": file,
            "binary": true,
            "bytes": bytes.len(),
        })
        .to_string();
        let summary = format!("file_diff {file} (binary, {} bytes)", bytes.len());
        store
            .append(session, EventKind::FileDiff, &summary, &scrubbed(&payload))
            .map_err(|e| format!("store: {e}"))?;
        return Ok(RecordedDiff { changed: true, summary });
    }

    let text = String::from_utf8_lossy(&bytes).into_owned();
    let is_new = previous.is_none();
    let (added, removed) = line_diff(previous.as_deref().unwrap_or(""), &text);

    if added.is_empty() && removed.is_empty() && previous == Some(text.clone()) {
        return Ok(RecordedDiff { changed: false, summary: format!("file_diff {file} (unchanged)") });
    }
    if added.is_empty() && removed.is_empty() && !is_new {
        // identical to the stored snapshot: nothing to record
        return Ok(RecordedDiff { changed: false, summary: format!("file_diff {file} (unchanged)") });
    }

    // store the new snapshot, capped on a char boundary
    let (content, truncated) = if text.len() > MAX_CONTENT_BYTES {
        let mut cut = MAX_CONTENT_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        (text[..cut].to_string(), true)
    } else {
        (text.clone(), false)
    };

    let (added, removed, diff_truncated) = cap_lines(added, removed);

    let mut payload = json!({
        "file": file,
        "added": added,
        "removed": removed,
        "content": content,
    });
    if truncated {
        payload["content_truncated"] = json!(true);
    }
    if diff_truncated {
        payload["diff_truncated"] = json!(true);
    }
    if is_new {
        payload["new_file"] = json!(true);
    }
    let payload = payload.to_string();

    let counts = if is_new {
        format!("(new file, +{} lines)", payload_added_len(&payload))
    } else {
        format!("+{} -{} lines", added_len(&payload), removed_len(&payload))
    };
    let summary = format!("file_diff {file} {counts}");
    store
        .append(session, EventKind::FileDiff, &summary, &scrubbed(&payload))
        .map_err(|e| format!("store: {e}"))?;
    Ok(RecordedDiff { changed: true, summary })
}

fn record_deleted(store: &Store, session: i64, file: &str, prev: &str) -> Result<RecordedDiff, String> {
    let (_, removed) = line_diff(prev, "");
    let (_, removed, diff_truncated) = cap_lines(Vec::new(), removed);
    let mut payload = json!({
        "file": file,
        "deleted": true,
        "content": serde_json::Value::Null,
        "removed": removed,
    });
    if diff_truncated {
        payload["diff_truncated"] = json!(true);
    }
    let payload = payload.to_string();
    let summary = format!("file_diff {file} (deleted)");
    store
        .append(session, EventKind::FileDiff, &summary, &scrubbed(&payload))
        .map_err(|e| format!("store: {e}"))?;
    Ok(RecordedDiff { changed: true, summary })
}

/// The newest stored snapshot for this file in this session, or None when the
/// file has never been recorded or was recorded as deleted.
pub fn last_file_content(store: &Store, session: i64, file: &str) -> Option<String> {
    for ev in store.events(session).ok()?.iter().rev() {
        if ev.kind != EventKind::FileDiff {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&ev.payload).ok()?;
        if v.get("file").and_then(|f| f.as_str()) != Some(file) {
            continue;
        }
        // newest record for the file wins: deletion means no baseline
        if v.get("content").and_then(|c| c.as_str()).is_some() {
            return v.get("content").and_then(|c| c.as_str()).map(String::from);
        }
        return None;
    }
    None
}

fn scrubbed(payload: &str) -> String {
    crate::redact::Redactor::builtin().scrub(payload)
}

/// Line diff: common prefix and suffix trimmed, then an LCS backtrack over
/// the changed middle when it fits the cell budget.
fn line_diff(prev: &str, cur: &str) -> (Vec<String>, Vec<String>) {
    let a: Vec<&str> = prev.lines().collect();
    let b: Vec<&str> = cur.lines().collect();
    let mut start = 0;
    while start < a.len() && start < b.len() && a[start] == b[start] {
        start += 1;
    }
    let mut end = 0;
    while end < a.len() - start && end < b.len() - start && a[a.len() - 1 - end] == b[b.len() - 1 - end] {
        end += 1;
    }
    let am = &a[start..a.len() - end];
    let bm = &b[start..b.len() - end];
    if am.is_empty() && bm.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if am.len().saturating_mul(bm.len()) > MAX_LCS_CELLS {
        return (bm.iter().map(|s| s.to_string()).collect(), am.iter().map(|s| s.to_string()).collect());
    }
    let (added, removed) = lcs_diff(am, bm);
    (added, removed)
}

fn lcs_diff(am: &[&str], bm: &[&str]) -> (Vec<String>, Vec<String>) {
    let n = am.len();
    let m = bm.len();
    let mut dp = vec![0usize; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * (m + 1) + j] = if am[i] == bm[j] {
                dp[(i + 1) * (m + 1) + (j + 1)] + 1
            } else {
                dp[(i + 1) * (m + 1) + j].max(dp[i * (m + 1) + (j + 1)])
            };
        }
    }
    let mut kept_a = vec![false; n];
    let mut kept_b = vec![false; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if am[i] == bm[j] {
            kept_a[i] = true;
            kept_b[j] = true;
            i += 1;
            j += 1;
        } else if dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + (j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let removed: Vec<String> = am
        .iter()
        .zip(kept_a)
        .filter(|(_, k)| !k)
        .map(|(s, _)| s.to_string())
        .collect();
    let added: Vec<String> = bm
        .iter()
        .zip(kept_b)
        .filter(|(_, k)| !k)
        .map(|(s, _)| s.to_string())
        .collect();
    (added, removed)
}

fn cap_lines(added: Vec<String>, removed: Vec<String>) -> (Vec<String>, Vec<String>, bool) {
    let mut truncated = false;
    let mut a = added;
    if a.len() > MAX_DIFF_LINES {
        a.truncate(MAX_DIFF_LINES);
        truncated = true;
    }
    let mut r = removed;
    if r.len() > MAX_DIFF_LINES {
        r.truncate(MAX_DIFF_LINES);
        truncated = true;
    }
    (a, r, truncated)
}

fn payload_added_len(payload: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("added").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn added_len(payload: &str) -> usize {
    payload_added_len(payload)
}

fn removed_len(payload: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("removed").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.len())
        .unwrap_or(0)
}
