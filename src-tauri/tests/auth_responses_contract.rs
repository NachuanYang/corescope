use corescope_lib::auth::responses::{build_responses_request, parse_responses_chunks};

#[test]
fn plan_usage_request_uses_only_supported_explicit_non_stored_stream_fields() {
    let request = build_responses_request("gpt-fixture", "Say exactly: verified.").unwrap();
    assert_eq!(request["model"], "gpt-fixture");
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
    assert_eq!(request["input"][0]["role"], "user");
    assert_eq!(request["input"][0]["content"], "Say exactly: verified.");
    assert_eq!(
        request
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["input", "model", "store", "stream"]
    );
}

#[test]
fn stream_succeeds_only_after_completed_and_preserves_delta_text() {
    let events = [
        br#"data: {"type":"response.output_text.delta","delta":"hello "}"# as &[u8],
        b"\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n",
        br#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
    ];
    let result = parse_responses_chunks(&events).unwrap();
    assert_eq!(result, "hello world");
}

#[test]
fn failed_incomplete_malformed_and_interrupted_streams_remain_distinct_failures() {
    for (event, code) in [
        (br#"data: {"type":"response.failed","response":{"error":{"code":"usage_limit"}}}"# as &[u8], "AUTH_RESPONSES_FAILED:usage_limit"),
        (br#"data: {"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#, "AUTH_RESPONSES_INCOMPLETE:max_output_tokens"),
        (br#"data: {bad json}"#, "AUTH_SSE_EVENT_INVALID"),
    ] {
        assert_eq!(parse_responses_chunks(&[event]).unwrap_err(), code);
    }
    assert_eq!(
        parse_responses_chunks(&[
            br#"data: {"type":"response.output_text.delta","delta":"partial"}"#
        ])
        .unwrap_err(),
        "AUTH_SSE_STREAM_INTERRUPTED"
    );
}

#[test]
fn completed_response_uses_final_text_when_no_deltas_arrive() {
    let event = br#"data: {"type":"response.completed","response":{"output":[{"type":"message","content":[{"type":"output_text","text":"finished"}]}]}}"#;
    assert_eq!(parse_responses_chunks(&[event]).unwrap(), "finished");
}
