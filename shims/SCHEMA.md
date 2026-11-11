# The shim record schema

Every shim posts one JSON object per model exchange to the daemon's
`/ingest` door (default `http://127.0.0.1:8123/ingest`, local loopback only):

```json
{
  "provider": "openai | anthropic | gemini",
  "method": "POST",
  "path": "v1/chat/completions",
  "status": 200,
  "request_body": "<full request body as text>",
  "response_body": "<full response body as text>",
  "streamed": false,
  "agent": ""
}
```

The daemon writes it through the exact same path as proxied traffic: secret
redaction at write time, one model_turn hop, and derived tool-call hops from
the provider dialect. Shims and the proxy therefore produce identical
records, and nothing about a shim is special anywhere else in dashcam.

Rules every shim follows:
- Loopback hosts are never recorded (no ingest loops).
- Only model-provider hosts are recorded: a built-in allowlist plus
  `DASHCAM_HOSTS` extras.
- A recording failure never breaks the wrapped call.
- Headers are never recorded, so an API key has nowhere to land.
- `streamed` is true when the response content type is text/event-stream.
- `agent` comes from the `DASHCAM_AGENT` env var (multi-agent traces).
