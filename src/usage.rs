//! Token usage extracted from recorded model turns.
//!
//! Borrowed in spirit from Spanlens (MIT): every model_turn hop gains a
//! `usage` object with the model name and the prompt/completion token counts
//! the provider reported, parsed out of the same response bodies capture
//! already stores. `dashcam costs` sums them; dollar estimates come only
//! from a price file the user passes, never from a table we guess at.

use serde_json::Value;

/// Token usage parsed from one recorded model turn's response body.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Usage {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Pull token usage out of a recorded model API response.
///
/// Works on the raw bodies the capture proxy stores: a plain JSON response
/// or the raw SSE stream text of a streamed response. The model name falls
/// back to the request body's `model` field when the response omits one.
/// Bodies that parse to nothing yield no usage, never an error: capture must
/// not fail because a provider speaks a dialect we do not know yet.
pub fn usage_from_response(
    provider: &str,
    request_body: &str,
    response_body: &str,
    streamed: bool,
) -> Option<Usage> {
    let mut u = if streamed {
        from_sse(provider, response_body)
    } else {
        from_json(provider, response_body)
    };
    if let Some(u) = u.as_mut() {
        if u.model.is_empty() {
            u.model = request_model(request_body);
        }
    }
    u
}

fn from_json(provider: &str, body: &str) -> Option<Usage> {
    let v = serde_json::from_str::<Value>(body).ok()?;
    match provider {
        "anthropic" => anthropic_json(&v),
        "gemini" => gemini_json(&v),
        _ => openai_json(&v).or_else(|| anthropic_json(&v)).or_else(|| gemini_json(&v)),
    }
}

/// OpenAI shape: top-level `usage` with `prompt_tokens` / `completion_tokens`.
fn openai_json(v: &Value) -> Option<Usage> {
    let usage = v.get("usage")?;
    let prompt = usage.get("prompt_tokens").and_then(|t| t.as_u64())?;
    let completion = usage.get("completion_tokens").and_then(|t| t.as_u64())?;
    Some(Usage {
        model: v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
}

/// Anthropic shape: top-level `usage` with `input_tokens` / `output_tokens`.
fn anthropic_json(v: &Value) -> Option<Usage> {
    let usage = v.get("usage")?;
    let prompt = usage.get("input_tokens").and_then(|t| t.as_u64())?;
    let completion = usage.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    Some(Usage {
        model: v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
}

/// Gemini shape: `usageMetadata` with `promptTokenCount` / `candidatesTokenCount`.
fn gemini_json(v: &Value) -> Option<Usage> {
    let usage = v.get("usageMetadata")?;
    let prompt = usage.get("promptTokenCount").and_then(|t| t.as_u64())?;
    let completion = usage.get("candidatesTokenCount").and_then(|t| t.as_u64()).unwrap_or(0);
    Some(Usage {
        model: v.get("modelVersion").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        prompt_tokens: prompt,
        completion_tokens: completion,
    })
}

/// SSE dialects, one accumulator per provider.
fn from_sse(provider: &str, body: &str) -> Option<Usage> {
    match provider {
        "anthropic" => anthropic_sse(body),
        _ => openai_sse(provider, body),
    }
}

/// OpenAI-compatible and Gemini SSE: usage arrives on a chunk's own field,
/// so the last chunk that carries one wins. OpenAI-compat streams put it on
/// the final chunk only when the client asked for `stream_options`; Gemini
/// repeats `usageMetadata` on every chunk.
fn openai_sse(provider: &str, body: &str) -> Option<Usage> {
    let mut out: Option<Usage> = None;
    for chunk in sse_chunks(body) {
        let parsed = match provider {
            "gemini" => gemini_json(&chunk),
            _ => openai_json(&chunk).or_else(|| gemini_json(&chunk)),
        };
        if let Some(u) = parsed {
            out = Some(u);
        }
    }
    out
}

/// Anthropic SSE: `message_start` carries the model and input tokens,
/// each `message_delta` carries cumulative output tokens, so the last
/// delta wins for completion and the start wins for prompt and model.
fn anthropic_sse(body: &str) -> Option<Usage> {
    let mut model = String::new();
    let mut prompt: Option<u64> = None;
    let mut completion: Option<u64> = None;
    let mut seen = false;
    for chunk in sse_chunks(body) {
        match chunk.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                model = chunk.pointer("/message/model").and_then(|m| m.as_str()).unwrap_or("").to_string();
                prompt = chunk.pointer("/message/usage/input_tokens").and_then(|t| t.as_u64());
                seen = true;
            }
            Some("message_delta") => {
                completion = chunk.pointer("/usage/output_tokens").and_then(|t| t.as_u64());
                seen = true;
            }
            _ => {}
        }
    }
    if !seen {
        return None;
    }
    Some(Usage {
        model,
        prompt_tokens: prompt.unwrap_or(0),
        completion_tokens: completion.unwrap_or(0),
    })
}

/// Parse the `data:` JSON payloads out of an SSE body, skipping `[DONE]`.
fn sse_chunks(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in body.lines() {
        let rest = match line.strip_prefix("data:") {
            Some(r) => r.trim(),
            None => continue,
        };
        if rest.is_empty() || rest == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(rest) {
            out.push(v);
        }
    }
    out
}

/// The request body's `model` field (OpenAI and Anthropic carry one there;
/// Gemini names the model in the URL path instead).
fn request_model(request_body: &str) -> String {
    serde_json::from_str::<Value>(request_body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|m| m.to_string()))
        .unwrap_or_default()
}

/// One row of a user-supplied price file: `model,in_per_mtok,out_per_mtok`.
#[derive(Debug, Clone)]
pub struct PriceRow {
    pub model: String,
    pub in_per_mtok: f64,
    pub out_per_mtok: f64,
}

/// Parse a price CSV: one model per line, dollars per million tokens.
/// Blank lines, `#` comments and a `model,...` header are skipped.
pub fn parse_prices(text: &str) -> Vec<PriceRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("model,") {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(|p| p.trim()).collect();
        if parts.len() < 3 {
            continue;
        }
        if let (Ok(i), Ok(o)) = (parts[1].parse::<f64>(), parts[2].parse::<f64>()) {
            rows.push(PriceRow { model: parts[0].to_string(), in_per_mtok: i, out_per_mtok: o });
        }
    }
    rows
}

/// Price lookup by longest model-prefix match, so `gpt-4o` also covers
/// dated variants like `gpt-4o-2024-08-06`.
pub fn price_of(rows: &[PriceRow], model: &str) -> Option<(f64, f64)> {
    let mut best: Option<&PriceRow> = None;
    for row in rows {
        if model.starts_with(&row.model) {
            let better = match best {
                Some(b) => row.model.len() > b.model.len(),
                None => true,
            };
            if better {
                best = Some(row);
            }
        }
    }
    best.map(|b| (b.in_per_mtok, b.out_per_mtok))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_plain_json() {
        let body = r#"{"model":"gpt-5-mini","usage":{"prompt_tokens":120,"completion_tokens":45,"total_tokens":165},"choices":[]}"#;
        let u = usage_from_response("openai", "", body, false).unwrap();
        assert_eq!(u.model, "gpt-5-mini");
        assert_eq!(u.prompt_tokens, 120);
        assert_eq!(u.completion_tokens, 45);
    }

    #[test]
    fn openai_sse_final_chunk() {
        let body = "data: {\"model\":\"gpt-5-mini\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: {\"model\":\"gpt-5-mini\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3}}\n\n\
                    data: [DONE]\n\n";
        let u = usage_from_response("openai", "", body, true).unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.completion_tokens, 3);
        assert_eq!(u.model, "gpt-5-mini");
    }

    #[test]
    fn anthropic_plain_json() {
        let body = r#"{"model":"claude-sonnet-4-5","usage":{"input_tokens":200,"output_tokens":88}}"#;
        let u = usage_from_response("anthropic", "", body, false).unwrap();
        assert_eq!(u.model, "claude-sonnet-4-5");
        assert_eq!(u.prompt_tokens, 200);
        assert_eq!(u.completion_tokens, 88);
    }

    #[test]
    fn anthropic_sse_accumulates() {
        let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":64}}}\n\n\
                    event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":12}}\n\n\
                    event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":31}}\n\n\
                    event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let u = usage_from_response("anthropic", "", body, true).unwrap();
        assert_eq!(u.model, "claude-sonnet-4-5");
        assert_eq!(u.prompt_tokens, 64);
        assert_eq!(u.completion_tokens, 31);
    }

    #[test]
    fn gemini_plain_and_sse() {
        let body = r#"{"modelVersion":"gemini-2.5-flash","usageMetadata":{"promptTokenCount":300,"candidatesTokenCount":57,"totalTokenCount":357}}"#;
        let u = usage_from_response("gemini", "", body, false).unwrap();
        assert_eq!(u.model, "gemini-2.5-flash");
        assert_eq!(u.prompt_tokens, 300);
        assert_eq!(u.completion_tokens, 57);

        let sse = "data: {\"usageMetadata\":{\"promptTokenCount\":300,\"candidatesTokenCount\":20}}\n\n\
                   data: {\"usageMetadata\":{\"promptTokenCount\":300,\"candidatesTokenCount\":57}}\n\n";
        let u2 = usage_from_response("gemini", "", sse, true).unwrap();
        assert_eq!(u2.completion_tokens, 57);
    }

    #[test]
    fn model_falls_back_to_request() {
        let body = r#"{"usage":{"prompt_tokens":5,"completion_tokens":1}}"#;
        let req = r#"{"model":"mistral-large-latest","messages":[]}"#;
        let u = usage_from_response("openai", req, body, false).unwrap();
        assert_eq!(u.model, "mistral-large-latest");
    }

    #[test]
    fn unknown_body_yields_none() {
        assert!(usage_from_response("openai", "", "not json at all", false).is_none());
        assert!(usage_from_response("openai", "", "{}", false).is_none());
    }

    #[test]
    fn prices_parse_and_prefix_match() {
        let csv = "# approx prices, dollars per 1M tokens\nmodel,in,out\ngpt-5-mini,0.25,2.00\ngpt-5-mini-2026-01,0.30,2.40\n";
        let rows = parse_prices(csv);
        assert_eq!(rows.len(), 2);
        let (i, o) = price_of(&rows, "gpt-5-mini-2026-01").unwrap();
        assert_eq!(i, 0.30);
        assert_eq!(o, 2.40);
        assert!(price_of(&rows, "claude-sonnet-4-5").is_none());
    }
}
