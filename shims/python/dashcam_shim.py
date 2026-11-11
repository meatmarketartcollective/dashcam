"""dashcam_shim: record model API traffic from inside any Python process.

Transport level: patches httpx and requests so every request to a model
provider host is copied to the local dashcam daemon /ingest door. No SDK
imports, no vendor code touched, and a recording failure never breaks the
wrapped call.

    import dashcam_shim; dashcam_shim.attach()

Env:
    DASHCAM_INGEST_URL  daemon door, default http://127.0.0.1:8123/ingest
    DASHCAM_HOSTS       comma-separated extra hosts to record
    DASHCAM_AGENT       agent name stamped on recorded hops
"""

import json
import os
import urllib.request
from urllib.parse import urlparse

_attached = False

DEFAULT_HOSTS = (
    "api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com",
    "aiplatform.googleapis.com", "integrate.api.nvidia.com", "openrouter.ai",
    "api.together.xyz", "api.together.ai", "api.mistral.ai", "api.deepseek.com",
    "api.groq.com", "api.x.ai", "api.cohere.com", "api.fireworks.ai",
    "api.perplexity.ai", "api.deepinfra.com", "api.moonshot.ai", "api.moonshot.cn",
    "api.cerebras.ai", "api.ai21.com", "api.minimax.chat", "api.lambdalabs.com",
    "dashscope.aliyuncs.com", "api.stability.ai",
)

_loopback = {"127.0.0.1", "localhost", "::1", "0.0.0.0"}


def _hosts():
    extra = [h.strip() for h in os.environ.get("DASHCAM_HOSTS", "").split(",") if h.strip()]
    return set(DEFAULT_HOSTS) | set(extra)


def _host(url):
    try:
        return (urlparse(url).hostname or "").lower()
    except Exception:
        return ""


def _matches(url):
    h = _host(url)
    if not h or h in _loopback:
        return False
    return any(h == x or h.endswith("." + x) for x in _hosts())


def _provider(host):
    if "anthropic.com" in host:
        return "anthropic"
    if "googleapis.com" in host:
        return "gemini"
    return "openai"


def _post(payload):
    url = os.environ.get("DASHCAM_INGEST_URL", "http://127.0.0.1:8123/ingest")
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req, timeout=5).read()


def _record(method, url, status, request_body, response_body, streamed):
    if not _matches(url):
        return
    host = _host(url)
    try:
        u = urlparse(url)
        _post({
            "provider": _provider(host),
            "method": method,
            "path": u.path.lstrip("/") + (("?" + u.query) if u.query else ""),
            "status": status,
            "request_body": request_body if isinstance(request_body, str)
                else "(dashcam: binary request body)",
            "response_body": response_body if isinstance(response_body, str) else "",
            "streamed": bool(streamed),
            "agent": os.environ.get("DASHCAM_AGENT", ""),
        })
    except Exception:
        pass


def _request_text(request):
    try:
        return request.read().decode("utf-8", "replace")
    except Exception:
        return "(dashcam: unreadable request body)"


async def _request_text_async(request):
    try:
        return (await request.aread()).decode("utf-8", "replace")
    except Exception:
        return _request_text(request)


def attach():
    global _attached
    if _attached:
        return
    _attached = True

    # httpx: the transport under openai, anthropic, google-genai and litellm
    try:
        import httpx

        _orig_send = httpx.Client.send

        def send(client, request, **kwargs):
            body = _request_text(request)
            try:
                resp = _orig_send(client, request, **kwargs)
            except Exception as exc:
                _record(request.method, str(request.url), 0, body,
                        "dashcam: request failed: %s" % exc, False)
                raise
            streamed = "text/event-stream" in resp.headers.get("content-type", "")
            try:
                resp.read()
            except Exception:
                pass
            _record(request.method, str(request.url), resp.status_code,
                    body, resp.text, streamed)
            return resp

        httpx.Client.send = send

        _orig_asend = httpx.AsyncClient.send

        async def asend(client, request, **kwargs):
            body = await _request_text_async(request)
            try:
                resp = await _orig_asend(client, request, **kwargs)
            except Exception as exc:
                _record(request.method, str(request.url), 0, body,
                        "dashcam: request failed: %s" % exc, False)
                raise
            streamed = "text/event-stream" in resp.headers.get("content-type", "")
            try:
                await resp.aread()
            except Exception:
                pass
            _record(request.method, str(request.url), resp.status_code,
                    body, resp.text, streamed)
            return resp

        httpx.AsyncClient.send = asend
    except ImportError:
        pass

    # requests: the transport under older SDKs and custom clients
    try:
        import requests.sessions as _rs

        _orig_req_send = _rs.Session.send

        def req_send(session, request, **kwargs):
            body = request.body if isinstance(request.body, (str, bytes)) \
                else "(dashcam: unrecordable request body)"
            try:
                resp = _orig_req_send(session, request, **kwargs)
            except Exception as exc:
                _record(request.method, request.url, 0, body,
                        "dashcam: request failed: %s" % exc, False)
                raise
            streamed = "text/event-stream" in resp.headers.get("Content-Type", "")
            _record(request.method, request.url, resp.status_code,
                    body, resp.text, streamed)
            return resp

        _rs.Session.send = req_send
    except ImportError:
        pass
