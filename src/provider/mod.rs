pub mod agent_tools;
pub mod models;
pub mod xai_client;

use crate::provider::models::ResponsesRequest;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub trait LanguageModelProvider: Send + Sync {
    async fn create_response_text(&self, request: &ResponsesRequest) -> Result<String>;
    async fn stream_response_text(&self, request: &ResponsesRequest) -> Result<String>;
    async fn create_response_json(&self, body: &Value) -> Result<Value>;
    async fn create_response_stream_json(&self, body: &Value) -> Result<Value>;
    async fn create_chat_completion_json(&self, body: &Value) -> Result<Value>;
}
