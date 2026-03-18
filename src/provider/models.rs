use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
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
        stream,
        parallel_tool_calls,
        store,
        max_output_tokens,
        temperature,
    }
}
