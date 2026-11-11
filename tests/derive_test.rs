use dashcam::derive::tool_calls_from_response;

/// A plain (non-streamed) OpenAI-style response with two tool calls.
#[test]
fn non_streamed_json_yields_tool_calls() {
    let body = r#"{
      "choices": [{
        "message": {
          "role": "assistant",
          "tool_calls": [
            {"id": "call_a", "type": "function",
             "function": {"name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}"}},
            {"id": "call_b", "type": "function",
             "function": {"name": "search_files", "arguments": "{\"q\":\"notes\"}"}}
          ]
        }
      }]
    }"#;
    let calls = tool_calls_from_response(body, false);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[0].arguments, r#"{"city":"Tokyo"}"#);
    assert_eq!(calls[1].name, "search_files");
    assert_eq!(calls[1].arguments, r#"{"q":"notes"}"#);
}

/// A streamed response where one tool call's name and arguments are split
/// across delta chunks must reassemble to the same call.
#[test]
fn streamed_sse_reassembles_split_tool_call() {
    let mut body = String::new();
    // first delta carries the id and the start of the name
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"type\":\"function\",\"function\":{\"name\":\"get_\",\"arguments\":\"\"}}]}}]}\n\n");
    // second delta finishes the name and starts the arguments
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"To\"}}]}}]}\n\n");
    // third delta finishes the arguments
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"kyo\\\"}\"}}]}}]}\n\n");
    body.push_str("data: [DONE]\n\n");
    let calls = tool_calls_from_response(&body, true);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_x");
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].arguments, r#"{"city":"Tokyo"}"#);
}

/// Two streamed tool calls keep their order and do not bleed into each other.
#[test]
fn streamed_sse_keeps_two_calls_separate() {
    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"alpha\",\"arguments\":\"{}\"}},{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"beta\",\"arguments\":\"{\\\"k\\\":1}\"}}]}}]}\n\n");
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"a2\",\"arguments\":\"\\\"tail\\\"\"}}]}}]}\n\n");
    body.push_str("data: [DONE]\n\n");
    let calls = tool_calls_from_response(&body, true);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "alphaa2");
    assert_eq!(calls[0].arguments, "{}\"tail\"");
    assert_eq!(calls[1].name, "beta");
    assert_eq!(calls[1].arguments, "{\"k\":1}");
}

/// A plain assistant reply with no tool calls yields nothing, and a body the
/// provider sent in some unknown dialect must not panic.
#[test]
fn non_tool_bodies_yield_nothing() {
    assert!(tool_calls_from_response(r#"{"choices":[{"message":{"content":"hello"}}]}"#, false).is_empty());
    assert!(tool_calls_from_response("not json at all", false).is_empty());
    assert!(tool_calls_from_response("", false).is_empty());
    assert!(tool_calls_from_response("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n", true).is_empty());
    assert!(tool_calls_from_response("data: [DONE]\n\n", true).is_empty());
}
