mod agent;
mod config;
mod persistence;
mod provider;
mod tools;
mod workflows;

use crate::agent::runtime::AgentRuntime;
use crate::config::config::AppConfig;
use crate::workflows::presets::{WorkflowPreset, apply_preset_prompt};
use crate::provider::xai_client::XaiClient;
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "grokcli")]
#[command(about = "Terminal-native Grok agent scaffold")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    no_stream: bool,
    #[arg(long, value_enum)]
    preset: Option<WorkflowPreset>,
    #[arg(long, default_value_t = 8)]
    max_steps: u32,
    prompt: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = AppConfig::load(cli.config)?;
    if let Some(model) = cli.model {
        cfg.model = model;
    }
    if cli.no_stream {
        cfg.stream = false;
    }

    let api_key = cfg.api_key()?;
    let client = XaiClient::new(&api_key, &cfg.base_url, cfg.timeout_seconds)?;
    let runtime = AgentRuntime::new(cfg, cli.max_steps);
    let prompt = match cli.preset {
        Some(preset) => apply_preset_prompt(&preset, &cli.prompt),
        None => cli.prompt,
    };
    runtime.run_ask(&client, prompt).await?;

    Ok(())
}
