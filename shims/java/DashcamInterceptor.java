package ai.dashcam.shim;

import java.io.IOException;
import java.net.HttpURLConnection;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.Set;
import java.util.HashSet;
import okhttp3.Interceptor;
import okhttp3.MediaType;
import okhttp3.Request;
import okhttp3.Response;
import okio.Buffer;

/**
 * dashcam-shim for the JVM: an OkHttp Interceptor that copies every request
 * to a model provider host into the local dashcam daemon /ingest door.
 * A recording failure never breaks the wrapped call.
 *
 *   client = new OkHttpClient.Builder()
 *       .addInterceptor(new DashcamInterceptor())
 *       .build();
 *
 * Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
 * DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.
 */
public class DashcamInterceptor implements Interceptor {

    private static final Set<String> DEFAULT_HOSTS = Set.of(
        "api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com",
        "aiplatform.googleapis.com", "integrate.api.nvidia.com", "openrouter.ai",
        "api.together.xyz", "api.together.ai", "api.mistral.ai", "api.deepseek.com",
        "api.groq.com", "api.x.ai", "api.cohere.com", "api.fireworks.ai",
        "api.perplexity.ai", "api.deepinfra.com", "api.moonshot.ai", "api.moonshot.cn",
        "api.cerebras.ai", "api.ai21.com", "api.minimax.chat", "api.lambdalabs.com",
        "dashscope.aliyuncs.com", "api.stability.ai"
    );

    private static final ExecutorService SENDER = Executors.newSingleThreadExecutor(r -> {
        Thread t = new Thread(r, "dashcam-shim");
        t.setDaemon(true);
        return t;
    });

    private final Set<String> extraHosts = parseEnvHosts();

    @Override
    public Response intercept(Chain chain) throws IOException {
        Request request = chain.request();
        String url = request.url().toString();
        String reqBody = peekRequest(request);
        Response response;
        try {
            response = chain.proceed(request);
        } catch (IOException e) {
            record(url, 0, reqBody, "dashcam: request failed: " + e, false);
            throw e;
        }
        if (matches(url)) {
            String respBody;
            try {
                respBody = response.peekBody(32L * 1024 * 1024).string();
            } catch (IOException e) {
                respBody = "(dashcam: unreadable response body)";
            }
            boolean streamed = String.valueOf(response.header("Content-Type"))
                    .contains("text/event-stream");
            record(url, response.code(), reqBody, respBody, streamed);
        }
        return response;
    }

    private void record(String url, int status, String reqBody, String respBody, boolean streamed) {
        if (!matches(url)) {
            return;
        }
        String agent = System.getenv("DASHCAM_AGENT") == null ? "" : System.getenv("DASHCAM_AGENT");
        String path = URI.create(url).getRawPath() == null ? "" : URI.create(url).getRawPath();
        String json = "{"
            + "\"provider\":\"" + esc(provider(host(url))) + "\","
            + "\"method\":\"POST\","
            + "\"path\":\"" + esc(path.replaceFirst("^/", "")) + "\","
            + "\"status\":" + status + ","
            + "\"request_body\":\"" + esc(reqBody) + "\","
            + "\"response_body\":\"" + esc(respBody) + "\","
            + "\"streamed\":" + streamed + ","
            + "\"agent\":\"" + esc(agent) + "\"}";
        SENDER.execute(() -> {
            try {
                HttpURLConnection conn = (HttpURLConnection) URI.create(
                        env("DASHCAM_INGEST_URL", "http://127.0.0.1:8123/ingest")).toURL().openConnection();
                conn.setConnectTimeout(2000);
                conn.setReadTimeout(5000);
                conn.setRequestMethod("POST");
                conn.setDoOutput(true);
                conn.setRequestProperty("Content-Type", "application/json");
                conn.getOutputStream().write(json.getBytes(StandardCharsets.UTF_8));
                conn.getResponseCode();
                conn.getInputStream().close();
            } catch (Exception ignored) {
            }
        });
    }

    private String peekRequest(Request request) {
        if (request.body() == null) {
            return "";
        }
        try {
            Buffer buf = new Buffer();
            request.body().writeTo(buf);
            return buf.readUtf8();
        } catch (Exception e) {
            return "(dashcam: unrecordable request body)";
        }
    }

    private boolean matches(String url) {
        String h = host(url);
        if (h.isEmpty() || h.equals("127.0.0.1") || h.equals("localhost") || h.equals("::1")) {
            return false;
        }
        if (h.startsWith("[") && h.endsWith("]")) {
            return false;
        }
        for (String x : DEFAULT_HOSTS) {
            if (h.equals(x) || h.endsWith("." + x)) {
                return true;
            }
        }
        for (String x : extraHosts) {
            if (h.equals(x) || h.endsWith("." + x)) {
                return true;
            }
        }
        return false;
    }

    private String provider(String host) {
        if (host.contains("anthropic.com")) return "anthropic";
        if (host.contains("googleapis.com")) return "gemini";
        return "openai";
    }

    private String host(String url) {
        try {
            return URI.create(url).getHost() == null ? "" : URI.create(url).getHost().toLowerCase();
        } catch (Exception e) {
            return "";
        }
    }

    private static String env(String name, String def) {
        String v = System.getenv(name);
        return v == null || v.isEmpty() ? def : v;
    }

    private static Set<String> parseEnvHosts() {
        Set<String> out = new HashSet<>();
        String raw = System.getenv("DASHCAM_HOSTS");
        if (raw != null) {
            for (String h : raw.split(",")) {
                h = h.trim().toLowerCase();
                if (!h.isEmpty()) out.add(h);
            }
        }
        return out;
    }

    private static String esc(String s) {
        if (s == null) return "";
        StringBuilder b = new StringBuilder(s.length() + 8);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"': b.append("\\\""); break;
                case '\\': b.append("\\\\"); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                case '\t': b.append("\\t"); break;
                default:
                    if (c < 0x20) b.append(String.format("\\u%04x", (int) c));
                    else b.append(c);
            }
        }
        return b.toString();
    }
}
