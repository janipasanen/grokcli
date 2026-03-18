use crate::provider::models::{ResponsesRequest, ResponsesResponse};
use crate::provider::LanguageModelProvider;
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use std::time::Duration;
use tokio::time::sleep;

pub struct XaiClient {
    client: reqwest::Client,
    base_url: String,
}

impl XaiClient {
    async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let max_attempts = 3u32;

        for attempt in 1..=max_attempts {
            let send_result = self.client.post(&url).json(body).send().await;
            match send_result {
                Ok(response) => {
                    let status = response.status();
                    let body_text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "<failed to read response body>".to_string());
                    if status.is_success() {
                        let value: Value = serde_json::from_str(&body_text)
                            .context("failed to parse xAI response JSON")?;
                        return Ok(value);
                    }

                    if should_retry_status(status) && attempt < max_attempts {
                        sleep(backoff_delay(attempt)).await;
                        continue;
                    }

                    let snippet = truncate_for_error(&body_text);
                    let class = classify_status(status);
                    bail!("xAI returned {status} ({class}) at {url}: {snippet}");
                }
                Err(err) => {
                    if should_retry_transport(&err) && attempt < max_attempts {
                        sleep(backoff_delay(attempt)).await;
                        continue;
                    }
                    let class = classify_transport(&err);
                    return Err(anyhow!("xAI request failed ({class}) at {url}: {err}"));
                }
            }
        }

        Err(anyhow!("xAI request failed after retries at {url}"))
    }

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

    pub async fn create_response_json(&self, body: &Value) -> Result<Value> {
        self.post_json("/v1/responses", body).await
    }

    pub async fn create_chat_completion_json(&self, body: &Value) -> Result<Value> {
        self.post_json("/v1/chat/completions", body).await
    }

    pub async fn create_response_stream_json(&self, body: &Value) -> Result<Value> {
        let url = format!("{}/v1/responses", self.base_url);
        let max_attempts = 2u32;
        for attempt in 1..=max_attempts {
            let response = match self.client.post(&url).json(body).send().await {
                Ok(r) => r,
                Err(err) => {
                    if should_retry_transport(&err) && attempt < max_attempts {
                        sleep(backoff_delay(attempt)).await;
                        continue;
                    }
                    let class = classify_transport(&err);
                    return Err(anyhow!("xAI streaming request failed ({class}) at {url}: {err}"));
                }
            };
            let status = response.status();
            if !status.is_success() {
                let text = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read response body>".to_string());
                if should_retry_status(status) && attempt < max_attempts {
                    sleep(backoff_delay(attempt)).await;
                    continue;
                }
                bail!(
                    "xAI returned {} ({}) at {}: {}",
                    status,
                    classify_status(status),
                    url,
                    truncate_for_error(&text)
                );
            }

            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut final_response: Option<Value> = None;
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
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(payload) {
                        if value.get("type").and_then(Value::as_str)
                            == Some("response.output_text.delta")
                        {
                            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                                print!("{delta}");
                            }
                        }
                        if let Some(resp) = value.get("response") {
                            let resp = resp.clone();
                            if has_function_call(&resp) {
                                println!();
                                return Ok(resp);
                            }
                            final_response = Some(resp);
                        } else if value.get("output").is_some() && value.get("id").is_some() {
                            if has_function_call(&value) {
                                println!();
                                return Ok(value.clone());
                            }
                            final_response = Some(value.clone());
                        }
                    }
                }
            }
            println!();
            if let Some(value) = final_response {
                return Ok(value);
            }
            return Err(anyhow!(
                "xAI streaming response completed without final response payload"
            ));
        }
        Err(anyhow!("xAI streaming response failed after retries"))
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

fn has_function_call(response: &Value) -> bool {
    response
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|item| {
                item.get("type")
                    .and_then(Value::as_str)
                    .map(|t| t == "function_call" || t.ends_with(".function_call"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[async_trait]
impl LanguageModelProvider for XaiClient {
    async fn create_response_text(&self, request: &ResponsesRequest) -> Result<String> {
        self.create_response(request).await
    }

    async fn stream_response_text(&self, request: &ResponsesRequest) -> Result<String> {
        self.stream_response_to_stdout(request).await
    }

    async fn create_response_json(&self, body: &Value) -> Result<Value> {
        self.create_response_json(body).await
    }

    async fn create_response_stream_json(&self, body: &Value) -> Result<Value> {
        self.create_response_stream_json(body).await
    }

    async fn create_chat_completion_json(&self, body: &Value) -> Result<Value> {
        self.create_chat_completion_json(body).await
    }
}

fn should_retry_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn should_retry_transport(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

fn classify_status(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "auth",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit",
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::INTERNAL_SERVER_ERROR
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => "server",
        _ => "http_error",
    }
}

fn classify_transport(err: &reqwest::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_request() {
        "request"
    } else {
        "transport"
    }
}

fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 250u64.saturating_mul(2u64.saturating_pow(attempt - 1));
    let jitter_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as u64)
        .unwrap_or(0))
        % 150;
    Duration::from_millis(base_ms + jitter_ms)
}

fn truncate_for_error(s: &str) -> String {
    const MAX: usize = 400;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}...", &s[..MAX])
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
