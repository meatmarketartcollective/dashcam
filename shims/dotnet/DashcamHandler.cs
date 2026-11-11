// dashcam-shim for .NET: an HttpClient DelegatingHandler that copies every
// request to a model provider host into the local dashcam daemon /ingest
// door. A recording failure never breaks the wrapped call.
//
//   var client = HttpClientFactory.Create()
//       .AddHttpMessageHandler<DashcamHandler>();
//   // or: new HttpClient(new DashcamHandler { InnerHandler = new SocketsHttpHandler() });
//
// Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
// DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace DashcamShim
{
    public class DashcamHandler : DelegatingHandler
    {
        private static readonly string[] DefaultHosts =
        {
            "api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com",
            "aiplatform.googleapis.com", "integrate.api.nvidia.com", "openrouter.ai",
            "api.together.xyz", "api.together.ai", "api.mistral.ai", "api.deepseek.com",
            "api.groq.com", "api.x.ai", "api.cohere.com", "api.fireworks.ai",
            "api.perplexity.ai", "api.deepinfra.com", "api.moonshot.ai", "api.moonshot.cn",
            "api.cerebras.ai", "api.ai21.com", "api.minimax.chat", "api.lambdalabs.com",
            "dashscope.aliyuncs.com", "api.stability.ai"
        };

        protected override async Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var url = request.RequestUri?.ToString() ?? "";
            var reqBody = "";
            if (request.Content != null)
            {
                var bytes = await request.Content.ReadAsByteArrayAsync(cancellationToken);
                reqBody = Encoding.UTF8.GetString(bytes);
                var replacement = new ByteArrayContent(bytes);
                foreach (var h in request.Content.Headers)
                    replacement.Headers.TryAddWithoutValidation(h.Key, h.Value);
                request.Content = replacement;
            }

            HttpResponseMessage response;
            try
            {
                response = await base.SendAsync(request, cancellationToken);
            }
            catch (Exception e)
            {
                Record(url, 0, reqBody, "dashcam: request failed: " + e.Message, false);
                throw;
            }

            if (Matches(url))
            {
                var respBytes = await response.Content.ReadAsByteArrayAsync(cancellationToken);
                var respBody = Encoding.UTF8.GetString(respBytes);
                var streamed = (response.Content?.Headers?.ContentType?.ToString() ?? "")
                    .Contains("text/event-stream", StringComparison.OrdinalIgnoreCase);
                Record(url, (int)response.StatusCode, reqBody, respBody, streamed);
                var replacement = new ByteArrayContent(respBytes);
                if (response.Content != null)
                    foreach (var h in response.Content.Headers)
                        replacement.Headers.TryAddWithoutValidation(h.Key, h.Value);
                response.Content = replacement;
            }
            return response;
        }

        private void Record(string url, int status, string reqBody, string respBody, bool streamed)
        {
            if (!Matches(url)) return;
            var payload = JsonSerializer.Serialize(new Dictionary<string, object?>
            {
                ["provider"] = Provider(Host(url)),
                ["method"] = "POST",
                ["path"] = new Uri(url).AbsolutePath.TrimStart('/'),
                ["status"] = status,
                ["request_body"] = reqBody,
                ["response_body"] = respBody,
                ["streamed"] = streamed,
                ["agent"] = Environment.GetEnvironmentVariable("DASHCAM_AGENT") ?? ""
            });
            var sender = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
            _ = sender.PostAsync(
                Environment.GetEnvironmentVariable("DASHCAM_INGEST_URL") ?? "http://127.0.0.1:8123/ingest",
                new StringContent(payload, Encoding.UTF8, "application/json"));
        }

        private bool Matches(string url)
        {
            var h = Host(url);
            if (h.Length == 0 || h == "127.0.0.1" || h == "localhost" || h == "::1") return false;
            var extra = (Environment.GetEnvironmentVariable("DASHCAM_HOSTS") ?? "")
                .Split(',').Select(s => s.Trim().ToLowerInvariant()).Where(s => s.Length > 0);
            return DefaultHosts.Concat(extra)
                .Any(x => h == x || h.EndsWith("." + x, StringComparison.Ordinal));
        }

        private string Provider(string host) =>
            host.Contains("anthropic.com") ? "anthropic" :
            host.Contains("googleapis.com") ? "gemini" : "openai";

        private string Host(string url)
        {
            try { return new Uri(url).Host.ToLowerInvariant(); }
            catch { return ""; }
        }
    }
}
