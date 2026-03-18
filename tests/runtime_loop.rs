use grokcli::agent::runtime::AgentRuntime;
use grokcli::config::config::AppConfig;
use grokcli::provider::LanguageModelProvider;
use grokcli::provider::models::ResponsesRequest;
use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[derive(Clone)]
struct MockProvider {
    responses: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl MockProvider {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LanguageModelProvider for MockProvider {
    async fn create_response_text(&self, _request: &ResponsesRequest) -> Result<String> {
        bail!("not used in this test")
    }

    async fn stream_response_text(&self, _request: &ResponsesRequest) -> Result<String> {
        bail!("not used in this test")
    }

    async fn create_response_json(&self, _body: &Value) -> Result<Value> {
        bail!("not used in this test")
    }

    async fn create_response_stream_json(&self, _body: &Value) -> Result<Value> {
        bail!("not used in this test")
    }

    async fn create_chat_completion_json(&self, body: &Value) -> Result<Value> {
        self.requests.lock().unwrap().push(body.clone());
        let mut responses = self.responses.lock().unwrap();
        responses
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no more mock responses queued"))
    }
}

fn chat_tool_call_response() -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": "{\"path\":\".\"}"
                    }
                }]
            }
        }]
    })
}

fn chat_final_response() -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "done"
            }
        }]
    })
}

#[tokio::test]
async fn runtime_executes_tool_and_returns_result_in_next_request() -> Result<()> {
    let dir = tempdir()?;
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    std::env::set_current_dir(dir.path())?;
    std::fs::write(dir.path().join("alpha.txt"), "alpha")?;
    std::fs::write(dir.path().join("beta.txt"), "beta")?;

    let provider = MockProvider::new(vec![chat_tool_call_response(), chat_final_response()]);
    let mut cfg = AppConfig::default();
    cfg.api_mode = "chat_completions".to_string();
    cfg.stream = false;
    let runtime = AgentRuntime::new(cfg, 4);
    runtime
        .run_ask(&provider, "list files".to_string(), None)
        .await?;

    let requests = provider.recorded_requests();
    assert!(requests.len() >= 2, "expected at least two provider calls");
    let second = &requests[1];
    let messages = second.get("messages").and_then(Value::as_array).unwrap();
    let tool_message = messages.iter().find(|m| {
        m.get("role")
            .and_then(Value::as_str)
            .map(|r| r == "tool")
            .unwrap_or(false)
    });
    let tool_message = tool_message.expect("tool message not found");
    let content = tool_message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(content.contains("alpha.txt"), "expected tool output to include alpha.txt");

    Ok(())
}
