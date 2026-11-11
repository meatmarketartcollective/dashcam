# frozen_string_literal: true

# dashcam-shim for Ruby: record model API traffic into the local dashcam
# daemon /ingest door. Two layers, use whichever fits the client:
#
#   require "dashcam_shim"
#   DashcamShim.attach                      # patches Net::HTTP (stdlib)
#   # and/or for Faraday-based clients (ruby-openai, anthropic gems):
#   conn = Faraday.new do |f|
#     f.use DashcamShim::FaradayMiddleware
#     f.adapter Faraday.default_adapter
#   end
#
# A recording failure never breaks the wrapped call.
#
# Env: DASHCAM_INGEST_URL (default http://127.0.0.1:8123/ingest),
# DASHCAM_HOSTS (comma-separated extras), DASHCAM_AGENT.
require "json"
require "net/http"
require "uri"

module DashcamShim
  DEFAULT_HOSTS = %w[
    api.openai.com api.anthropic.com generativelanguage.googleapis.com
    aiplatform.googleapis.com integrate.api.nvidia.com openrouter.ai
    api.together.xyz api.together.ai api.mistral.ai api.deepseek.com
    api.groq.com api.x.ai api.cohere.com api.fireworks.ai
    api.perplexity.ai api.deepinfra.com api.moonshot.ai api.moonshot.cn
    api.cerebras.ai api.ai21.com api.minimax.chat api.lambdalabs.com
    dashscope.aliyuncs.com api.stability.ai
  ].freeze

  LOOPBACK = %w[127.0.0.1 localhost ::1 0.0.0.0].freeze

  module_function

  def extra_hosts
    @extra_hosts ||= ENV["DASHCAM_HOSTS"].to_s.split(",").map(&:strip).reject(&:empty?)
  end

  def matches(url)
    host = URI(url).host.to_s.downcase
    return false if host.empty? || LOOPBACK.include?(host)
    (DEFAULT_HOSTS + extra_hosts).any? { |x| host == x || host.end_with?(".#{x}") }
  end

  def provider(host)
    return "anthropic" if host.include?("anthropic.com")
    return "gemini" if host.include?("googleapis.com")

    "openai"
  end

  def record(method:, url:, status:, request_body:, response_body:, streamed:)
    return unless matches(url)

    payload = {
      "provider" => provider(URI(url).host.to_s.downcase),
      "method" => method,
      "path" => URI(url).request_uri.to_s.sub(%r{\A/}, ""),
      "status" => status,
      "request_body" => request_body,
      "response_body" => response_body,
      "streamed" => streamed,
      "agent" => ENV["DASHCAM_AGENT"].to_s
    }
    Thread.new do
      begin
        target = URI(ENV.fetch("DASHCAM_INGEST_URL", "http://127.0.0.1:8123/ingest"))
        req = Net::HTTP::Post.new(target)
        req["Content-Type"] = "application/json"
        req.body = JSON.generate(payload)
        Net::HTTP.start(target.host, target.port, use_ssl: target.scheme == "https",
                        open_timeout: 2, read_timeout: 5) { |http| http.request(req) }
      rescue StandardError
        nil
      end
    end
  end

  # Faraday layer for gem clients that use Faraday (ruby-openai, anthropic).
  begin
    require "faraday"
    class FaradayMiddleware < Faraday::Middleware
      def call(env)
        req_body = env[:body].is_a?(String) ? env[:body] : "(dashcam: unrecordable request body)"
        url = env[:url].to_s
        @app.call(env).on_complete do |renv|
          streamed = renv[:response_headers].to_s.downcase.include?("text/event-stream")
          DashcamShim.record(
            method: env[:method].to_s.upcase, url: url, status: renv[:status].to_i,
            request_body: req_body, response_body: renv[:body].to_s, streamed: streamed
          )
        end
      rescue StandardError => e
        DashcamShim.record(method: env[:method].to_s.upcase, url: url, status: 0,
                           request_body: req_body,
                           response_body: "dashcam: request failed: #{e.class}", streamed: false)
        raise
      end
    end
  rescue LoadError
    # Faraday not installed: the Net::HTTP patch still covers stdlib callers.
  end

  # stdlib layer: every Net::HTTP#request gets recorded.
  def attach
    return if @attached

    @attached = true
    Net::HTTP.class_eval do
      alias_method :request_without_dashcam, :request

      def request(req, body = nil, &blk)
        req_body = req.body.to_s
        url = "http#{use_ssl? ? 's' : ''}://#{addr_host}:#{port}#{req.path}"
        begin
          res = request_without_dashcam(req, body, &blk)
        rescue StandardError => e
          DashcamShim.record(method: req.method, url: url, status: 0,
                             request_body: req_body,
                             response_body: "dashcam: request failed: #{e.class}", streamed: false)
          raise
        end
        streamed = res["content-type"].to_s.include?("text/event-stream")
        DashcamShim.record(method: req.method, url: url, status: res.code.to_i,
                           request_body: req_body, response_body: res.body.to_s, streamed: streamed)
        res
      end
    end
  end
end
