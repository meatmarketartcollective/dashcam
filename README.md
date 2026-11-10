# mayday

A vendor-neutral local flight recorder for AI agent sessions. mayday sits
beside any agent harness and passively records what the agent actually did:
the model API boundary, the tool calls, the file changes. Then it replays any
session hop by hop, showing the state at each step, so a run that went wrong
can be understood after the fact.

Nothing leaves the machine. The timeline is an encrypted SQLite database
(AES-256-GCM, app-managed key, no user credentials), the proxy and the
viewer bind to 127.0.0.1 only, and secrets are redacted before anything is
written. Headers are never recorded at all.

## Quick start

Record a session by pointing the harness's API base URL at the proxy:

    mayday record --upstream https://api.openai.com --note "my agent, task X"

The proxy forwards to the real provider, records both directions as one hop
per turn, and derives tool-call hops from the responses. Streaming
(text/event-stream) responses pass through chunk by chunk and still land as
one complete hop.

Read the timeline:

    mayday timeline                      # sessions and hop counts
    mayday replay --session 1            # every hop, oldest first
    mayday replay --session 1 --interactive   # step through, state at each hop
    mayday serve --port 8098             # browse in a local web page

Record file changes at the file boundary:

    mayday diff src/main.rs --session 1

## How it keeps itself safe

- **Redaction always on.** Token-shaped secrets (sk-, Bearer, nvapi-) are
  scrubbed at write time. Extra rules with `--rules file` add to the
  built-ins; they never weaken them.
- **Headers are never stored**, so API keys have nowhere to land.
- **Retention.** `--max-events` (count), `--max-age-days` (age),
  `--max-size` (stored bytes). Oldest go first, and emptied sessions go
  with them.
- **Export before aging out.** `--export file` makes retention append every
  aged-out hop to that file as readable JSONL first; `mayday export
  --session 1` does the same for a whole session on demand.

## Scan it from an agent

`mayday mcp` speaks the Model Context Protocol on stdio, read-only, local
only. Point an MCP client at the binary and it can list sessions, walk
hops, search the timeline, pull any hop or file diff, and summarize a
session: list_sessions, list_hops, search_timeline, get_hop, get_diff,
summarize_session. It also speaks resources: one resource per session
(`mayday://session/1` reads back that session's summary, and
`mayday://session/1/hops/2` reads back one hop's full record), so an agent
can browse the timeline instead of only tool-calling.

## Multi-agent traces

A harness that stamps an `X-Mayday-Agent` header on its API calls gets that
identity recorded on every hop, so one timeline can show which agent did
what.

## Build

    cargo build && cargo test

Rust, no runtime dependencies beyond SQLite (bundled). See
docs/architecture.md for the design.
