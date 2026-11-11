//! dashcam-shim: record model API traffic from any reqwest-based Rust process.
//!
//! Transport level: a `reqwest-middleware` layer that copies every request to
//! a model provider host into the local dashcam daemon /ingest door. A
//! recording failure never breaks the wrapped call.
//!
//! ```no_run
//! use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
//!
//! let client: ClientWithMiddleware = ClientBuilder::new(reqwest::Client::new())
//!     .with(dashcam_shim::DashcamMiddleware::new())
//!     .build();
//! ```
//!
//! Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
//! DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.

use reqwest::header::CONTENT_TYPE;
use reqwest_middleware::{Error, Middleware, Next, Request, Response};

const DEFAULT_HOSTS: &[&str] = &[
    "api.openai.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
    "aiplatform.googleapis.com",
    "integrate.api.nvidia.com",
    "openrouter.ai",
    "api.together.xyz",
    "api.together.ai",
    "api.mistral.ai",
    "api.deepseek.com",
    "api.groq.com",
    "api.x.ai",
    "api.cohere.com",
    "api.fireworks.ai",
    "api.perplexity.ai",
    "api.deepinfra.com",
    "api.moonshot.ai",
    "api.moonshot.cn",
    "api.cerebras.ai",
    "api.ai21.com",
    "api.minimax.chat",
    "api.lambdalabs.com",
    "dashscope.aliyuncs.com",
    "api.stability.ai",
];

#[derive(Clone)]
pub struct DashcamMiddleware;

impl DashcamMiddleware {
    pub fn new() -> Self {
        DashcamMiddleware
    }

    fn matches(&self, url: &reqwest::Url) -> bool {
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        if host.is_empty() || host == "127.0.0.1" || host == "localhost" || host == "::1" {
            return false;
        }
        let extra: Vec<String> = std::env::var("DASHCAM_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        DEFAULT_HOSTS
            .iter()
            .map(|s| s.to_string())
            .chain(extra)
            .any(|x| host == x || host.ends_with(&format!(".{x}")))
    }

    fn provider(host: &str) -> &'static str {
        if host.contains("anthropic.com") {
            "anthropic"
        } else if host.contains("googleapis.com") {
            "gemini"
        } else {
            "openai"
        }
    }

    fn post(&self, payload: serde_json::Value) {
        // Best effort: a dead daemon or a bad door never breaks the caller.
        let url = std::env::var("DASHCAM_INGEST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8123/ingest".to_string());
        let body = payload.to_string();
        std::thread::spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
            {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = client
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .send();
        });
    }
}

impl Default for DashcamMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Middleware for DashcamMiddleware {
    async fn run(
        &self,
        req: Request,
        extensions: &mut http::Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        let url = req.url().clone();
        let method = req.method().to_string();
        let mut path = url.path().trim_start_matches('/').to_string();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let req_body = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "(dashcam: unrecordable request body)".to_string());
        let agent = std::env::var("DASHCAM_AGENT").unwrap_or_default();

        let res = next.run(req, extensions).await;
        if !self.matches(&url) {
            return res;
        }
        let provider = Self::provider(&host);
        match res {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let ct = resp
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let streamed = ct.contains("text/event-stream");
                let bytes = resp.bytes().await?;
                let text = String::from_utf8_lossy(&bytes).into_owned();
                self.post(serde_json::json!({
                    "provider": provider,
                    "method": method,
                    "path": path,
                    "status": status,
                    "request_body": req_body,
                    "response_body": text,
                    "streamed": streamed,
                    "agent": agent,
                }));
                let mut builder = http::Response::builder().status(status);
                for (name, value) in resp.headers() {
                    builder = builder.header(name, value);
                }
                let rebuilt = builder
                    .body(reqwest::Body::from(bytes))
                    .map_err(Error::middleware)?;
                Ok(Response::new(rebuilt))
            }
            Err(e) => {
                self.post(serde_json::json!({
                    "provider": provider,
                    "method": method,
                    "path": path,
                    "status": 0,
                    "request_body": req_body,
                    "response_body": format!("dashcam: request failed: {e}"),
                    "streamed": false,
                    "agent": agent,
                }));
                Err(e)
            }
        }
    }
}
