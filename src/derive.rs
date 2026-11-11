use serde_json::Value;

/// One tool call reassembled out of a recorded model turn's response body.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Pull tool calls out of a recorded model API response.
///
/// Works on the raw bodies the capture proxy stores: a plain JSON response
/// (OpenAI-compatible `choices[].message.tool_calls`) or the raw SSE stream
/// text of a streamed response, whose deltas are reassembled in order.
/// Bodies that parse to nothing yield no calls, never an error: capture must
/// not fail because a provider speaks a dialect we do not know yet.
pub fn tool_calls_from_response(body: &str, streamed: bool) -> Vec<DerivedToolCall> {
    if streamed {
        from_sse(body)
    } else {
        from_json(body)
    }
}

fn from_json(body: &str) -> Vec<DerivedToolCall> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return out;
    };
    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
        return out;
    };
    for choice in choices {
        let Some(tcs) = choice
            .pointer("/message/tool_calls")
            .and_then(|t| t.as_array())
        else {
            continue;
        };
        for tc in tcs {
            let name = tc
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = tc
                .pointer("/function/arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            if name.is_empty() && arguments.is_empty() {
                continue;
            }
            out.push(DerivedToolCall { id, name, arguments });
        }
    }
    out
}

fn from_sse(body: &str) -> Vec<DerivedToolCall> {
    // Streaming tool calls arrive as deltas split across many chunks; the
    // provider numbers them by (choice index, tool-call index) and splits the
    // function name and JSON arguments string across chunks. Reassemble both.
    let mut order: Vec<(usize, usize)> = Vec::new();
    let mut acc: std::collections::HashMap<(usize, usize), DerivedToolCall> =
        std::collections::HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
            continue;
        };
        for (ci, choice) in choices.iter().enumerate() {
            let Some(tcs) = choice
                .pointer("/delta/tool_calls")
                .and_then(|t| t.as_array())
            else {
                continue;
            };
            for tc in tcs {
                let ti = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let key = (ci, ti);
                let entry = acc.entry(key).or_insert_with(|| {
                    order.push(key);
                    DerivedToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    }
                });
                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                    if !id.is_empty() {
                        entry.id = id.to_string();
                    }
                }
                if let Some(name) = tc.pointer("/function/name").and_then(|n| n.as_str()) {
                    entry.name.push_str(name);
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(|a| a.as_str()) {
                    entry.arguments.push_str(args);
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| acc.remove(&k))
        .filter(|c| !(c.name.is_empty() && c.arguments.is_empty()))
        .collect()
}

/// Anthropic native `/v1/messages` response: content blocks of type
/// `tool_use` carry `id`, `name` and an `input` JSON *object*. The object is
/// serialized to a string so every dialect stores arguments the same way.
/// Streaming SSE is not parsed yet: a streamed Anthropic turn still records
/// its raw body in full, it just derives no tool-call hops.
pub fn tool_calls_from_anthropic(body: &str, streamed: bool) -> Vec<DerivedToolCall> {
    let mut out = Vec::new();
    if streamed {
        return out;
    }
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return out;
    };
    let Some(content) = v.get("content").and_then(|c| c.as_array()) else {
        return out;
    };
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
        let arguments = match block.get("input") {
            Some(input) => {
                if input.is_null() {
                    "{}".to_string()
                } else {
                    input.to_string()
                }
            }
            None => "{}".to_string(),
        };
        if name.is_empty() && arguments.is_empty() {
            continue;
        }
        out.push(DerivedToolCall { id, name, arguments });
    }
    out
}

/// Google Gemini native `generateContent` response: candidates[].content.
/// parts[].functionCall carries `name` and an `args` JSON object. Gemini
/// gives no call id, so the field stays empty.
/// Streaming SSE is not parsed yet, same as Anthropic.
pub fn tool_calls_from_gemini(body: &str, streamed: bool) -> Vec<DerivedToolCall> {
    let mut out = Vec::new();
    if streamed {
        return out;
    }
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return out;
    };
    let Some(candidates) = v.get("candidates").and_then(|c| c.as_array()) else {
        return out;
    };
    for cand in candidates {
        let Some(parts) = cand.pointer("/content/parts").and_then(|p| p.as_array()) else {
            continue;
        };
        for part in parts {
            let Some(fc) = part.get("functionCall") else {
                continue;
            };
            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let arguments = match fc.get("args") {
                Some(args) => {
                    if args.is_null() {
                        "{}".to_string()
                    } else {
                        args.to_string()
                    }
                }
                None => "{}".to_string(),
            };
            if name.is_empty() && arguments.is_empty() {
                continue;
            }
            out.push(DerivedToolCall {
                id: String::new(),
                name,
                arguments,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_tool_use_blocks() {
        let body = r#"{"id":"msg_1","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_9","name":"get_weather","input":{"city":"Denver"}}]}"#;
        let tcs = tool_calls_from_anthropic(body, false);
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_9");
        assert_eq!(tcs[0].name, "get_weather");
        assert_eq!(tcs[0].arguments, r#"{"city":"Denver"}"#);
    }

    #[test]
    fn anthropic_streamed_derives_nothing_yet() {
        let tcs = tool_calls_from_anthropic("event: stream", true);
        assert!(tcs.is_empty());
    }

    #[test]
    fn gemini_function_calls() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"checking"},{"functionCall":{"name":"lookup","args":{"q":"bus 15"}}}]}}]}"#;
        let tcs = tool_calls_from_gemini(body, false);
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].name, "lookup");
        assert_eq!(tcs[0].arguments, r#"{"q":"bus 15"}"#);
    }

    #[test]
    fn gemini_null_args_is_empty_object() {
        let body = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"ping","args":null}}]}}]}"#;
        let tcs = tool_calls_from_gemini(body, false);
        assert_eq!(tcs[0].arguments, "{}");
    }

    #[test]
    fn openai_bodies_untouched_by_native_parsers() {
        let body = r#"{"choices":[]}"#;
        assert!(tool_calls_from_anthropic(body, false).is_empty());
        assert!(tool_calls_from_gemini(body, false).is_empty());
    }
}
