use anyhow::{Result, bail};
use async_trait::async_trait;
use grokcli::agent::runtime::AgentRuntime;
use grokcli::config::config::AppConfig;
use grokcli::provider::LanguageModelProvider;
use grokcli::provider::models::ResponsesRequest;
use grokcli::tools::checkpoint_repo::CheckpointRepoResult;
use grokcli::tools::registry::ToolRegistry;
use grokcli::tools::run_shell_command::RunShellCommandResult;
use grokcli::tools::undo_last_patch::UndoLastPatchResult;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

#[derive(Clone)]
struct MockProvider {
    responses: Arc<Mutex<VecDeque<Value>>>,
    responses_stream: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl MockProvider {
    fn new(responses: Vec<Value>, responses_stream: Vec<Value>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            responses_stream: Arc::new(Mutex::new(VecDeque::from(responses_stream))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn test_env_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
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
        self.requests.lock().unwrap().push(_body.clone());
        let mut responses = self.responses.lock().unwrap();
        responses
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no more mock responses queued"))
    }

    async fn create_response_stream_json(&self, _body: &Value) -> Result<Value> {
        self.requests.lock().unwrap().push(_body.clone());
        let mut responses = self.responses_stream.lock().unwrap();
        responses
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no more mock streaming responses queued"))
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
    let _guard = test_env_guard();
    let dir = tempdir()?;
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    std::env::set_current_dir(dir.path())?;
    std::fs::write(dir.path().join("alpha.txt"), "alpha")?;
    std::fs::write(dir.path().join("beta.txt"), "beta")?;

    let provider = MockProvider::new(
        vec![chat_tool_call_response(), chat_final_response()],
        Vec::new(),
    );
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
    assert!(
        content.contains("alpha.txt"),
        "expected tool output to include alpha.txt"
    );

    Ok(())
}

fn responses_tool_call_response() -> Value {
    json!({
        "id": "resp_1",
        "output": [{
            "type": "function_call",
            "name": "list_directory",
            "call_id": "call_1",
            "arguments": "{\"path\":\".\"}"
        }]
    })
}

fn responses_final_response() -> Value {
    json!({
        "id": "resp_2",
        "output": [{
            "content": [{
                "text": "done"
            }]
        }]
    })
}

#[tokio::test]
async fn runtime_streaming_responses_executes_tool() -> Result<()> {
    let _guard = test_env_guard();
    let dir = tempdir()?;
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    std::env::set_current_dir(dir.path())?;
    std::fs::write(dir.path().join("alpha.txt"), "alpha")?;

    let provider = MockProvider::new(
        vec![responses_final_response()],
        vec![responses_tool_call_response()],
    );
    let mut cfg = AppConfig::default();
    cfg.api_mode = "responses".to_string();
    cfg.stream = true;
    let runtime = AgentRuntime::new(cfg, 3);
    runtime
        .run_ask(&provider, "list files".to_string(), None)
        .await?;

    let requests = provider.recorded_requests();
    assert!(requests.len() >= 2, "expected at least two provider calls");
    let first = &requests[0];
    assert!(
        first.get("input").is_some(),
        "responses request should include input"
    );
    assert_eq!(first.get("store").and_then(Value::as_bool), Some(true));
    assert_eq!(first.get("stream").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[tokio::test]
async fn runtime_forces_store_true_for_multi_step_responses() -> Result<()> {
    let _guard = test_env_guard();
    let dir = tempdir()?;
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    std::env::set_current_dir(dir.path())?;
    std::fs::write(dir.path().join("alpha.txt"), "alpha")?;

    let provider = MockProvider::new(
        vec![responses_final_response()],
        vec![responses_tool_call_response()],
    );
    let mut cfg = AppConfig::default();
    cfg.api_mode = "responses".to_string();
    cfg.stream = false;
    cfg.store = false;
    let runtime = AgentRuntime::new(cfg, 3);
    runtime
        .run_ask(&provider, "list files".to_string(), None)
        .await?;

    let requests = provider.recorded_requests();
    let first = &requests[0];
    assert_eq!(first.get("store").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[tokio::test]
async fn runtime_omits_instructions_on_responses_continuation() -> Result<()> {
    let _guard = test_env_guard();
    let dir = tempdir()?;
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    std::env::set_current_dir(dir.path())?;
    std::fs::write(dir.path().join("alpha.txt"), "alpha")?;

    let provider = MockProvider::new(
        vec![responses_tool_call_response(), responses_final_response()],
        Vec::new(),
    );
    let mut cfg = AppConfig::default();
    cfg.api_mode = "responses".to_string();
    cfg.stream = false;
    let runtime = AgentRuntime::new(cfg, 3);
    runtime
        .run_ask(&provider, "list files".to_string(), None)
        .await?;

    let requests = provider.recorded_requests();
    assert!(requests.len() >= 2, "expected at least two provider calls");
    let first = &requests[0];
    let second = &requests[1];
    assert!(
        first.get("instructions").and_then(Value::as_str).is_some(),
        "initial responses request should include instructions"
    );
    assert!(
        second
            .get("previous_response_id")
            .and_then(Value::as_str)
            .is_some(),
        "continuation request should include previous_response_id"
    );
    assert!(
        second.get("instructions").is_none(),
        "continuation request must omit instructions"
    );
    Ok(())
}

#[test]
fn tool_registry_run_tests_and_build_project() -> Result<()> {
    let _guard = test_env_guard();
    let dir = tempdir()?;
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )?;
    std::fs::create_dir_all(dir.path().join("src"))?;
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n")?;

    let registry = ToolRegistry::new(dir.path());
    let run_tests = registry.execute(grokcli::tools::registry::ToolCallRequest {
        name: "run_tests".to_string(),
        arguments: json!({ "language": "rust", "approved": true }),
    })?;
    let run_tests_result: RunShellCommandResult = serde_json::from_value(run_tests.result.clone())?;
    assert!(run_tests_result.exit_code.is_some());

    let build_project = registry.execute(grokcli::tools::registry::ToolCallRequest {
        name: "build_project".to_string(),
        arguments: json!({ "language": "rust", "approved": true }),
    })?;
    let build_project_result: RunShellCommandResult =
        serde_json::from_value(build_project.result.clone())?;
    assert!(build_project_result.exit_code.is_some());

    Ok(())
}

#[test]
fn tool_registry_checkpoint_and_undo_require_approval() -> Result<()> {
    let _guard = test_env_guard();
    let dir = tempdir()?;
    let registry = ToolRegistry::new(dir.path());

    let checkpoint = registry.execute(grokcli::tools::registry::ToolCallRequest {
        name: "checkpoint_repo".to_string(),
        arguments: json!({ "approved": false }),
    })?;
    let checkpoint_result: CheckpointRepoResult =
        serde_json::from_value(checkpoint.result.clone())?;
    assert!(checkpoint_result.approval_required);

    let undo = registry.execute(grokcli::tools::registry::ToolCallRequest {
        name: "undo_last_patch".to_string(),
        arguments: json!({ "approved": false }),
    })?;
    let undo_result: UndoLastPatchResult = serde_json::from_value(undo.result.clone())?;
    assert!(undo_result.approval_required);

    Ok(())
}

#[test]
fn tool_registry_apply_patch_description_mentions_supported_format() {
    let registry = ToolRegistry::new(".");
    let defs = registry.definitions_json();
    let apply_patch = defs
        .as_array()
        .and_then(|defs| {
            defs.iter()
                .find(|def| def.get("name").and_then(Value::as_str) == Some("apply_patch"))
        })
        .expect("apply_patch definition");
    let description = apply_patch
        .get("description")
        .and_then(Value::as_str)
        .expect("apply_patch description");

    assert!(description.contains("raw unified diff"));
    assert!(description.contains("*** Begin Patch"));
}
