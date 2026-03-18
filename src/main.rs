mod agent;
mod config;
mod persistence;
mod provider;
mod tools;
mod workflows;

use crate::agent::runtime::AgentRuntime;
use crate::config::config::AppConfig;
use crate::persistence::patch_store::PatchStore;
use crate::persistence::session_store::{SessionStore, latest_session_path, resolve_session_path};
use crate::provider::xai_client::XaiClient;
use crate::workflows::presets::{WorkflowPreset, apply_preset_prompt};
use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, ValueEnum)]
enum AgentMode {
    Ask,
    Edit,
    Agent,
}

#[derive(Debug, Clone, ValueEnum)]
enum ApiModeOpt {
    Responses,
    ChatCompletions,
}

#[derive(Parser, Debug)]
#[command(name = "grokcli")]
#[command(about = "Terminal-native Grok agent scaffold")]
#[command(
    long_about = "Grok terminal coding agent prototype.\n\nThis CLI runs a deterministic local tool loop with xAI as planner. It supports safe local tool execution, approval-gated actions, patch application, and resumable sessions."
)]
#[command(
    after_long_help = "Help Sections\n\nModes:\n  ask   Single-turn Q&A or short explanation. Prefer read-only behavior.\n  edit  Multi-step with edits after approval.\n  agent Full iterative tool-calling loop until stop conditions.\n\nSafety:\n  Tiered command policy is enforced by local tools.\n  Use --auto-approve only in trusted repos.\n\nSessions:\n  Each run writes JSONL events under ~/.local/share/grok-agent/sessions.\n  Use --resume-latest or --resume-session <id|path> to continue."
)]
struct Cli {
    #[arg(long, help_heading = "Config", help = "Path to a TOML config file.")]
    config: Option<PathBuf>,
    #[arg(long, help_heading = "Model", help = "Override model name for this run.")]
    model: Option<String>,
    #[arg(long, value_enum, help_heading = "Runtime", help = "High-level runtime mode.")]
    mode: Option<AgentMode>,
    #[arg(long, value_enum, help_heading = "Model", help = "Provider API mode override.")]
    api_mode: Option<ApiModeOpt>,
    #[arg(long, help_heading = "Model", help = "Disable streaming output where supported.")]
    no_stream: bool,
    #[arg(long, help_heading = "Safety", help = "Automatically approve approval-gated tool calls.")]
    auto_approve: bool,
    #[arg(long, help_heading = "Patches", help = "Reverse the most recent recorded patch for this repo and exit.")]
    undo_last_patch: bool,
    #[arg(long, help_heading = "Sessions", help = "Resume a specific session by id (timestamp) or file path.")]
    resume_session: Option<String>,
    #[arg(long, help_heading = "Sessions", help = "Resume the latest session from session storage.")]
    resume_latest: bool,
    #[arg(long, value_enum, help_heading = "Presets", help = "Apply a workflow prompt preset.")]
    preset: Option<WorkflowPreset>,
    #[arg(long, default_value_t = 8, help_heading = "Runtime", help = "Maximum agent loop steps.")]
    max_steps: u32,
    #[arg(long, help_heading = "Help", help = "Show extended help sections and exit.")]
    show_help_sections: bool,
    #[arg(help = "User task prompt.")]
    prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.show_help_sections {
        print_help_sections();
        return Ok(());
    }

    if cli.undo_last_patch {
        let repo_root = env::current_dir()?;
        let store = PatchStore::default()?;
        let result = store.undo_last_for_repo(&repo_root)?;
        println!("{}", result.message);
        return Ok(());
    }

    let mut cfg = AppConfig::load(cli.config)?;
    if let Some(model) = cli.model {
        cfg.model = model;
    }
    if cli.no_stream {
        cfg.stream = false;
    }
    if cli.auto_approve {
        cfg.auto_approve = true;
    }
    if let Some(api_mode) = cli.api_mode {
        cfg.api_mode = match api_mode {
            ApiModeOpt::Responses => "responses".to_string(),
            ApiModeOpt::ChatCompletions => "chat_completions".to_string(),
        };
    }

    let selected_mode = cli.mode.unwrap_or(AgentMode::Agent);
    let header_model = cfg.model.clone();
    let header_api_mode = cfg.api_mode.clone();
    let header_auto_approve = cfg.auto_approve;

    let api_key = cfg.api_key()?;
    let client = XaiClient::new(&api_key, &cfg.base_url, cfg.timeout_seconds)?;
    let runtime = AgentRuntime::new(cfg, cli.max_steps);
    let base_prompt = cli
        .prompt
        .ok_or_else(|| anyhow::anyhow!("prompt is required unless --undo-last-patch is used"))?;
    let prompt = match cli.preset {
        Some(preset) => apply_preset_prompt(&preset, &base_prompt),
        None => base_prompt,
    };
    let resume_store = if cli.resume_latest {
        match latest_session_path()? {
            Some(path) => Some(SessionStore::open_existing(path)?),
            None => None,
        }
    } else if let Some(spec) = cli.resume_session {
        let path = resolve_session_path(&spec)?;
        Some(SessionStore::open_existing(path)?)
    } else {
        None
    };
    print_run_header(
        &selected_mode,
        header_model,
        header_api_mode,
        cli.max_steps,
        header_auto_approve,
        resume_store
            .as_ref()
            .map(|s| s.path().to_string_lossy().to_string()),
    );
    runtime.run_ask(&client, prompt, resume_store).await?;

    Ok(())
}

fn print_help_sections() {
    println!("Grok CLI Help Sections");
    println!();
    println!("Modes:");
    println!("  ask   Short Q&A and explanation flows.");
    println!("  edit  Multi-step with explicit edits and approvals.");
    println!("  agent Full iterative tool-calling loop.");
    println!();
    println!("Safety:");
    println!("  Approval gates apply to risky tools.");
    println!("  --auto-approve bypasses prompts for this run.");
    println!();
    println!("Sessions:");
    println!("  Session logs: ~/.local/share/grok-agent/sessions/*.jsonl");
    println!("  Resume latest: --resume-latest");
    println!("  Resume specific: --resume-session <id|path>");
    println!();
    println!("Patches:");
    println!("  Undo last applied patch for current repo: --undo-last-patch");
}

fn print_run_header(
    mode: &AgentMode,
    model: String,
    api_mode: String,
    max_steps: u32,
    auto_approve: bool,
    resumed_session: Option<String>,
) {
    let mode_str = match mode {
        AgentMode::Ask => "ask",
        AgentMode::Edit => "edit",
        AgentMode::Agent => "agent",
    };
    eprintln!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={}",
        mode_str, model, api_mode, max_steps, auto_approve
    );
    if let Some(path) = resumed_session {
        eprintln!("resuming_session={}", path);
    }
}
