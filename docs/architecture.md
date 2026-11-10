# Architecture

One daemon, one SQLite file, three capture boundaries.

```
agent harness (any) ──► model API
        │                    ▲
        │                    │  1. localhost proxy copies turns
        │  2. tool hook/wrapper emits a record per call
        │  3. working-tree diffs captured between turns
        ▼
   mayday daemon ──► timeline.db (SQLite)
                          │
                    replay UI: why did it do that
```

The rule for capture: passive by default. A harness that can't be hooked
still gets recorded at boundary 1, because every agent talks to a model API
and that traffic crosses a socket we can stand in front of.

## File boundary

`mayday diff <path> --session <id>` records a `file_diff` hop: the harness
calls it after every file change. Edges are handled, not crashed on:

- new file: everything added, `new_file: true`
- deleted file: everything removed, `content: null`; recording a still-missing
  file again is an error, never a fake hop
- binary file: NUL in the first 8 KiB records size only, no content
- non-UTF-8 text: lossy conversion, never a panic
- oversized files: snapshot truncated at 64 KiB with `content_truncated`
- oversized edits: line detail capped at 200 lines with `diff_truncated`
- unchanged file: no hop recorded
- sessions keep separate baselines; the diff is against the newest snapshot
  for that file in that session only

Line diffs are LCS-based after common prefix/suffix trimming, falling back to
remove-all/add-all when the changed middle exceeds the cell budget. Content
and diff lines pass the always-on redactor before write.
