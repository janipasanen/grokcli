use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    pub stream: bool,
    pub parallel_tool_calls: bool,
    pub store: bool,
    pub max_output_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputContent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesResponse {
    pub output: Option<Vec<ResponseOutput>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseOutput {
    pub content: Option<Vec<ResponseContent>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseContent {
    #[serde(default)]
    pub text: Option<String>,
}

pub fn build_simple_request(
    model: String,
    prompt: String,
    stream: bool,
    parallel_tool_calls: bool,
    store: bool,
    max_output_tokens: u32,
    temperature: f32,
    tools: Option<Vec<Value>>,
) -> ResponsesRequest {
    ResponsesRequest {
        model,
        input: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContent {
                kind: "input_text".to_string(),
                text: prompt,
            }],
        }],
        instructions: None,
        tools,
        stream,
        parallel_tool_calls,
        store,
        max_output_tokens,
        temperature,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_simple_request_defaults_to_no_tools() {
        let request = build_simple_request(
            "grok-code-fast-1".to_string(),
            "hello".to_string(),
            true,
            false,
            true,
            4000,
            0.1,
            None,
        );

        assert!(request.tools.is_none());
    }
}
