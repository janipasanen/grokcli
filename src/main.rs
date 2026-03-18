use grokcli::agent::runtime::AgentRuntime;
use grokcli::config::config::AppConfig;
use grokcli::persistence::patch_store::PatchStore;
use grokcli::persistence::session_store::{SessionStore, latest_session_path, resolve_session_path};
use grokcli::provider::xai_client::XaiClient;
use grokcli::workflows::presets::{WorkflowPreset, apply_preset_prompt};
use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::env;
use std::io::{self, Write};
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

#[derive(Debug, Clone)]
enum InteractiveCommand {
    Help,
    Show,
    Exit,
    NewSession,
    ResumeLatest,
    Resume(String),
    Model(String),
    ApiMode(String),
    Mode(AgentMode),
    MaxSteps(u32),
    AutoApprove(bool),
    Stream(bool),
    Preset(Option<WorkflowPreset>),
    ContextBudget(usize),
    Prompt(String),
}

#[derive(Debug, Clone)]
struct InteractiveState {
    cfg: AppConfig,
    mode: AgentMode,
    max_steps: u32,
    preset: Option<WorkflowPreset>,
    session: SessionStore,
}

#[derive(Parser, Debug)]
#[command(name = "grokcli")]
#[command(about = "Terminal-native Grok agent scaffold")]
#[command(
    long_about = "Grok terminal coding agent prototype.\n\nThis CLI runs a deterministic local tool loop with xAI as planner. It supports safe local tool execution, approval-gated actions, patch application, resumable sessions, and an interactive shell when started without a prompt."
)]
#[command(
    after_long_help = "Help Sections\n\nInteractive:\n  Run `grokcli` with no prompt to enter the interactive shell.\n  Type `/` or `/help` to see slash commands.\n\nModes:\n  ask   Single-turn Q&A or short explanation. Prefer read-only behavior.\n  edit  Multi-step with edits after approval.\n  agent Full iterative tool-calling loop until stop conditions.\n\nSafety:\n  Tiered command policy is enforced by local tools.\n  Use --auto-approve only in trusted repos.\n\nSessions:\n  Each run writes JSONL events under ~/.local/share/grok-agent/sessions.\n  Use --resume-latest or --resume-session <id|path> to continue."
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
    #[arg(
        long,
        help_heading = "Runtime",
        help = "Override context budget in bytes for tool outputs."
    )]
    context_budget_bytes: Option<usize>,
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
    if let Some(bytes) = cli.context_budget_bytes {
        cfg.context_budget_bytes = bytes;
    }

    let selected_mode = cli.mode.unwrap_or(AgentMode::Agent);
    let header_model = cfg.model.clone();
    let header_api_mode = cfg.api_mode.clone();
    let header_auto_approve = cfg.auto_approve;

    let api_key = cfg.api_key()?;
    let client = XaiClient::new(&api_key, &cfg.base_url, cfg.timeout_seconds)?;
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
    if cli.prompt.is_none() {
        let session = match resume_store {
            Some(store) => store,
            None => SessionStore::for_new_session()?,
        };
        let state = InteractiveState {
            cfg,
            mode: selected_mode,
            max_steps: cli.max_steps,
            preset: cli.preset,
            session,
        };
        return run_interactive_shell(&client, state).await;
    }

    let runtime = AgentRuntime::new(cfg, effective_max_steps(&selected_mode, cli.max_steps));
    let base_prompt = cli
        .prompt
        .ok_or_else(|| anyhow::anyhow!("prompt is required unless --undo-last-patch is used"))?;
    let prompt = apply_selected_preset(cli.preset.as_ref(), &base_prompt);
    print_run_header(
        &selected_mode,
        header_model,
        header_api_mode,
        effective_max_steps(&selected_mode, cli.max_steps),
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
    println!("Interactive:");
    println!("  Run `grokcli` with no prompt to enter the interactive shell.");
    println!("  Type a normal prompt and press Enter to run it.");
    println!("  Type `/` or `/help` to list slash commands.");
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

async fn run_interactive_shell(client: &XaiClient, mut state: InteractiveState) -> Result<()> {
    print_interactive_banner(&state);
    loop {
        print!("grok> ");
        io::stdout().flush()?;

        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            println!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match parse_interactive_command(line) {
            Ok(InteractiveCommand::Help) => print_interactive_help(),
            Ok(InteractiveCommand::Show) => print_interactive_state(&state),
            Ok(InteractiveCommand::Exit) => break,
            Ok(InteractiveCommand::NewSession) => {
                state.session = SessionStore::for_new_session()?;
                println!("new session: {}", state.session.path().display());
            }
            Ok(InteractiveCommand::ResumeLatest) => match latest_session_path()? {
                Some(path) => {
                    state.session = SessionStore::open_existing(path)?;
                    println!("resumed session: {}", state.session.path().display());
                }
                None => println!("no saved sessions found"),
            },
            Ok(InteractiveCommand::Resume(spec)) => {
                let path = resolve_session_path(&spec)?;
                state.session = SessionStore::open_existing(path)?;
                println!("resumed session: {}", state.session.path().display());
            }
            Ok(InteractiveCommand::Model(model)) => {
                state.cfg.model = model;
                println!("model={}", state.cfg.model);
            }
            Ok(InteractiveCommand::ApiMode(api_mode)) => {
                state.cfg.api_mode = api_mode;
                println!("api_mode={}", state.cfg.api_mode);
            }
            Ok(InteractiveCommand::Mode(mode)) => {
                state.mode = mode;
                println!("mode={}", mode_name(&state.mode));
            }
            Ok(InteractiveCommand::MaxSteps(steps)) => {
                state.max_steps = steps;
                println!("max_steps={}", state.max_steps);
            }
            Ok(InteractiveCommand::AutoApprove(value)) => {
                state.cfg.auto_approve = value;
                println!("auto_approve={}", state.cfg.auto_approve);
            }
            Ok(InteractiveCommand::Stream(value)) => {
                state.cfg.stream = value;
                println!("stream={}", state.cfg.stream);
            }
            Ok(InteractiveCommand::Preset(preset)) => {
                state.preset = preset;
                match &state.preset {
                    Some(preset) => println!("preset={}", preset.to_possible_value().unwrap().get_name()),
                    None => println!("preset=off"),
                }
            }
            Ok(InteractiveCommand::ContextBudget(bytes)) => {
                state.cfg.context_budget_bytes = bytes;
                println!("context_budget_bytes={}", state.cfg.context_budget_bytes);
            }
            Ok(InteractiveCommand::Prompt(prompt)) => {
                let runtime = AgentRuntime::new(
                    state.cfg.clone(),
                    effective_max_steps(&state.mode, state.max_steps),
                );
                let prompt = apply_selected_preset(state.preset.as_ref(), &prompt);
                runtime
                    .run_ask(client, prompt, Some(state.session.clone()))
                    .await?;
            }
            Err(err) => println!("{err}"),
        }
    }
    Ok(())
}

fn print_interactive_banner(state: &InteractiveState) {
    println!("Interactive shell");
    println!("session={}", state.session.path().display());
    println!("type a prompt and press Enter");
    println!("type / or /help for commands, /exit to quit");
    print_interactive_state(state);
}

fn print_interactive_help() {
    println!("/help");
    println!("/show");
    println!("/exit");
    println!("/new-session");
    println!("/resume-latest");
    println!("/resume <id|path>");
    println!("/model <name>");
    println!("/api-mode <responses|chat-completions>");
    println!("/mode <ask|edit|agent>");
    println!("/max-steps <n>");
    println!("/auto-approve <on|off>");
    println!("/stream <on|off>");
    println!("/preset <rust-tests|swift-build|review-changed|off>");
    println!("/context-budget <bytes>");
}

fn print_interactive_state(state: &InteractiveState) {
    let preset = state
        .preset
        .as_ref()
        .and_then(|p| p.to_possible_value())
        .map(|v| v.get_name().to_string())
        .unwrap_or_else(|| "off".to_string());
    println!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={} stream={} preset={} context_budget_bytes={}",
        mode_name(&state.mode),
        state.cfg.model,
        state.cfg.api_mode,
        effective_max_steps(&state.mode, state.max_steps),
        state.cfg.auto_approve,
        state.cfg.stream,
        preset,
        state.cfg.context_budget_bytes
    );
}

fn parse_interactive_command(line: &str) -> Result<InteractiveCommand, String> {
    if !line.starts_with('/') {
        return Ok(InteractiveCommand::Prompt(line.to_string()));
    }
    if line == "/" || line == "/help" {
        return Ok(InteractiveCommand::Help);
    }

    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or_default();
    match cmd {
        "/show" => Ok(InteractiveCommand::Show),
        "/exit" | "/quit" => Ok(InteractiveCommand::Exit),
        "/new-session" => Ok(InteractiveCommand::NewSession),
        "/resume-latest" => Ok(InteractiveCommand::ResumeLatest),
        "/resume" => parts
            .next()
            .map(|s| InteractiveCommand::Resume(s.to_string()))
            .ok_or_else(|| "usage: /resume <id|path>".to_string()),
        "/model" => {
            let rest = line.trim_start_matches("/model").trim();
            if rest.is_empty() {
                Err("usage: /model <name>".to_string())
            } else {
                Ok(InteractiveCommand::Model(rest.to_string()))
            }
        }
        "/api-mode" => parts
            .next()
            .ok_or_else(|| "usage: /api-mode <responses|chat-completions>".to_string())
            .and_then(parse_api_mode_command),
        "/mode" => parts
            .next()
            .ok_or_else(|| "usage: /mode <ask|edit|agent>".to_string())
            .and_then(parse_mode_command),
        "/max-steps" => parts
            .next()
            .ok_or_else(|| "usage: /max-steps <n>".to_string())
            .and_then(|s| s.parse::<u32>().map(InteractiveCommand::MaxSteps).map_err(|_| "invalid step count".to_string())),
        "/auto-approve" => parts
            .next()
            .ok_or_else(|| "usage: /auto-approve <on|off>".to_string())
            .and_then(parse_bool_command)
            .map(InteractiveCommand::AutoApprove),
        "/stream" => parts
            .next()
            .ok_or_else(|| "usage: /stream <on|off>".to_string())
            .and_then(parse_bool_command)
            .map(InteractiveCommand::Stream),
        "/preset" => parts
            .next()
            .ok_or_else(|| "usage: /preset <rust-tests|swift-build|review-changed|off>".to_string())
            .and_then(parse_preset_command),
        "/context-budget" => parts
            .next()
            .ok_or_else(|| "usage: /context-budget <bytes>".to_string())
            .and_then(|s| s.parse::<usize>().map(InteractiveCommand::ContextBudget).map_err(|_| "invalid byte count".to_string())),
        _ => Err("unknown command; type /help".to_string()),
    }
}

fn parse_api_mode_command(value: &str) -> Result<InteractiveCommand, String> {
    match value {
        "responses" => Ok(InteractiveCommand::ApiMode("responses".to_string())),
        "chat-completions" | "chat_completions" => {
            Ok(InteractiveCommand::ApiMode("chat_completions".to_string()))
        }
        _ => Err("usage: /api-mode <responses|chat-completions>".to_string()),
    }
}

fn parse_mode_command(value: &str) -> Result<InteractiveCommand, String> {
    match value {
        "ask" => Ok(InteractiveCommand::Mode(AgentMode::Ask)),
        "edit" => Ok(InteractiveCommand::Mode(AgentMode::Edit)),
        "agent" => Ok(InteractiveCommand::Mode(AgentMode::Agent)),
        _ => Err("usage: /mode <ask|edit|agent>".to_string()),
    }
}

fn parse_bool_command(value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        _ => Err("expected on|off".to_string()),
    }
}

fn parse_preset_command(value: &str) -> Result<InteractiveCommand, String> {
    match value {
        "rust-tests" => Ok(InteractiveCommand::Preset(Some(WorkflowPreset::RustTests))),
        "swift-build" => Ok(InteractiveCommand::Preset(Some(WorkflowPreset::SwiftBuild))),
        "review-changed" => Ok(InteractiveCommand::Preset(Some(WorkflowPreset::ReviewChanged))),
        "off" => Ok(InteractiveCommand::Preset(None)),
        _ => Err("usage: /preset <rust-tests|swift-build|review-changed|off>".to_string()),
    }
}

fn apply_selected_preset(preset: Option<&WorkflowPreset>, prompt: &str) -> String {
    match preset {
        Some(preset) => apply_preset_prompt(preset, prompt),
        None => prompt.to_string(),
    }
}

fn effective_max_steps(mode: &AgentMode, max_steps: u32) -> u32 {
    match mode {
        AgentMode::Ask => 1,
        AgentMode::Edit | AgentMode::Agent => max_steps.max(1),
    }
}

fn mode_name(mode: &AgentMode) -> &'static str {
    match mode {
        AgentMode::Ask => "ask",
        AgentMode::Edit => "edit",
        AgentMode::Agent => "agent",
    }
}

fn print_run_header(
    mode: &AgentMode,
    model: String,
    api_mode: String,
    max_steps: u32,
    auto_approve: bool,
    resumed_session: Option<String>,
) {
    eprintln!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={}",
        mode_name(mode), model, api_mode, max_steps, auto_approve
    );
    if let Some(path) = resumed_session {
        eprintln!("resuming_session={}", path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_help_command() {
        match parse_interactive_command("/") {
            Ok(InteractiveCommand::Help) => {}
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_model_command() {
        match parse_interactive_command("/model grok-code-fast-1") {
            Ok(InteractiveCommand::Model(model)) => assert_eq!(model, "grok-code-fast-1"),
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_preset_off_command() {
        match parse_interactive_command("/preset off") {
            Ok(InteractiveCommand::Preset(None)) => {}
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_prompt_passthrough() {
        match parse_interactive_command("fix failing tests") {
            Ok(InteractiveCommand::Prompt(prompt)) => assert_eq!(prompt, "fix failing tests"),
            other => panic!("unexpected parse result: {:?}", other),
        }
    }
}
