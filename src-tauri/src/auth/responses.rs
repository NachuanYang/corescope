use futures_util::StreamExt;
use reqwest::Response;
use serde_json::{Value, json};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTPUT_TEXT_BYTES: usize = 512 * 1024;

pub fn build_responses_request(model_slug: &str, input: &str) -> Result<Value, String> {
    if model_slug.is_empty()
        || model_slug.len() > 160
        || !model_slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || input.trim().is_empty()
        || input.len() > 128 * 1024
    {
        return Err("AUTH_RESPONSES_REQUEST_INVALID".to_owned());
    }
    Ok(json!({
        "model": model_slug,
        "input": [{ "role": "user", "content": input }],
        "store": false,
        "stream": true,
    }))
}

pub async fn consume_response(response: Response) -> Result<String, String> {
    if !response.status().is_success() {
        return Err(format!(
            "AUTH_RESPONSES_HTTP_{}",
            response.status().as_u16()
        ));
    }
    let mut stream = response.bytes_stream();
    let mut parser = ResponseSseParser::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                "AUTH_NETWORK_TIMEOUT".to_owned()
            } else {
                "AUTH_SSE_STREAM_INTERRUPTED".to_owned()
            }
        })?;
        if let Some(text) = parser.push(&chunk)? {
            return Ok(text);
        }
    }
    parser
        .finish()?
        .ok_or_else(|| "AUTH_SSE_STREAM_INTERRUPTED".to_owned())
}

pub fn parse_responses_chunks(chunks: &[&[u8]]) -> Result<String, String> {
    let mut parser = ResponseSseParser::default();
    for chunk in chunks {
        if let Some(text) = parser.push(chunk)? {
            return Ok(text);
        }
    }
    parser
        .finish()?
        .ok_or_else(|| "AUTH_SSE_STREAM_INTERRUPTED".to_owned())
}

#[derive(Default)]
struct ResponseSseParser {
    pending: Vec<u8>,
    data_lines: Vec<Vec<u8>>,
    total_bytes: usize,
    output_text: String,
    completed: bool,
}

impl ResponseSseParser {
    fn push(&mut self, chunk: &[u8]) -> Result<Option<String>, String> {
        self.total_bytes = self
            .total_bytes
            .checked_add(chunk.len())
            .filter(|size| *size <= MAX_RESPONSE_BYTES)
            .ok_or_else(|| "AUTH_SSE_RESPONSE_TOO_LARGE".to_owned())?;
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(text) = self.process_line(&line)? {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }

    fn finish(&mut self) -> Result<Option<String>, String> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            if let Some(text) = self.process_line(&line)? {
                return Ok(Some(text));
            }
        }
        self.dispatch_event()
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<String>, String> {
        if line.is_empty() {
            return self.dispatch_event();
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            self.data_lines.push(value.to_vec());
        }
        Ok(None)
    }

    fn dispatch_event(&mut self) -> Result<Option<String>, String> {
        if self.data_lines.is_empty() {
            return Ok(None);
        }
        let data = self
            .data_lines
            .drain(..)
            .map(|line| String::from_utf8(line).map_err(|_| "AUTH_SSE_EVENT_INVALID".to_owned()))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        if data == "[DONE]" {
            return Ok(None);
        }
        let event: Value =
            serde_json::from_str(&data).map_err(|_| "AUTH_SSE_EVENT_INVALID".to_owned())?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "AUTH_SSE_EVENT_INVALID".to_owned())?;
                if self.output_text.len() + delta.len() > MAX_OUTPUT_TEXT_BYTES {
                    return Err("AUTH_SSE_OUTPUT_TOO_LARGE".to_owned());
                }
                self.output_text.push_str(delta);
                Ok(None)
            }
            Some("response.failed") => Err(safe_terminal_error(
                "AUTH_RESPONSES_FAILED",
                event
                    .pointer("/response/error/code")
                    .and_then(Value::as_str),
            )),
            Some("response.incomplete") => Err(safe_terminal_error(
                "AUTH_RESPONSES_INCOMPLETE",
                event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str),
            )),
            Some("error") => Err("AUTH_RESPONSES_EVENT_ERROR".to_owned()),
            Some("response.completed") => {
                if self.output_text.is_empty() {
                    self.output_text = extract_output_text(&event);
                }
                if self.output_text.len() > MAX_OUTPUT_TEXT_BYTES {
                    return Err("AUTH_SSE_OUTPUT_TOO_LARGE".to_owned());
                }
                self.completed = true;
                Ok(Some(std::mem::take(&mut self.output_text)))
            }
            Some(_) => Ok(None),
            None => Err("AUTH_SSE_EVENT_INVALID".to_owned()),
        }
    }
}

fn extract_output_text(event: &Value) -> String {
    event
        .pointer("/response/output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn safe_terminal_error(prefix: &str, code: Option<&str>) -> String {
    let code = code
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 100
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
        .unwrap_or("unknown");
    format!("{prefix}:{code}")
}
