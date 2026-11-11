<?php
// dashcam-shim for PHP: a Guzzle middleware that copies every request to a
// model provider host into the local dashcam daemon /ingest door. A recording
// failure never breaks the wrapped call.
//
//   use Dashcam\DashcamMiddleware;
//   $client = new \GuzzleHttp\Client(['handler' => \GuzzleHttp\HandlerStack::create(new DashcamMiddleware())]);
//
// Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
// DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.

namespace Dashcam;

use Psr\Http\Message\RequestInterface;
use Psr\Http\Message\ResponseInterface;

class DashcamMiddleware
{
    public const DEFAULT_HOSTS = [
        'api.openai.com', 'api.anthropic.com', 'generativelanguage.googleapis.com',
        'aiplatform.googleapis.com', 'integrate.api.nvidia.com', 'openrouter.ai',
        'api.together.xyz', 'api.together.ai', 'api.mistral.ai', 'api.deepseek.com',
        'api.groq.com', 'api.x.ai', 'api.cohere.com', 'api.fireworks.ai',
        'api.perplexity.ai', 'api.deepinfra.com', 'api.moonshot.ai', 'api.moonshot.cn',
        'api.cerebras.ai', 'api.ai21.com', 'api.minimax.chat', 'api.lambdalabs.com',
        'dashscope.aliyuncs.com', 'api.stability.ai',
    ];

    public function __invoke(callable $handler)
    {
        return function (RequestInterface $request, array $options) use ($handler) {
            $reqBody = (string) $request->getBody();
            if ($request->getBody()->isSeekable()) {
                $request->getBody()->rewind();
            }
            $url = (string) $request->getUri();

            return $handler($request, $options)->then(
                function (ResponseInterface $response) use ($url, $reqBody) {
                    try {
                        $stream = $response->getBody();
                        $respBody = (string) $stream;
                        if ($stream->isSeekable()) {
                            $stream->rewind();
                        }
                        if (self::matches($url)) {
                            self::record(
                                $url,
                                $response->getStatusCode(),
                                $reqBody,
                                $respBody,
                                str_contains(strtolower($response->getHeaderLine('Content-Type')), 'text/event-stream')
                            );
                        }
                    } catch (\Throwable $e) {
                        // never break the wrapped call
                    }
                    return $response;
                }
            );
        };
    }

    private static function matches(string $url): bool
    {
        $host = strtolower(parse_url($url, PHP_URL_HOST) ?? '');
        if ($host === '' || in_array($host, ['127.0.0.1', 'localhost', '::1', '0.0.0.0'], true)) {
            return false;
        }
        $extra = array_filter(array_map('trim', explode(',', (string) getenv('DASHCAM_HOSTS'))));
        foreach (array_merge(self::DEFAULT_HOSTS, $extra) as $x) {
            if ($host === $x || str_ends_with($host, '.' . $x)) {
                return true;
            }
        }
        return false;
    }

    private static function provider(string $host): string
    {
        if (str_contains($host, 'anthropic.com')) {
            return 'anthropic';
        }
        if (str_contains($host, 'googleapis.com')) {
            return 'gemini';
        }
        return 'openai';
    }

    private static function record(string $url, int $status, string $reqBody, string $respBody, bool $streamed): void
    {
        $payload = json_encode([
            'provider' => self::provider(strtolower(parse_url($url, PHP_URL_HOST) ?? '')),
            'method' => 'POST',
            'path' => ltrim(parse_url($url, PHP_URL_PATH) ?? '', '/') . ((parse_url($url, PHP_URL_QUERY) ?? '') !== '' ? '?' . parse_url($url, PHP_URL_QUERY) : ''),
            'status' => $status,
            'request_body' => $reqBody,
            'response_body' => $respBody,
            'streamed' => $streamed,
            'agent' => (string) getenv('DASHCAM_AGENT'),
        ]);
        $target = getenv('DASHCAM_INGEST_URL') ?: 'http://127.0.0.1:8123/ingest';
        $ctx = stream_context_create(['http' => [
            'method' => 'POST',
            'header' => "Content-Type: application/json\r\n",
            'content' => $payload,
            'timeout' => 5,
            'ignore_errors' => true,
        ]]);
        @file_get_contents($target, false, $ctx);
    }
}
