use reqwest::{Client, Response, StatusCode, redirect::Policy};
use serde::Serialize;
use std::time::Duration;
use zeroize::Zeroizing;

use super::oauth::NetworkRoute;

const MAX_HTTP_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub struct AuthHttpClients {
    system_proxy: Client,
    direct: Client,
}

impl AuthHttpClients {
    pub fn new() -> Result<Self, String> {
        let build = |direct: bool| {
            let mut builder = Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(Policy::none());
            if direct {
                builder = builder.no_proxy();
            }
            builder
                .build()
                .map_err(|_| "AUTH_HTTP_CLIENT_INIT_FAILED".to_owned())
        };
        Ok(Self {
            system_proxy: build(false)?,
            direct: build(true)?,
        })
    }

    pub fn client(&self, route: NetworkRoute) -> &Client {
        match route {
            NetworkRoute::SystemProxy => &self.system_proxy,
            NetworkRoute::Direct => &self.direct,
        }
    }
}

pub struct BoundedHttpResponse {
    pub status: StatusCode,
    pub body: Zeroizing<Vec<u8>>,
}

pub async fn get_response(
    client: &Client,
    endpoint: &str,
    bearer_token: Option<&str>,
    max_bytes: usize,
) -> Result<BoundedHttpResponse, String> {
    let mut request = client.get(endpoint);
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| network_failure("send", error))?;
    read_bounded_response(response, max_bytes).await
}

pub async fn post_form_response(
    client: &Client,
    endpoint: &str,
    fields: &[(&str, &str)],
    max_bytes: usize,
) -> Result<BoundedHttpResponse, String> {
    let response = client
        .post(endpoint)
        .form(fields)
        .send()
        .await
        .map_err(|error| network_failure("form_send", error))?;
    read_bounded_response(response, max_bytes).await
}

pub async fn post_json_response<T: Serialize>(
    client: &Client,
    endpoint: &str,
    bearer_token: &str,
    body: &T,
) -> Result<Response, String> {
    client
        .post(endpoint)
        .bearer_auth(bearer_token)
        .json(body)
        .send()
        .await
        .map_err(|error| network_failure("json_send", error))
}

pub async fn post_streaming_json_response<T: Serialize>(
    client: &Client,
    endpoint: &str,
    bearer_token: &str,
    body: &T,
    timeout: Duration,
) -> Result<Response, String> {
    client
        .post(endpoint)
        .timeout(timeout)
        .bearer_auth(bearer_token)
        .json(body)
        .send()
        .await
        .map_err(|error| network_failure("stream_send", error))
}

pub async fn read_bounded_response(
    mut response: Response,
    max_bytes: usize,
) -> Result<BoundedHttpResponse, String> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length as usize > max_bytes)
    {
        return Err("AUTH_HTTP_RESPONSE_TOO_LARGE".to_owned());
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| network_failure("response_read", error))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err("AUTH_HTTP_RESPONSE_TOO_LARGE".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(BoundedHttpResponse { status, body })
}

pub const fn max_json_response_bytes() -> usize {
    MAX_HTTP_RESPONSE_BYTES
}

fn network_failure(stage: &'static str, error: reqwest::Error) -> String {
    let timeout = error.is_timeout();
    let connect = error.is_connect();
    let request = error.is_request();
    let body = error.is_body();
    tracing::warn!(
        stage,
        timeout,
        connect,
        request,
        body,
        "ChatGPT network request failed"
    );
    if timeout {
        "AUTH_NETWORK_TIMEOUT".to_owned()
    } else if connect {
        "AUTH_NETWORK_CONNECT_FAILED".to_owned()
    } else if body {
        "AUTH_NETWORK_RESPONSE_FAILED".to_owned()
    } else {
        "AUTH_NETWORK_REQUEST_FAILED".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::sleep,
    };

    #[tokio::test]
    async fn streaming_timeout_covers_the_response_body_and_is_not_reported_as_interruption() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            sleep(Duration::from_millis(100)).await;
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });

        let client = Client::builder().no_proxy().build().unwrap();
        let endpoint = format!("http://{address}/responses");
        let response = post_streaming_json_response(
            &client,
            &endpoint,
            "synthetic-token",
            &json!({ "model": "fixture", "stream": true }),
            Duration::from_millis(30),
        )
        .await
        .unwrap();
        let error = super::super::responses::consume_response(response)
            .await
            .unwrap_err();

        assert_eq!(error, "AUTH_NETWORK_TIMEOUT");
        server.await.unwrap();
    }
}
