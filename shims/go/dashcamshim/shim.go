// Package dashcamshim records model API traffic from any Go process.
//
// Transport level: wrap any http.RoundTripper (or install over
// http.DefaultTransport with Attach) and every request to a model provider
// host is copied to the local dashcam daemon /ingest door. A recording
// failure never breaks the wrapped call.
//
//	dashcamshim.Attach()
//
// Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
// DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.
package dashcamshim

import (
	"bytes"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"time"
)

var defaultHosts = []string{
	"api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com",
	"aiplatform.googleapis.com", "integrate.api.nvidia.com", "openrouter.ai",
	"api.together.xyz", "api.together.ai", "api.mistral.ai", "api.deepseek.com",
	"api.groq.com", "api.x.ai", "api.cohere.com", "api.fireworks.ai",
	"api.perplexity.ai", "api.deepinfra.com", "api.moonshot.ai", "api.moonshot.cn",
	"api.cerebras.ai", "api.ai21.com", "api.minimax.chat", "api.lambdalabs.com",
	"dashscope.aliyuncs.com", "api.stability.ai",
}

var hostsOnce sync.Once
var extraHosts []string

func env(name, def string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return def
}

func allowed(host string) bool {
	host = strings.ToLower(host)
	if i := strings.Index(host, ":"); i >= 0 {
		host = host[:i]
	}
	if host == "" || host == "127.0.0.1" || host == "localhost" || host == "::1" {
		return false
	}
	hostsOnce.Do(func() {
		for _, h := range strings.Split(os.Getenv("DASHCAM_HOSTS"), ",") {
			h = strings.TrimSpace(h)
			if h != "" {
				extraHosts = append(extraHosts, h)
			}
		}
	})
	for _, x := range append(append([]string{}, defaultHosts...), extraHosts...) {
		if host == x || strings.HasSuffix(host, "."+x) {
			return true
		}
	}
	return false
}

func provider(host string) string {
	if strings.Contains(host, "anthropic.com") {
		return "anthropic"
	}
	if strings.Contains(host, "googleapis.com") {
		return "gemini"
	}
	return "openai"
}

// postAsync forwards one record to the daemon door without ever blocking or
// failing the wrapped call.
func postAsync(payload map[string]any) {
	go func() {
		defer func() { recover() }()
		body, err := jsonMarshal(payload)
		if err != nil {
			return
		}
		client := &http.Client{Timeout: 5 * time.Second}
		req, err := http.NewRequest("POST", env("DASHCAM_INGEST_URL", "http://127.0.0.1:8123/ingest"), bytes.NewReader(body))
		if err != nil {
			return
		}
		req.Header.Set("Content-Type", "application/json")
		resp, err := client.Do(req)
		if err != nil {
			return
		}
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
	}()
}

// Transport records every matching exchange it forwards.
type Transport struct {
	Base http.RoundTripper
}

func (t *Transport) RoundTrip(req *http.Request) (*http.Response, error) {
	base := t.Base
	if base == nil {
		base = http.DefaultTransport
	}
	var reqBody []byte
	if req.Body != nil {
		reqBody, _ = io.ReadAll(req.Body)
		req.Body.Close()
		req.Body = io.NopCloser(bytes.NewReader(reqBody))
	}
	resp, err := base.RoundTrip(req)
	if !allowed(req.URL.Host) {
		return resp, err
	}
	if err != nil {
		postAsync(recordPayload(req, 0, string(reqBody), "dashcam: request failed: "+err.Error(), false))
		return resp, err
	}
	respBody, rerr := io.ReadAll(resp.Body)
	resp.Body.Close()
	if rerr == nil {
		resp.Body = io.NopCloser(bytes.NewReader(respBody))
	}
	streamed := strings.Contains(resp.Header.Get("Content-Type"), "text/event-stream")
	postAsync(recordPayload(req, resp.StatusCode, string(reqBody), string(respBody), streamed))
	return resp, nil
}

func recordPayload(req *http.Request, status int, reqBody, respBody string, streamed bool) map[string]any {
	path := req.URL.Path
	if req.URL.RawQuery != "" {
		path += "?" + req.URL.RawQuery
	}
	agent := os.Getenv("DASHCAM_AGENT")
	return map[string]any{
		"provider":      provider(strings.ToLower(req.URL.Host)),
		"method":        req.Method,
		"path":          strings.TrimPrefix(path, "/"),
		"status":        status,
		"request_body":  reqBody,
		"response_body": respBody,
		"streamed":      streamed,
		"agent":         agent,
	}
}

// Attach installs the recording transport over http.DefaultTransport.
func Attach() {
	http.DefaultTransport = &Transport{Base: http.DefaultTransport}
}

// jsonMarshal keeps the shim free of any encoding/json import in user code
// paths that shadow it; here it is a plain alias.
func jsonMarshal(v any) ([]byte, error) {
	return jsonEncode(v)
}
