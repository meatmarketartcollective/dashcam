# Redaction

Everything dashcam records is scrubbed at write time, before it reaches the
timeline. There is no opt-out in v1: the built-in rules always run, and a
rules file can only add more.

## Built-in rules

Token-shaped secrets: a known credential prefix followed by 8 or more token
characters (`A-Z a-z 0-9 _ . -`) is masked, and the prefix is kept so a replay
still shows what kind of credential it was.

| Prefix    | Example before                    | After                  |
|-----------|-----------------------------------|------------------------|
| `sk-`     | `sk-ab12cd34ef56gh78`             | `sk-[redacted]`        |
| `Bearer ` | `Bearer ab12cd34ef56gh78`         | `Bearer [redacted]`    |
| `nvapi-`  | `nvapi-ab12cd34ef56gh78ij90`      | `nvapi-[redacted]`     |

Runs shorter than 8 characters are left alone, so `sk-short` stays readable.
Authorization headers are never recorded at all.

## Rules file

`dashcam record --rules ~/.dashcam/redaction.rules` loads extra rules on top of
the built-ins. Format:

- one regular expression per line;
- every whole match is replaced with `[redacted]` (the built-ins keep their
  prefix; user rules do not);
- blank lines and lines starting with `#` are ignored;
- a line that does not compile as a regex is skipped, never fatal;
- a missing or unreadable file is not an error: the built-ins still apply.

Example `~/.dashcam/redaction.rules`:

```
# vendor-specific API keys
ACME-[A-Z0-9]{8,}
# anything after password=
password=\S+
```

Rules apply in addition to the built-ins, never instead of them: user rules
cannot weaken redaction.

## Where it applies

Every recorded payload passes through the redactor before it is written:
request bodies, response bodies (streaming and buffered), derived tool-call
arguments, and the 502 error path. The raw database file holds no readable
secret (leak-checked in tests).
