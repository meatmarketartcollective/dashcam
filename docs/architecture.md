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

## Multi-agent trace propagation

Any number of agents can share one proxy session: the timeline is safe for
concurrent writers (WAL + atomic seq). To tell agents apart, a harness sets
the `X-Mayday-Agent` header on model API requests; every hop that agent
produces (model turns, derived tool calls, error hops) carries `"agent"` in
its payload. Agents that do not set the header record without an agent field,
so nothing changes for a single-agent setup.

## Interactive replay

`mayday replay <session> --interactive` steps through the timeline one hop at
a time: the full payload pretty-printed, then the state at that hop (every
summary up to and including it) so a failed run reads like a story. Keys:
enter next, `b` back, `f` first, `l` last, `q` quit. `mayday replay <session>`
keeps the one-shot dump.

## Scan tools over MCP (milestone 3)

`mayday mcp` speaks the Model Context Protocol on stdio: newline-delimited
JSON-RPC 2.0 in, responses out, notifications answer nothing. It is
read-only and local-only; there is no network listener. A developer points
their coding agent's MCP config at the binary and the agent can scan the
timeline it just recorded.

Six tools, all thin wrappers over store reads:

- `list_sessions` - id, start time, note, hop count
- `list_hops {session}` - seq, kind, time, summary per hop
- `search_timeline {query, limit?}` - case-insensitive substring over every
  session's summaries and payloads, capped at 100 hits
- `get_hop {session, seq}` - the full decrypted record; the payload is
  embedded as parsed JSON when it parses
- `get_diff {session, seq}` - the parsed file_diff payload (file, added and
  removed lines, content, new_file/deleted/binary/truncation flags)
- `summarize_session {session}` - hop counts by kind, tools called, files
  touched, time span

Missing arguments, unknown tools, unknown hops, and unknown sessions come
back as `isError: true` results, never a crash. The summarize reader scopes
its connection lock strictly: the store's connection is a non-reentrant
mutex, so no reader may hold it across another reader call.

## Retention completes: size cap and export-before-aging-out

The retention policy now covers all three limits Casey named: count
(`--max-events`), age (`--max-age-days`), and size (`--max-size`, stored
bytes of the encrypted rows). The size cap drops oldest hops first until
the timeline is under the cap.

Nothing retention removes is lost silently: name `--export <file>` and
every aged-out hop lands in that file first, one JSON line per hop,
decrypted, payload embedded as parsed JSON. The file is appended to, so
repeated prunes accumulate. `mayday export --session N [--out file]` does
the same for a whole session on demand (stdout when no file is named).

## Resources: browsing instead of tool-calling

The MCP surface also speaks resources. `resources/list` returns one
resource per session (`mayday://session/{id}`, text/plain); reading it
returns the session summary. `resources/read` also accepts
`mayday://session/{id}/hops/{seq}`, which returns one hop's full record as
JSON with the payload embedded as parsed JSON. Unknown or malformed URIs
are rejected with the MCP "resource not found" error (-32002), never a
crash. Read-only, local-only, same store reads as the scan tools.
