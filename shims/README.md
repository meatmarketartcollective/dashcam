# dashcam shims

For harnesses you cannot point at the proxy, a shim records model API
traffic from inside the process itself. Every shim is transport-level: it
hooks the HTTP client layer, not the SDK, so it survives SDK version bumps
and covers every SDK built on that transport. Full record schema in
[SCHEMA.md](SCHEMA.md).

## Coverage matrix

| Language | Hook | Covers |
| --- | --- | --- |
| Python | httpx client send + requests session send | openai, anthropic, google-genai, litellm, and every requests/httpx-based client |
| Node / TS | globalThis.fetch | openai v4+, @anthropic-ai/sdk, @google/generative-ai, langchain, every fetch-based client |
| Go | http.RoundTripper (+ Attach over DefaultTransport) | every Go model SDK (all sit on net/http) |
| Rust | reqwest-middleware layer | reqwest-based SDKs (async-openai, anthropic-sdk, rig) |
| Java / Kotlin | OkHttp Interceptor | openai-java, anthropic-java, langchain4j (OkHttp-based) |
| .NET | HttpClient DelegatingHandler | OpenAI .NET, Anthropic SDK, Semantic Kernel (HttpClient-based) |
| Ruby | Faraday middleware + Net::HTTP patch | ruby-openai, anthropic gem, stdlib clients |
| PHP | Guzzle middleware | openai-php/client and Guzzle-based clients |

Not covered: SDKs that bypass these transports (rare; e.g. gRPC-based
clients, raw sockets). For those, the tool and file boundaries still record
what the agent did, and the optional network metadata tap shows when and
where each call went out.

## Environment (all shims)

- `DASHCAM_INGEST_URL` - daemon door, default `http://127.0.0.1:8123/ingest`
- `DASHCAM_HOSTS` - comma-separated extra hosts to record
- `DASHCAM_AGENT` - agent name stamped on recorded hops (multi-agent runs)

## Usage

Python (httpx and requests, sync and async):

```python
import dashcam_shim; dashcam_shim.attach()
# then import and use openai / anthropic / google-genai as usual
```

Node:

```js
require("dashcam-shim").attach();
```

Go:

```go
import dashcamshim "github.com/dashcam/shims/go/dashcamshim"
func main() { dashcamshim.Attach(); ... }
```

Rust:

```rust
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
let client: ClientWithMiddleware = ClientBuilder::new(reqwest::Client::new())
    .with(dashcam_shim::DashcamMiddleware::new())
    .build();
```

Java (OkHttp):

```java
client = new OkHttpClient.Builder().addInterceptor(new ai.dashcam.shim.DashcamInterceptor()).build();
```

.NET:

```csharp
var client = new HttpClient(new DashcamShim.DashcamHandler { InnerHandler = new SocketsHttpHandler() });
```

Ruby (stdlib always; Faraday gems):

```ruby
require "dashcam_shim"
DashcamShim.attach
# Faraday-based: f.use DashcamShim::FaradayMiddleware
```

PHP (Guzzle):

```php
$stack = \GuzzleHttp\HandlerStack::create(new \Dashcam\DashcamMiddleware());
$client = new \GuzzleHttp\Client(["handler" => $stack]);
```

## Testing status

- Python shim: live-tested end to end against a running daemon.
- Rust shim: compiles (`cargo check`), design mirrors the tested Python path.
- Node, Go, Java, .NET, Ruby, PHP: code complete against the shared schema;
  no runtime on the build phone to execute them. Run one live pass per
  language when a matching machine is available.
