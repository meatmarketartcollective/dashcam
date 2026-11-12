s = open("README.md").read()
anchor = "## How it keeps itself safe"
assert anchor in s
block = """### Swarm attribution

A swarm is many agents hitting one recorder. Identity is opt-in per hop: a
harness that stamps `X-Dashcam-Agent` (and optionally `X-Dashcam-Parent` and
`X-Dashcam-Run`) gets it recorded on every hop. Agents that only inherit the
environment can set `DASHCAM_AGENT_ID`, `DASHCAM_PARENT` and `DASHCAM_RUN`
instead; the recorder falls back to those when the headers are absent. The
WebSocket passthrough reads the same three headers on the upgrade request.

Filter the timeline to one agent or one run:

    dashcam timeline --agent researcher
    dashcam timeline --run 2026-11-11-batch

Replay marks each hop with its agent and parent, so one session reads as the
spawn tree it was.

"""
open("README.md", "w").write(s.replace(anchor, block + anchor, 1))
print("README patched")
