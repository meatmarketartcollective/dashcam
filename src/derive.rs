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
