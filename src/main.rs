use grokcli::agent::runtime::AgentRuntime;
use grokcli::config::config::AppConfig;
use grokcli::persistence::patch_store::PatchStore;
use grokcli::persistence::session_store::{SessionStore, latest_session_path, resolve_session_path};
use grokcli::provider::xai_client::XaiClient;
use grokcli::workflows::presets::{
    WorkflowPreset, apply_preset_prompt, infer_preset_for_prompt,
};
use anyhow::Result;
use clap::{Parser, ValueEnum};
use rustyline::error::ReadlineError;
use rustyline::{
    Cmd, ConditionalEventHandler, DefaultEditor, Event, EventContext, EventHandler, KeyEvent,
    RepeatCount,
};
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    VerboseTools(bool),
    Stream(bool),
    Preset(Option<WorkflowPreset>),
    ContextBudget(usize),
    Queue(String),
    QueueShow,
    QueueClear,
    QueueRun,
    Prompt(String),
}

#[derive(Debug, Clone)]
struct InteractiveState {
    cfg: AppConfig,
    mode: AgentMode,
    max_steps: u32,
    preset: Option<WorkflowPreset>,
    session: SessionStore,
    queued_tasks: VecDeque<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadlineAction {
    QueueCurrentLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputSource {
    Immediate,
    Queued,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineOutcome {
    Continue,
    DrainQueue,
    Exit,
}

#[derive(Clone)]
struct QueueCurrentLineHandler {
    signal: Arc<Mutex<Option<ReadlineAction>>>,
}

impl ConditionalEventHandler for QueueCurrentLineHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext,
    ) -> Option<Cmd> {
        if ctx.line().trim().is_empty() {
            return None;
        }
        if let Ok(mut signal) = self.signal.lock() {
            *signal = Some(ReadlineAction::QueueCurrentLine);
        }
        Some(Cmd::AcceptLine)
    }
}

#[derive(Parser, Debug)]
#[command(name = "grokcli")]
#[command(about = "Terminal-native Grok agent scaffold")]
#[command(
    long_about = "Grok terminal coding agent prototype.\n\nThis CLI runs a deterministic local tool loop with xAI as planner. It supports safe local tool execution, approval-gated actions, patch application, resumable sessions, and an interactive shell when started without a prompt."
)]
#[command(
    after_long_help = "Help Sections\n\nInteractive:\n  Run `grokcli` with no prompt to enter the interactive shell.\n  Type `/` or `/help` to see slash commands.\n  In TTY mode, Up/Down recall history and Tab or Ctrl+Q queue the current line.\n\nModes:\n  ask   Single-turn Q&A or short explanation. Prefer read-only behavior.\n  edit  Multi-step with edits after approval.\n  agent Full iterative tool-calling loop until stop conditions.\n\nSafety:\n  Tiered command policy is enforced by local tools.\n  Use --auto-approve only in trusted repos.\n  Use --verbose-tools or --no-verbose-tools to control live tool traces and tool output rendering.\n\nSessions:\n  Each run writes JSONL events under ~/.local/share/grok-agent/sessions.\n  Use --resume-latest or --resume-session <id|path> to continue."
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
    #[arg(long, help_heading = "Runtime", help = "Print tool actions and tool outputs during execution.")]
    verbose_tools: bool,
    #[arg(long, help_heading = "Runtime", help = "Suppress tool action and tool output printing during execution.", conflicts_with = "verbose_tools")]
    no_verbose_tools: bool,
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
    if cli.verbose_tools {
        cfg.verbose_tools = true;
    }
    if cli.no_verbose_tools {
        cfg.verbose_tools = false;
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
    let header_verbose_tools = cfg.verbose_tools;

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
            queued_tasks: VecDeque::new(),
        };
        return run_interactive_shell(&client, state).await;
    }

    let runtime = AgentRuntime::new(cfg, effective_max_steps(&selected_mode, cli.max_steps));
    let base_prompt = cli
        .prompt
        .ok_or_else(|| anyhow::anyhow!("prompt is required unless --undo-last-patch is used"))?;
    let repo_root = env::current_dir()?;
    let (prompt, effective_preset) =
        prepare_prompt(&repo_root, cli.preset.as_ref(), &selected_mode, &base_prompt);
    print_run_header(
        &selected_mode,
        header_model,
        header_api_mode,
        effective_max_steps(&selected_mode, cli.max_steps),
        header_auto_approve,
        header_verbose_tools,
        effective_preset
            .as_ref()
            .and_then(|p| p.to_possible_value())
            .map(|v| v.get_name().to_string()),
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
    println!("  Use Up/Down arrow keys to browse command history in TTY mode.");
    println!("  Press Tab or Ctrl+Q to queue the current line for later execution.");
    println!("  Type `/` or `/help` to list slash commands.");
    println!();
    println!("Queue:");
    println!("  `/queue <text>` adds a task to the FIFO queue.");
    println!("  `/queue-show` lists queued tasks.");
    println!("  `/queue-run` drains queued tasks now.");
    println!("  `/queue-clear` removes queued tasks.");
    println!();
    println!("Modes:");
    println!("  ask   Short Q&A and explanation flows.");
    println!("  edit  Multi-step with explicit edits and approvals.");
    println!("  agent Full iterative tool-calling loop.");
    println!();
    println!("Safety:");
    println!("  Approval gates apply to risky tools.");
    println!("  --auto-approve bypasses prompts for this run.");
    println!("  --verbose-tools / --no-verbose-tools toggle live tool traces and tool outputs.");
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
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        return run_interactive_shell_with_editor(client, state).await;
    }
    run_interactive_shell_plain(client, &mut state).await
}

async fn run_interactive_shell_with_editor(
    client: &XaiClient,
    mut state: InteractiveState,
) -> Result<()> {
    let history_path = interactive_history_path()?;
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut editor = DefaultEditor::new()?;
    if history_path.exists() {
        let _ = editor.load_history(&history_path);
    }
    let queue_signal = Arc::new(Mutex::new(None));
    let queue_handler = QueueCurrentLineHandler {
        signal: queue_signal.clone(),
    };
    editor.bind_sequence(
        KeyEvent::from('\t'),
        EventHandler::Conditional(Box::new(queue_handler.clone())),
    );
    editor.bind_sequence(
        KeyEvent::ctrl('Q'),
        EventHandler::Conditional(Box::new(queue_handler)),
    );

    loop {
        clear_readline_action(&queue_signal);
        match editor.readline("grok> ") {
            Ok(line) => {
                let action = take_readline_action(&queue_signal);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(line);
                if action == Some(ReadlineAction::QueueCurrentLine) {
                    enqueue_task(&mut state, line);
                    continue;
                }
                let outcome =
                    handle_interactive_line(client, &mut state, line, InputSource::Immediate)
                        .await?;
                match outcome {
                    LineOutcome::Continue => {}
                    LineOutcome::DrainQueue => {
                        if drain_queued_tasks(client, &mut state).await? {
                            break;
                        }
                    }
                    LineOutcome::Exit => break,
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                break;
            }
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(err) => return Err(err.into()),
        }
    }

    let _ = editor.save_history(&history_path);
    Ok(())
}

async fn run_interactive_shell_plain(
    client: &XaiClient,
    state: &mut InteractiveState,
) -> Result<()> {
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
        match handle_interactive_line(client, state, line, InputSource::Immediate).await? {
            LineOutcome::Continue => {}
            LineOutcome::DrainQueue => {
                if drain_queued_tasks(client, state).await? {
                    break;
                }
            }
            LineOutcome::Exit => break,
        }
    }
    Ok(())
}

async fn handle_interactive_line(
    client: &XaiClient,
    state: &mut InteractiveState,
    line: &str,
    source: InputSource,
) -> Result<LineOutcome> {
    match parse_interactive_command(line) {
        Ok(InteractiveCommand::Help) => print_interactive_help(),
        Ok(InteractiveCommand::Show) => print_interactive_state(state),
        Ok(InteractiveCommand::Exit) => return Ok(LineOutcome::Exit),
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
        Ok(InteractiveCommand::VerboseTools(value)) => {
            state.cfg.verbose_tools = value;
            println!("verbose_tools={}", state.cfg.verbose_tools);
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
        Ok(InteractiveCommand::Queue(text)) => enqueue_task(state, &text),
        Ok(InteractiveCommand::QueueShow) => print_queue(state),
        Ok(InteractiveCommand::QueueClear) => {
            let cleared = state.queued_tasks.len();
            state.queued_tasks.clear();
            println!("cleared {cleared} queued task(s)");
        }
        Ok(InteractiveCommand::QueueRun) => {
            if state.queued_tasks.is_empty() {
                println!("queue is empty");
                return Ok(LineOutcome::Continue);
            }
            if matches!(source, InputSource::Queued) {
                println!("queue already running");
            } else {
                return Ok(LineOutcome::DrainQueue);
            }
        }
        Ok(InteractiveCommand::Prompt(prompt)) => {
            let runtime = AgentRuntime::new(
                state.cfg.clone(),
                effective_max_steps(&state.mode, state.max_steps),
            );
            let repo_root = env::current_dir()?;
            let (prompt, effective_preset) =
                prepare_prompt(&repo_root, state.preset.as_ref(), &state.mode, &prompt);
            if state.preset.is_none() {
                if let Some(preset) = effective_preset.as_ref() {
                    if let Some(value) = preset.to_possible_value() {
                        eprintln!("inferred_preset={}", value.get_name());
                    }
                }
            }
            runtime
                .run_ask(client, prompt, Some(state.session.clone()))
                .await?;
            if matches!(source, InputSource::Immediate) && !state.queued_tasks.is_empty() {
                return Ok(LineOutcome::DrainQueue);
            }
        }
        Err(err) => println!("{err}"),
    }
    Ok(LineOutcome::Continue)
}

fn print_interactive_banner(state: &InteractiveState) {
    println!("Interactive shell");
    println!("session={}", state.session.path().display());
    println!("type a prompt and press Enter");
    println!("press Tab or Ctrl+Q to queue the current line");
    println!("use Up/Down to browse history");
    println!("type / or /help for commands, /exit to quit");
    print_interactive_state(state);
}

fn print_interactive_help() {
    println!("Use Up/Down arrow keys for history in TTY mode.");
    println!("Press Tab or Ctrl+Q to queue the current line.");
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
    println!("/verbose-tools <on|off>");
    println!("/stream <on|off>");
    println!("/preset <rust-tests|swift-build|review-changed|off>");
    println!("/context-budget <bytes>");
    println!("/queue <text>");
    println!("/queue-show");
    println!("/queue-run");
    println!("/queue-clear");
}

fn print_interactive_state(state: &InteractiveState) {
    let preset = state
        .preset
        .as_ref()
        .and_then(|p| p.to_possible_value())
        .map(|v| v.get_name().to_string())
        .unwrap_or_else(|| "off".to_string());
    println!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={} verbose_tools={} stream={} preset={} context_budget_bytes={} queued={}",
        mode_name(&state.mode),
        state.cfg.model,
        state.cfg.api_mode,
        effective_max_steps(&state.mode, state.max_steps),
        state.cfg.auto_approve,
        state.cfg.verbose_tools,
        state.cfg.stream,
        preset,
        state.cfg.context_budget_bytes,
        state.queued_tasks.len(),
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
        "/new-session" | "/new_session" => Ok(InteractiveCommand::NewSession),
        "/resume-latest" | "/resume_latest" => Ok(InteractiveCommand::ResumeLatest),
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
        "/api-mode" | "/api_mode" => parts
            .next()
            .ok_or_else(|| "usage: /api-mode <responses|chat-completions>".to_string())
            .and_then(parse_api_mode_command),
        "/mode" => parts
            .next()
            .ok_or_else(|| "usage: /mode <ask|edit|agent>".to_string())
            .and_then(parse_mode_command),
        "/max-steps" | "/max_steps" => parts
            .next()
            .ok_or_else(|| "usage: /max-steps <n>".to_string())
            .and_then(|s| s.parse::<u32>().map(InteractiveCommand::MaxSteps).map_err(|_| "invalid step count".to_string())),
        "/auto-approve" | "/auto_approve" => parts
            .next()
            .ok_or_else(|| "usage: /auto-approve <on|off>".to_string())
            .and_then(parse_bool_command)
            .map(InteractiveCommand::AutoApprove),
        "/verbose-tools" | "/verbose_tools" => parts
            .next()
            .ok_or_else(|| "usage: /verbose-tools <on|off>".to_string())
            .and_then(parse_bool_command)
            .map(InteractiveCommand::VerboseTools),
        "/stream" => parts
            .next()
            .ok_or_else(|| "usage: /stream <on|off>".to_string())
            .and_then(parse_bool_command)
            .map(InteractiveCommand::Stream),
        "/preset" => parts
            .next()
            .ok_or_else(|| "usage: /preset <rust-tests|swift-build|review-changed|off>".to_string())
            .and_then(parse_preset_command),
        "/context-budget" | "/context_budget" => parts
            .next()
            .ok_or_else(|| "usage: /context-budget <bytes>".to_string())
            .and_then(|s| s.parse::<usize>().map(InteractiveCommand::ContextBudget).map_err(|_| "invalid byte count".to_string())),
        "/queue" => {
            let rest = line.trim_start_matches("/queue").trim();
            if rest.is_empty() {
                Err("usage: /queue <text>".to_string())
            } else {
                Ok(InteractiveCommand::Queue(rest.to_string()))
            }
        }
        "/queue-show" | "/queue_show" => Ok(InteractiveCommand::QueueShow),
        "/queue-run" | "/queue_run" => Ok(InteractiveCommand::QueueRun),
        "/queue-clear" | "/queue_clear" => Ok(InteractiveCommand::QueueClear),
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

fn clear_readline_action(signal: &Arc<Mutex<Option<ReadlineAction>>>) {
    if let Ok(mut state) = signal.lock() {
        *state = None;
    }
}

fn take_readline_action(signal: &Arc<Mutex<Option<ReadlineAction>>>) -> Option<ReadlineAction> {
    signal.lock().ok().and_then(|mut state| state.take())
}

fn enqueue_task(state: &mut InteractiveState, line: &str) {
    state.queued_tasks.push_back(line.to_string());
    println!(
        "queued[{}]: {}",
        state.queued_tasks.len(),
        summarize_queue_entry(line)
    );
}

fn print_queue(state: &InteractiveState) {
    if state.queued_tasks.is_empty() {
        println!("queue is empty");
        return;
    }

    for (index, entry) in state.queued_tasks.iter().enumerate() {
        println!("{}: {}", index + 1, summarize_queue_entry(entry));
    }
}

async fn drain_queued_tasks(client: &XaiClient, state: &mut InteractiveState) -> Result<bool> {
    let mut processed = 0usize;
    while let Some(line) = state.queued_tasks.pop_front() {
        processed += 1;
        println!(
            "running queued[{}] remaining={} {}",
            processed,
            state.queued_tasks.len(),
            summarize_queue_entry(&line)
        );
        match handle_interactive_line(client, state, &line, InputSource::Queued).await? {
            LineOutcome::Continue => {}
            LineOutcome::DrainQueue => {}
            LineOutcome::Exit => return Ok(true),
        }
    }

    Ok(false)
}

fn summarize_queue_entry(line: &str) -> String {
    const MAX_CHARS: usize = 96;
    let trimmed = line.trim();
    let mut preview = trimmed.chars().take(MAX_CHARS).collect::<String>();
    if trimmed.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    if preview.is_empty() {
        "<empty>".to_string()
    } else {
        preview
    }
}

fn interactive_history_path() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".local/share/grok-agent/history.txt"))
}

fn prepare_prompt(
    repo_root: &std::path::Path,
    preset: Option<&WorkflowPreset>,
    mode: &AgentMode,
    prompt: &str,
) -> (String, Option<WorkflowPreset>) {
    let effective_preset = match preset {
        Some(preset) => Some(preset.clone()),
        None if !matches!(mode, AgentMode::Ask) => infer_preset_for_prompt(repo_root, prompt),
        None => None,
    };
    (
        apply_selected_preset(effective_preset.as_ref(), prompt),
        effective_preset,
    )
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
    verbose_tools: bool,
    effective_preset: Option<String>,
    resumed_session: Option<String>,
) {
    eprintln!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={} verbose_tools={}",
        mode_name(mode), model, api_mode, max_steps, auto_approve, verbose_tools
    );
    if let Some(preset) = effective_preset {
        eprintln!("effective_preset={preset}");
    }
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
    fn parse_queue_command() {
        match parse_interactive_command("/queue fix the failing tests next") {
            Ok(InteractiveCommand::Queue(task)) => {
                assert_eq!(task, "fix the failing tests next")
            }
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_verbose_tools_command() {
        match parse_interactive_command("/verbose-tools off") {
            Ok(InteractiveCommand::VerboseTools(value)) => assert!(!value),
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_max_steps_alias() {
        match parse_interactive_command("/max_steps 12") {
            Ok(InteractiveCommand::MaxSteps(steps)) => assert_eq!(steps, 12),
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

    #[test]
    fn prepare_prompt_infers_swift_build_for_non_ask_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Package.swift"), "// swift-tools-version:5.3\n")
            .expect("write Package.swift");
        let (prompt, preset) = prepare_prompt(
            dir.path(),
            None,
            &AgentMode::Agent,
            "swift test fails with cannot find 'Application' in scope",
        );
        assert!(prompt.contains("Swift build/test fix workflow."));
        assert_eq!(preset, Some(WorkflowPreset::SwiftBuild));
    }

    #[test]
    fn prepare_prompt_does_not_infer_for_ask_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Package.swift"), "// swift-tools-version:5.3\n")
            .expect("write Package.swift");
        let (prompt, preset) = prepare_prompt(
            dir.path(),
            None,
            &AgentMode::Ask,
            "swift test fails with cannot find 'Application' in scope",
        );
        assert_eq!(
            prompt,
            "swift test fails with cannot find 'Application' in scope"
        );
        assert_eq!(preset, None);
    }
}
