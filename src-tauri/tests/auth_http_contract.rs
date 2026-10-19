use corescope_lib::{
    analysis::{AnalysisScope, EvidenceDevice, EvidenceSnapshot, build_analysis_prompt},
    auth::{
        http::{AuthHttpClients, get_response, post_json_response},
        models::filter_listed_models,
        oauth::NetworkRoute,
        responses::{build_responses_request, consume_response},
    },
    chat::{ChatTurn, build_chat_prompt, parse_chat_answer},
};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

async fn start_capture(response: Vec<u8>) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let expected_end = loop {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let body_len = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")?
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0);
                break header_end + 4 + body_len;
            }
        };
        while request.len() < expected_end {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
        }
        stream.write_all(&response).await.unwrap();
        request
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn model_request_uses_bearer_auth_and_parses_the_account_catalog() {
    let body =
        r#"{"models":[{"slug":"model-one","display_name":"Model One","visibility":"list"}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body,
    ).into_bytes();
    let (endpoint, server) = start_capture(response).await;
    let clients = AuthHttpClients::new().unwrap();
    let url = format!("{endpoint}/v1/models");
    let result = get_response(
        clients.client(NetworkRoute::Direct),
        &url,
        Some("synthetic-access-token"),
        2048,
    )
    .await
    .unwrap();
    assert!(result.status.is_success());
    let catalog: Value = serde_json::from_slice(&result.body).unwrap();
    assert_eq!(filter_listed_models(&catalog).unwrap()[0].slug, "model-one");

    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-access-token")
    );
}

#[tokio::test]
async fn responses_request_uses_public_endpoint_explicit_input_store_false_and_stream_true() {
    let event_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"verified\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        event_body.len(),
        event_body,
    );
    let (endpoint, server) = start_capture(response.into_bytes()).await;
    let clients = AuthHttpClients::new().unwrap();
    let body = build_responses_request("model-one", "fixed verification text").unwrap();
    let response = post_json_response(
        clients.client(NetworkRoute::Direct),
        &format!("{endpoint}/v1/responses"),
        "synthetic-access-token",
        &body,
    )
    .await
    .unwrap();
    assert_eq!(consume_response(response).await.unwrap(), "verified");

    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-access-token")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    let (_, request_body) = request.split_once("\r\n\r\n").unwrap();
    let request_json: Value = serde_json::from_str(request_body).unwrap();
    assert_eq!(request_json["model"], "model-one");
    assert_eq!(request_json["input"][0]["role"], "user");
    assert_eq!(
        request_json["input"][0]["content"],
        "fixed verification text"
    );
    assert_eq!(request_json["store"], false);
    assert_eq!(request_json["stream"], true);
    assert!(
        request_json
            .as_object()
            .unwrap()
            .keys()
            .all(|key| matches!(key.as_str(), "model" | "input" | "store" | "stream"))
    );
}

#[tokio::test]
async fn analysis_sends_only_the_allowlisted_snapshot_in_the_non_stored_responses_input() {
    let snapshot = EvidenceSnapshot {
        snapshot_id: "fixture-snapshot-001".to_owned(),
        scope: AnalysisScope::System,
        captured_at_utc: "2026-10-14T12:00:00Z".to_owned(),
        scan_at_utc: "2026-10-14T11:59:00Z".to_owned(),
        devices: vec![EvidenceDevice {
            evidence_key: "hardware-1.kind".to_owned(),
            device_key: "hardware-1".to_owned(),
            category: "cpu".to_owned(),
            kind: "cpu".to_owned(),
            name: Some("Fixture CPU".to_owned()),
            manufacturer: Some("Fixture Vendor".to_owned()),
            virtual_device: Some(false),
            source: "Windows inventory".to_owned(),
            attributes: BTreeMap::new(),
        }],
        metrics: Vec::new(),
    };
    let prompt = build_analysis_prompt(&snapshot).unwrap();
    assert!(prompt.contains("fixture-snapshot-001"));
    assert!(!prompt.contains("local-cpu-id"));

    let event_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"fixture analysis\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        event_body.len(),
        event_body,
    );
    let (endpoint, server) = start_capture(response.into_bytes()).await;
    let clients = AuthHttpClients::new().unwrap();
    let body = build_responses_request("model-fixture", &prompt).unwrap();
    let response = post_json_response(
        clients.client(NetworkRoute::Direct),
        &format!("{endpoint}/v1/responses"),
        "synthetic-access-token",
        &body,
    )
    .await
    .unwrap();
    assert_eq!(
        consume_response(response).await.unwrap(),
        "fixture analysis"
    );

    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-access-token")
    );
    let (_, request_body) = request.split_once("\r\n\r\n").unwrap();
    let request_json: Value = serde_json::from_str(request_body).unwrap();
    assert_eq!(request_json["model"], "model-fixture");
    assert_eq!(request_json["input"][0]["content"], prompt);
    assert_eq!(request_json["store"], false);
    assert_eq!(request_json["stream"], true);
    assert!(!request_body.contains("local-cpu-id"));
}

#[tokio::test]
async fn chat_sends_the_reviewed_evidence_prompt_through_completed_non_stored_responses() {
    let snapshot = EvidenceSnapshot {
        snapshot_id: "chat-snapshot-002".to_owned(),
        scope: AnalysisScope::Device,
        captured_at_utc: "2026-10-14T12:00:00Z".to_owned(),
        scan_at_utc: "2026-10-14T11:59:00Z".to_owned(),
        devices: vec![EvidenceDevice {
            evidence_key: "device.kind".to_owned(),
            device_key: "device".to_owned(),
            category: "gpu".to_owned(),
            kind: "gpu_adapter".to_owned(),
            name: Some("Fixture GPU".to_owned()),
            manufacturer: Some("Fixture Vendor".to_owned()),
            virtual_device: Some(false),
            source: "Windows inventory".to_owned(),
            attributes: BTreeMap::new(),
        }],
        metrics: vec![],
    };
    let prompt = build_chat_prompt(
        &snapshot,
        &[ChatTurn {
            role: "user".to_owned(),
            status: "COMPLETED".to_owned(),
            body: "先前问题".to_owned(),
        }],
        "这张显卡的温度有数据吗？",
    )
    .unwrap();
    assert!(prompt.contains("chat-snapshot-002"));
    assert!(!prompt.contains("private-local-device-id"));

    let answer = serde_json::json!({
        "answer": "当前快照只包含已识别型号，没有温度读数。",
        "citations": [{ "claim": "当前快照包含已识别型号。", "evidenceKeys": ["device.kind"] }],
        "evidenceSnapshotId": snapshot.snapshot_id,
    })
    .to_string();
    let event_body = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\"}}}}\n\n",
        serde_json::to_string(&answer).unwrap(),
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        event_body.len(),
        event_body,
    );
    let (endpoint, server) = start_capture(response.into_bytes()).await;
    let clients = AuthHttpClients::new().unwrap();
    let body = build_responses_request("model-chat-fixture", &prompt).unwrap();
    let response = post_json_response(
        clients.client(NetworkRoute::Direct),
        &format!("{endpoint}/v1/responses"),
        "synthetic-access-token",
        &body,
    )
    .await
    .unwrap();
    let completed_text = consume_response(response).await.unwrap();
    let parsed = parse_chat_answer(&completed_text, &snapshot).unwrap();
    assert_eq!(parsed.evidence_snapshot_id, snapshot.snapshot_id);

    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-access-token")
    );
    let (_, request_body) = request.split_once("\r\n\r\n").unwrap();
    let request_json: Value = serde_json::from_str(request_body).unwrap();
    assert_eq!(request_json["model"], "model-chat-fixture");
    assert_eq!(request_json["input"][0]["content"], prompt);
    assert_eq!(request_json["store"], false);
    assert_eq!(request_json["stream"], true);
    assert!(!request_body.contains("private-local-device-id"));
}

#[tokio::test]
async fn http_error_does_not_surface_or_retain_untrusted_response_body() {
    let body = r#"{"error":"fixture"}"#;
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body,
    ).into_bytes();
    let (endpoint, _server) = start_capture(response).await;
    let clients = AuthHttpClients::new().unwrap();
    let request = build_responses_request("model-one", "fixed verification text").unwrap();
    let response = post_json_response(
        clients.client(NetworkRoute::Direct),
        &format!("{endpoint}/v1/responses"),
        "synthetic-access-token",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(
        consume_response(response).await.unwrap_err(),
        "AUTH_RESPONSES_HTTP_401"
    );
}
