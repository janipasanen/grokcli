use crate::provider::models::{ResponsesRequest, ResponsesResponse};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use std::time::Duration;

pub struct XaiClient {
    client: reqwest::Client,
    base_url: String,
}

impl XaiClient {
    pub fn new(api_key: &str, base_url: &str, timeout_seconds: u64) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {api_key}");
        let auth_value = HeaderValue::from_str(&bearer).context("invalid API key for header")?;
        headers.insert(AUTHORIZATION, auth_value);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(timeout_seconds))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    pub async fn create_response(&self, request: &ResponsesRequest) -> Result<String> {
        let url = format!("{}/v1/responses", self.base_url);
        let response = self
            .client
            .post(url)
            .json(request)
            .send()
            .await
            .context("xAI request failed")?
            .error_for_status()
            .context("xAI returned error status")?;

        let parsed = response
            .json::<ResponsesResponse>()
            .await
            .context("failed to parse xAI response JSON")?;
        Ok(extract_text_from_response(&parsed))
    }

    pub async fn stream_response_to_stdout(&self, request: &ResponsesRequest) -> Result<String> {
        let url = format!("{}/v1/responses", self.base_url);
        let response = self
            .client
            .post(url)
            .json(request)
            .send()
            .await
            .context("xAI request failed")?
            .error_for_status()
            .context("xAI returned error status")?;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut collected = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.context("failed to read streaming response chunk")?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(newline_pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_pos).collect();
                let trimmed = line.trim();
                if !trimmed.starts_with("data:") {
                    continue;
                }
                let payload = trimmed.trim_start_matches("data:").trim();
                if payload == "[DONE]" {
                    return Ok(collected);
                }
                if payload.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(payload) {
                    if let Some(delta) = extract_delta_text(&value) {
                        collected.push_str(&delta);
                        print!("{delta}");
                    }
                }
            }
        }

        Ok(collected)
    }
}

fn extract_text_from_response(response: &ResponsesResponse) -> String {
    response
        .output
        .as_ref()
        .into_iter()
        .flatten()
        .flat_map(|output| output.content.iter().flatten())
        .filter_map(|content| content.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

fn extract_delta_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) == Some("response.output_text.delta") {
        return value
            .get("delta")
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }

    value
        .get("output")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("content"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
