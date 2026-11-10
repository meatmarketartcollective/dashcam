# mayday

A local flight recorder for agent sessions.

Agent frameworks are huge and their issue trackers are drowning, and underneath
most reports is the same complaint: an agent did something wrong and nobody can
say what it actually saw, decided, or did. Mayday is a local daemon that sits
beside any agent session and records every tool call, prompt, file diff and
model turn into one queryable timeline, then replays a failed run step by step
with the state at each hop.

Vendor-neutral by design: it records whatever harness you point it at.

## Design

- Capture happens at three boundaries, none of which require touching the
  agent's code:
  1. the model API boundary (a localhost proxy that copies requests and
     responses as they pass),
  2. tool execution (hook/wrapper shims that emit a record per call),
  3. the working tree (file diffs between turns).
- Storage is a single SQLite database, one row per event, sessions as the top
  grouping. Fully local; no phone-home.
- The replay UI walks a session turn by turn and answers one question:
  why did it do that.

## Status

MVP scaffold. The timeline store (SQLite, sessions + events) and CLI surface
exist and are tested; capture transports and the replay UI are next.

## License

MIT
