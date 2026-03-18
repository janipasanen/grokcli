use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_parallel_tool_calls")]
    pub parallel_tool_calls: bool,
    #[serde(default = "default_store")]
    pub store: bool,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_api_mode")]
    pub api_mode: String,
    #[serde(default = "default_auto_approve")]
    pub auto_approve: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            base_url: default_base_url(),
            stream: default_stream(),
            max_output_tokens: default_max_output_tokens(),
            temperature: default_temperature(),
            parallel_tool_calls: default_parallel_tool_calls(),
            store: default_store(),
            timeout_seconds: default_timeout_seconds(),
            api_mode: default_api_mode(),
            auto_approve: default_auto_approve(),
        }
    }
}

impl AppConfig {
    pub fn load(config_path: Option<PathBuf>) -> Result<Self> {
        let resolved = config_path.or_else(default_config_path);
        match resolved {
            Some(path) if path.exists() => {
                let raw = fs::read_to_string(&path).with_context(|| {
                    format!("failed to read config file at {}", path.display())
                })?;
                let cfg = toml::from_str::<Self>(&raw)
                    .with_context(|| format!("failed to parse TOML config at {}", path.display()))?;
                Ok(cfg)
            }
            _ => Ok(Self::default()),
        }
    }

    pub fn api_key(&self) -> Result<String> {
        let _ = self;
        env::var("XAI_API_KEY").context("XAI_API_KEY is not set")
    }
}

fn default_config_path() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/grok-agent/config.toml"))
}

fn default_provider() -> String {
    "xai".to_string()
}

fn default_model() -> String {
    "grok-4.1-fast-reasoning".to_string()
}

fn default_base_url() -> String {
    "https://api.x.ai".to_string()
}

fn default_stream() -> bool {
    true
}

fn default_max_output_tokens() -> u32 {
    4000
}

fn default_temperature() -> f32 {
    0.1
}

fn default_parallel_tool_calls() -> bool {
    false
}

fn default_store() -> bool {
    false
}

fn default_timeout_seconds() -> u64 {
    60
}

fn default_api_mode() -> String {
    "responses".to_string()
}

fn default_auto_approve() -> bool {
    false
}
