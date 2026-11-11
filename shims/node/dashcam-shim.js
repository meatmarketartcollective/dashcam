"use strict";
// dashcam-shim: record model API traffic from any Node process.
// Transport level: patches globalThis.fetch, the transport under the openai,
// @anthropic-ai/sdk and @google/generative-ai clients. A recording failure
// never breaks the wrapped call.
//
//   require("dashcam-shim").attach();
//
// Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
//      DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.
const http = require("http");

const DEFAULT_HOSTS = [
  "api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com",
  "aiplatform.googleapis.com", "integrate.api.nvidia.com", "openrouter.ai",
  "api.together.xyz", "api.together.ai", "api.mistral.ai", "api.deepseek.com",
  "api.groq.com", "api.x.ai", "api.cohere.com", "api.fireworks.ai",
  "api.perplexity.ai", "api.deepinfra.com", "api.moonshot.ai", "api.moonshot.cn",
  "api.cerebras.ai", "api.ai21.com", "api.minimax.chat", "api.lambdalabs.com",
  "dashscope.aliyuncs.com", "api.stability.ai",
];
const LOOPBACK = new Set(["127.0.0.1", "localhost", "::1", "0.0.0.0"]);

function hosts() {
  const extra = (process.env.DASHCAM_HOSTS || "")
    .split(",").map((s) => s.trim()).filter(Boolean);
  return new Set(DEFAULT_HOSTS.concat(extra));
}

function hostOf(u) {
  try { return new URL(u).hostname.toLowerCase(); } catch (e) { return ""; }
}

function matches(u) {
  const h = hostOf(u);
  if (!h || LOOPBACK.has(h)) return false;
  for (const x of hosts()) if (h === x || h.endsWith("." + x)) return true;
  return false;
}

function provider(host) {
  if (host.includes("anthropic.com")) return "anthropic";
  if (host.includes("googleapis.com")) return "gemini";
  return "openai";
}

function post(payload) {
  try {
    const t = new URL(process.env.DASHCAM_INGEST_URL || "http://127.0.0.1:8123/ingest");
    const req = http.request(
      { hostname: t.hostname, port: t.port, path: t.pathname, method: "POST",
        headers: { "content-type": "application/json" } },
      (res) => res.resume()
    );
    req.on("error", () => {});
    req.end(JSON.stringify(payload));
  } catch (e) {}
}

function attach() {
  if (attach._done) return;
  attach._done = true;
  const orig = globalThis.fetch;
  if (typeof orig !== "function") return;
  globalThis.fetch = async function dashcamFetch(input, init) {
    let reqUrl = "";
    let reqBody = "";
    let method = "GET";
    try {
      if (typeof input === "string" || input instanceof URL) {
        reqUrl = String(input);
        method = (init && init.method) || "GET";
        if (init && typeof init.body === "string") reqBody = init.body;
        else if (init && init.body) reqBody = "(dashcam: non-text request body)";
      } else if (input && typeof input.clone === "function") {
        reqUrl = input.url;
        method = input.method || "GET";
        const c = input.clone();
        try { reqBody = await c.text(); }
        catch (e) { reqBody = "(dashcam: unreadable request body)"; }
      }
    } catch (e) {}
    const res = await orig(input, init);
    try {
      if (matches(reqUrl)) {
        const u = new URL(reqUrl);
        const streamed = (res.headers.get("content-type") || "").includes("text/event-stream");
        const text = await res.clone().text();
        post({
          provider: provider(hostOf(reqUrl)),
          method: method,
          path: u.pathname + (u.search || ""),
          status: res.status,
          request_body: reqBody,
          response_body: text,
          streamed: streamed,
          agent: process.env.DASHCAM_AGENT || "",
        });
      }
    } catch (e) {}
    return res;
  };
}

module.exports = { attach };
