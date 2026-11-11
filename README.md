# dashcam

A vendor-neutral local flight recorder for AI agent sessions. dashcam sits
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

    dashcam record --upstream https://api.openai.com --note "my agent, task X"

The proxy forwards to the real provider, records both directions as one hop
per turn, and derives tool-call hops from the responses. Streaming
(text/event-stream) responses pass through chunk by chunk and still land as
one complete hop.

Read the timeline:

    dashcam timeline                      # sessions and hop counts
    dashcam replay --session 1            # every hop, oldest first
    dashcam replay --session 1 --interactive   # step through, state at each hop
    dashcam serve --port 8098             # browse in a local web page

Record file changes at the file boundary:

    dashcam diff src/main.rs --session 1

## How it keeps itself safe

- **Redaction always on.** Token-shaped secrets (sk-, Bearer, nvapi-) are
  scrubbed at write time. Extra rules with `--rules file` add to the
  built-ins; they never weaken them.
- **Headers are never stored**, so API keys have nowhere to land.
- **Retention.** `--max-events` (count), `--max-age-days` (age),
  `--max-size` (stored bytes). Oldest go first, and emptied sessions go
  with them.
- **Export before aging out.** `--export file` makes retention append every
  aged-out hop to that file as readable JSONL first; `dashcam export
  --session 1` does the same for a whole session on demand.

## Scan it from an agent

`dashcam mcp` speaks the Model Context Protocol on stdio, read-only, local
only. Point an MCP client at the binary and it can list sessions, walk
hops, search the timeline, pull any hop or file diff, and summarize a
session: list_sessions, list_hops, search_timeline, get_hop, get_diff,
summarize_session. It also speaks resources: one resource per session
(`dashcam://session/1` reads back that session's summary, and
`dashcam://session/1/hops/2` reads back one hop's full record), so an agent
can browse the timeline instead of only tool-calling.

## Multi-agent traces

A harness that stamps an `X-Dashcam-Agent` header on its API calls gets that
identity recorded on every hop, so one timeline can show which agent did
what.

## Build

    cargo build && cargo test

Rust, no runtime dependencies beyond SQLite (bundled). See
docs/architecture.md for the design.

## WebSocket passthrough (realtime and voice agents)

Some agents never speak request-response HTTP; they hold a WebSocket open
(OpenAI Realtime, Gemini Live, and similar). Run a second port beside the
proxy:

    dashcam record --port 8099 --upstream https://api.openai.com \
        --ws-port 8098 --ws-upstream wss://api.openai.com

Point the harness's WebSocket client at ws://127.0.0.1:8098 and dashcam
terminates the connection, records every frame in both directions with
timestamps, and forwards over its own secure connection to the provider.
Auth headers are forwarded upstream and never recorded. One model_turn hop
is written per connection: transport "websocket", the ordered `frames`
transcript, plus the first text frame each way as request_body and
response_body. Frames over 32 MiB are refused, a session stops recording
past 8 MiB, fragmented messages keep their framing, and an idle session is
cut at 10 minutes. The wss:// path uses the standard webpki roots; it is
compile-checked and unit-tested but has not been driven against a live
provider yet.

## Network metadata tap (optional, off by default)

    dashcam tap --pcap capture.pcap --note why
    dashcam tap --live   # needs root or CAP_NET_RAW

Records which providers got called, and when: the TLS SNI hostname (read
without any decryption; no payloads, no man-in-the-middle, no root
certificates), bytes in and out per connection, and a flow-end hop with the
totals. Written as network_hop hops. A ClientHello must arrive inside the
first 16 KiB of a connection (virtually always true); on a locked-down
machine, capture offline with `tcpdump -i any -w file.pcap` and replay it
here.
