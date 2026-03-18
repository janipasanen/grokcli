use anyhow::Result;
use clap::{Parser, ValueEnum};
use grokcli::agent::runtime::{AgentRuntime, ApprovalHandler};
use grokcli::config::config::AppConfig;
use grokcli::output::{ExternalPrinterSink, OutputSink};
use grokcli::persistence::patch_store::PatchStore;
use grokcli::persistence::session_store::{
    SessionStore, latest_session_path, resolve_session_path,
};
use grokcli::provider::xai_client::XaiClient;
use grokcli::workflows::presets::{WorkflowPreset, apply_preset_prompt, infer_preset_for_prompt};
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

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
    Approve(bool),
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
    queued_tasks: Arc<Mutex<VecDeque<QueuedPromptTask>>>,
}

#[derive(Debug, Clone)]
struct QueuedPromptTask {
    prompt: String,
    cfg: AppConfig,
    mode: AgentMode,
    max_steps: u32,
    preset: Option<WorkflowPreset>,
    session: SessionStore,
}

struct PendingApproval {
    responder: mpsc::Sender<bool>,
}

#[derive(Clone)]
struct InteractiveApprovalHandler {
    pending: Arc<Mutex<Option<PendingApproval>>>,
    output: Arc<dyn OutputSink>,
}

#[derive(Clone)]
struct BackgroundTaskRunner {
    client: XaiClient,
    output: Arc<dyn OutputSink>,
    approval_handler: Arc<InteractiveApprovalHandler>,
    running: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadlineAction {
    QueueCurrentLine,
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

impl ApprovalHandler for InteractiveApprovalHandler {
    fn request_approval(&self, action: &str, reason: Option<&str>) -> Result<bool> {
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("interactive approval state poisoned"))?;
            *pending = Some(PendingApproval { responder: tx });
        }

        match reason {
            Some(reason) if !reason.trim().is_empty() => self.output.stderr_line(&format!(
                "approval required for {action}: {reason} (reply with /approve yes or /approve no)"
            )),
            _ => self.output.stderr_line(&format!(
                "approval required for {action} (reply with /approve yes or /approve no)"
            )),
        }

        let approved = rx.recv().unwrap_or(false);
        Ok(approved)
    }
}

impl InteractiveApprovalHandler {
    fn approve(&self, approved: bool) -> bool {
        let pending = match self.pending.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        let Some(pending) = pending else {
            return false;
        };
        let _ = pending.responder.send(approved);
        true
    }
}

impl BackgroundTaskRunner {
    fn start_if_idle(&self, state: InteractiveState) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let runner = self.clone();
        tokio::spawn(async move {
            runner.run(state).await;
        });
    }

    async fn run(self, state: InteractiveState) {
        let mut processed = 0usize;
        loop {
            let task = {
                let mut queued = match state.queued_tasks.lock() {
                    Ok(guard) => guard,
                    Err(_) => break,
                };
                queued.pop_front()
            };

            let Some(mut task) = task else {
                self.running.store(false, Ordering::SeqCst);
                let should_restart = match state.queued_tasks.lock() {
                    Ok(queued) => !queued.is_empty(),
                    Err(_) => false,
                };
                if should_restart
                    && self
                        .running
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    continue;
                }
                break;
            };

            processed += 1;
            let remaining = queued_task_count(&state);
            self.output.stderr_line(&format!(
                "running queued[{}] remaining={} {}",
                processed,
                remaining,
                summarize_queue_entry(&task.prompt)
            ));

            // Background queueing works best with complete-line output so the prompt can be redrawn cleanly.
            task.cfg.stream = false;

            let runtime = AgentRuntime::new(
                task.cfg.clone(),
                effective_max_steps(&task.mode, task.max_steps),
            )
            .with_output(self.output.clone())
            .with_approval_handler(self.approval_handler.clone());
            let repo_root = match env::current_dir() {
                Ok(path) => path,
                Err(err) => {
                    self.output
                        .stderr_line(&format!("failed to resolve current directory: {err}"));
                    continue;
                }
            };
            let (prompt, effective_preset) =
                prepare_prompt(&repo_root, task.preset.as_ref(), &task.mode, &task.prompt);
            maybe_print_inferred_preset_with_output(
                &*self.output,
                task.preset.as_ref(),
                effective_preset.as_ref(),
            );
            let client = self.client.with_output(self.output.clone());
            if let Err(err) = runtime.run_ask(&client, prompt, Some(task.session)).await {
                self.output.stderr_line(&format!("task failed: {err}"));
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "grokcli")]
#[command(about = "Terminal-native Grok agent scaffold")]
#[command(
    long_about = "Grok terminal coding agent prototype.\n\nThis CLI runs a deterministic local tool loop with xAI as planner. It supports safe local tool execution, approval-gated actions, patch application, resumable sessions, and an interactive shell when started without a prompt."
)]
#[command(
    after_long_help = "Help Sections\n\nInteractive:\n  Run `grokcli` with no prompt to enter the interactive shell.\n  Type `/` or `/help` to see slash commands.\n  In TTY mode, Enter submits the current prompt into the background queue, Up/Down recall history, and Tab or Ctrl+Q queue the current line without starting it.\n\nModes:\n  ask   Single-turn Q&A or short explanation. Prefer read-only behavior.\n  edit  Multi-step with edits after approval.\n  agent Full iterative tool-calling loop until stop conditions.\n\nSafety:\n  Tiered command policy is enforced by local tools.\n  Use --auto-approve only in trusted repos.\n  Use --verbose-tools or --no-verbose-tools to control detailed tool previews.\n  Use --queue <prompt> to batch follow-up prompts in non-interactive mode.\n  Use /approve <yes|no> to answer approval requests from queued interactive work.\n\nSessions:\n  Each run writes JSONL events under ~/.local/share/grok-agent/sessions.\n  Use --resume-latest or --resume-session <id|path> to continue."
)]
struct Cli {
    #[arg(long, help_heading = "Config", help = "Path to a TOML config file.")]
    config: Option<PathBuf>,
    #[arg(
        long,
        help_heading = "Model",
        help = "Override model name for this run."
    )]
    model: Option<String>,
    #[arg(
        long,
        value_enum,
        help_heading = "Runtime",
        help = "High-level runtime mode."
    )]
    mode: Option<AgentMode>,
    #[arg(
        long,
        value_enum,
        help_heading = "Model",
        help = "Provider API mode override."
    )]
    api_mode: Option<ApiModeOpt>,
    #[arg(
        long,
        help_heading = "Model",
        help = "Disable streaming output where supported."
    )]
    no_stream: bool,
    #[arg(
        long,
        help_heading = "Safety",
        help = "Automatically approve approval-gated tool calls."
    )]
    auto_approve: bool,
    #[arg(
        long,
        help_heading = "Runtime",
        help = "Print detailed tool result previews during execution. Concise tool action/result lines are always shown."
    )]
    verbose_tools: bool,
    #[arg(
        long,
        help_heading = "Runtime",
        help = "Use concise tool action/result lines without detailed tool previews.",
        conflicts_with = "verbose_tools"
    )]
    no_verbose_tools: bool,
    #[arg(
        long = "queue",
        help_heading = "Runtime",
        help = "Queue an additional prompt to run after the main prompt. Can be passed more than once."
    )]
    queue_prompts: Vec<String>,
    #[arg(
        long,
        help_heading = "Patches",
        help = "Reverse the most recent recorded patch for this repo and exit."
    )]
    undo_last_patch: bool,
    #[arg(
        long,
        help_heading = "Sessions",
        help = "Resume a specific session by id (timestamp) or file path."
    )]
    resume_session: Option<String>,
    #[arg(
        long,
        help_heading = "Sessions",
        help = "Resume the latest session from session storage."
    )]
    resume_latest: bool,
    #[arg(
        long,
        value_enum,
        help_heading = "Presets",
        help = "Apply a workflow prompt preset."
    )]
    preset: Option<WorkflowPreset>,
    #[arg(
        long,
        default_value_t = 8,
        help_heading = "Runtime",
        help = "Maximum agent loop steps."
    )]
    max_steps: u32,
    #[arg(
        long,
        help_heading = "Runtime",
        help = "Override context budget in bytes for tool outputs."
    )]
    context_budget_bytes: Option<usize>,
    #[arg(
        long,
        help_heading = "Help",
        help = "Show extended help sections and exit."
    )]
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
    let has_primary_prompt = cli
        .prompt
        .as_ref()
        .map(|prompt| !prompt.trim().is_empty())
        .unwrap_or(false);
    let cli_tasks = collect_cli_tasks(cli.prompt.clone(), cli.queue_prompts.clone());
    let total_tasks = cli_tasks.len();

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
    if cli_tasks.is_empty() {
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
            queued_tasks: Arc::new(Mutex::new(VecDeque::new())),
        };
        return run_interactive_shell(&client, state).await;
    }

    let runtime = AgentRuntime::new(cfg, effective_max_steps(&selected_mode, cli.max_steps));
    let repo_root = env::current_dir()?;
    let first_task = cli_tasks
        .first()
        .ok_or_else(|| anyhow::anyhow!("at least one prompt is required"))?;
    let (_, effective_preset) =
        prepare_prompt(&repo_root, cli.preset.as_ref(), &selected_mode, first_task);
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
        total_tasks.saturating_sub(usize::from(has_primary_prompt)),
        resume_store
            .as_ref()
            .map(|s| s.path().to_string_lossy().to_string()),
    );
    let session = match resume_store {
        Some(store) => store,
        None => SessionStore::for_new_session()?,
    };
    for (index, base_prompt) in cli_tasks.into_iter().enumerate() {
        maybe_print_cli_queue_notice(
            has_primary_prompt,
            index,
            total_tasks.saturating_sub(index + 1),
            &base_prompt,
        );
        let (prompt, effective_preset) = prepare_prompt(
            &repo_root,
            cli.preset.as_ref(),
            &selected_mode,
            &base_prompt,
        );
        maybe_print_inferred_preset(cli.preset.as_ref(), effective_preset.as_ref(), index);
        runtime
            .run_ask(&client, prompt, Some(session.clone()))
            .await?;
    }

    Ok(())
}

fn print_help_sections() {
    println!("Grok CLI Help Sections");
    println!();
    println!("Interactive:");
    println!("  Run `grokcli` with no prompt to enter the interactive shell.");
    println!("  Type a normal prompt and press Enter to queue and run it.");
    println!("  Use Up/Down arrow keys to browse command history in TTY mode.");
    println!("  Press Tab or Ctrl+Q to queue the current line for later execution.");
    println!("  Type `/` or `/help` to list slash commands.");
    println!();
    println!("Queue:");
    println!("  `/queue <text>` adds a task to the FIFO queue.");
    println!("  `/queue-show` lists queued tasks.");
    println!("  `/queue-run` drains queued tasks now.");
    println!("  `/queue-clear` removes queued tasks.");
    println!("  `/approve <yes|no>` answers an approval request from a running queued task.");
    println!();
    println!("Modes:");
    println!("  ask   Short Q&A and explanation flows.");
    println!("  edit  Multi-step with explicit edits and approvals.");
    println!("  agent Full iterative tool-calling loop.");
    println!();
    println!("Safety:");
    println!("  Approval gates apply to risky tools.");
    println!("  --auto-approve bypasses prompts for this run.");
    println!("  --verbose-tools / --no-verbose-tools toggle detailed tool previews.");
    println!("  --queue <text> adds a queued prompt for non-interactive runs.");
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
    let background_runner = match editor.create_external_printer() {
        Ok(printer) => {
            let output: Arc<dyn OutputSink> = Arc::new(ExternalPrinterSink::new(printer));
            let approval_handler = Arc::new(InteractiveApprovalHandler {
                pending: Arc::new(Mutex::new(None)),
                output: output.clone(),
            });
            Some(BackgroundTaskRunner {
                client: client.clone(),
                output,
                approval_handler,
                running: Arc::new(AtomicBool::new(false)),
            })
        }
        Err(err) => {
            eprintln!("external printer unavailable, using foreground prompt execution: {err}");
            None
        }
    };
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
                    enqueue_task(&state, line);
                    continue;
                }
                let outcome =
                    handle_interactive_line(client, &mut state, line, background_runner.as_ref())
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
        match handle_interactive_line(client, state, line, None).await? {
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
    background_runner: Option<&BackgroundTaskRunner>,
) -> Result<LineOutcome> {
    match parse_interactive_command(line) {
        Ok(InteractiveCommand::Help) => print_interactive_help(),
        Ok(InteractiveCommand::Show) => print_interactive_state(state),
        Ok(InteractiveCommand::Exit) => return Ok(LineOutcome::Exit),
        Ok(InteractiveCommand::Approve(approved)) => {
            if let Some(runner) = background_runner {
                if runner.approval_handler.approve(approved) {
                    println!("approval={approved}");
                } else {
                    println!("no approval request pending");
                }
            } else {
                println!("no approval request pending");
            }
        }
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
                Some(preset) => {
                    println!("preset={}", preset.to_possible_value().unwrap().get_name())
                }
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
            let cleared = clear_queued_tasks(state);
            println!("cleared {cleared} queued task(s)");
        }
        Ok(InteractiveCommand::QueueRun) => {
            if queued_task_count(state) == 0 {
                println!("queue is empty");
                return Ok(LineOutcome::Continue);
            }
            if let Some(runner) = background_runner {
                runner.start_if_idle(state.clone());
            } else {
                return Ok(LineOutcome::DrainQueue);
            }
        }
        Ok(InteractiveCommand::Prompt(prompt)) => {
            if let Some(runner) = background_runner {
                enqueue_task(state, &prompt);
                runner.start_if_idle(state.clone());
                return Ok(LineOutcome::Continue);
            }
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
            if queued_task_count(state) > 0 {
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
    println!("type a prompt and press Enter to queue and run it");
    println!("use /queue <text> or press Tab/Ctrl+Q to queue work");
    println!("use Up/Down to browse history");
    println!("type / or /help for commands, /exit to quit");
    print_interactive_state(state);
}

fn print_interactive_help() {
    println!("Use Up/Down arrow keys for history in TTY mode.");
    println!("Press Enter to queue the prompt and start work immediately.");
    println!("Use /queue <text> or press Tab/Ctrl+Q to queue additional work.");
    println!("/help");
    println!("/show");
    println!("/approve <yes|no>");
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
        queued_task_count(state),
    );
}

fn collect_cli_tasks(prompt: Option<String>, queued_prompts: Vec<String>) -> Vec<String> {
    let mut tasks = Vec::new();
    if let Some(prompt) = prompt {
        if !prompt.trim().is_empty() {
            tasks.push(prompt);
        }
    }
    for prompt in queued_prompts {
        if !prompt.trim().is_empty() {
            tasks.push(prompt);
        }
    }
    tasks
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
        "/approve" => parts
            .next()
            .ok_or_else(|| "usage: /approve <yes|no>".to_string())
            .and_then(parse_approve_command),
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
            .and_then(|s| {
                s.parse::<u32>()
                    .map(InteractiveCommand::MaxSteps)
                    .map_err(|_| "invalid step count".to_string())
            }),
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
            .and_then(|s| {
                s.parse::<usize>()
                    .map(InteractiveCommand::ContextBudget)
                    .map_err(|_| "invalid byte count".to_string())
            }),
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

fn parse_approve_command(value: &str) -> Result<InteractiveCommand, String> {
    match value {
        "yes" | "y" => Ok(InteractiveCommand::Approve(true)),
        "no" | "n" => Ok(InteractiveCommand::Approve(false)),
        _ => Err("usage: /approve <yes|no>".to_string()),
    }
}

fn parse_preset_command(value: &str) -> Result<InteractiveCommand, String> {
    match value {
        "rust-tests" => Ok(InteractiveCommand::Preset(Some(WorkflowPreset::RustTests))),
        "swift-build" => Ok(InteractiveCommand::Preset(Some(WorkflowPreset::SwiftBuild))),
        "review-changed" => Ok(InteractiveCommand::Preset(Some(
            WorkflowPreset::ReviewChanged,
        ))),
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

fn enqueue_task(state: &InteractiveState, line: &str) {
    if let Ok(mut queued) = state.queued_tasks.lock() {
        queued.push_back(snapshot_queued_task(state, line));
    }
    println!(
        "queued[{}]: {}",
        queued_task_count(state),
        summarize_queue_entry(line)
    );
}

fn print_queue(state: &InteractiveState) {
    let queued = queued_task_summaries(state);
    if queued.is_empty() {
        println!("queue is empty");
        return;
    }

    for (index, entry) in queued.iter().enumerate() {
        println!("{}: {}", index + 1, entry);
    }
}

async fn drain_queued_tasks(client: &XaiClient, state: &mut InteractiveState) -> Result<bool> {
    let mut processed = 0usize;
    while let Some(task) = pop_queued_task(state) {
        processed += 1;
        println!(
            "running queued[{}] remaining={} {}",
            processed,
            queued_task_count(state),
            summarize_queue_entry(&task.prompt)
        );
        run_task_foreground(client, task).await?;
    }

    Ok(false)
}

fn snapshot_queued_task(state: &InteractiveState, line: &str) -> QueuedPromptTask {
    QueuedPromptTask {
        prompt: line.to_string(),
        cfg: state.cfg.clone(),
        mode: state.mode.clone(),
        max_steps: state.max_steps,
        preset: state.preset.clone(),
        session: state.session.clone(),
    }
}

fn queued_task_count(state: &InteractiveState) -> usize {
    state
        .queued_tasks
        .lock()
        .map(|queued| queued.len())
        .unwrap_or(0)
}

fn queued_task_summaries(state: &InteractiveState) -> Vec<String> {
    state
        .queued_tasks
        .lock()
        .map(|queued| {
            queued
                .iter()
                .map(|task| summarize_queue_entry(&task.prompt))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn pop_queued_task(state: &InteractiveState) -> Option<QueuedPromptTask> {
    state
        .queued_tasks
        .lock()
        .ok()
        .and_then(|mut queued| queued.pop_front())
}

fn clear_queued_tasks(state: &InteractiveState) -> usize {
    match state.queued_tasks.lock() {
        Ok(mut queued) => {
            let cleared = queued.len();
            queued.clear();
            cleared
        }
        Err(_) => 0,
    }
}

async fn run_task_foreground(client: &XaiClient, task: QueuedPromptTask) -> Result<()> {
    let runtime = AgentRuntime::new(
        task.cfg.clone(),
        effective_max_steps(&task.mode, task.max_steps),
    );
    let repo_root = env::current_dir()?;
    let (prompt, effective_preset) =
        prepare_prompt(&repo_root, task.preset.as_ref(), &task.mode, &task.prompt);
    maybe_print_inferred_preset_direct(task.preset.as_ref(), effective_preset.as_ref());
    runtime.run_ask(client, prompt, Some(task.session)).await
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

fn maybe_print_cli_queue_notice(
    has_primary_prompt: bool,
    index: usize,
    remaining: usize,
    prompt: &str,
) {
    if has_primary_prompt && index == 0 {
        return;
    }
    let queue_index = if has_primary_prompt { index } else { index + 1 };
    eprintln!(
        "running queued[{}] remaining={} {}",
        queue_index,
        remaining,
        summarize_queue_entry(prompt)
    );
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
    queued_tasks: usize,
    resumed_session: Option<String>,
) {
    eprintln!(
        "mode={} model={} api_mode={} max_steps={} auto_approve={} verbose_tools={} queued={}",
        mode_name(mode),
        model,
        api_mode,
        max_steps,
        auto_approve,
        verbose_tools,
        queued_tasks
    );
    if let Some(preset) = effective_preset {
        eprintln!("effective_preset={preset}");
    }
    if let Some(path) = resumed_session {
        eprintln!("resuming_session={}", path);
    }
}

fn maybe_print_inferred_preset(
    configured_preset: Option<&WorkflowPreset>,
    effective_preset: Option<&WorkflowPreset>,
    task_index: usize,
) {
    if configured_preset.is_some() || task_index == 0 {
        return;
    }
    if let Some(preset) = effective_preset {
        if let Some(value) = preset.to_possible_value() {
            eprintln!("inferred_preset={}", value.get_name());
        }
    }
}

fn maybe_print_inferred_preset_direct(
    configured_preset: Option<&WorkflowPreset>,
    effective_preset: Option<&WorkflowPreset>,
) {
    if configured_preset.is_some() {
        return;
    }
    if let Some(preset) = effective_preset {
        if let Some(value) = preset.to_possible_value() {
            eprintln!("inferred_preset={}", value.get_name());
        }
    }
}

fn maybe_print_inferred_preset_with_output(
    output: &dyn OutputSink,
    configured_preset: Option<&WorkflowPreset>,
    effective_preset: Option<&WorkflowPreset>,
) {
    if configured_preset.is_some() {
        return;
    }
    if let Some(preset) = effective_preset {
        if let Some(value) = preset.to_possible_value() {
            output.stderr_line(&format!("inferred_preset={}", value.get_name()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_help_command() {
        match parse_interactive_command("/") {
            Ok(InteractiveCommand::Help) => {}
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

    #[test]
    fn parse_approve_command() {
        match parse_interactive_command("/approve yes") {
            Ok(InteractiveCommand::Approve(value)) => assert!(value),
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
        std::fs::write(
            dir.path().join("Package.swift"),
            "// swift-tools-version:5.3\n",
        )
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
        std::fs::write(
            dir.path().join("Package.swift"),
            "// swift-tools-version:5.3\n",
        )
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

    #[test]
    fn cli_accepts_no_verbose_tools_with_prompt() {
        let cli =
            Cli::try_parse_from(["grokcli", "--no-verbose-tools", "fix failing tests"]).unwrap();
        assert!(cli.no_verbose_tools);
        assert_eq!(cli.prompt.as_deref(), Some("fix failing tests"));
    }

    #[test]
    fn cli_collects_queued_prompts() {
        let cli = Cli::try_parse_from([
            "grokcli",
            "--queue",
            "inspect build",
            "--queue",
            "rerun tests",
            "fix failing tests",
        ])
        .unwrap();
        assert_eq!(cli.queue_prompts.len(), 2);
        assert_eq!(cli.queue_prompts[0], "inspect build");
        assert_eq!(cli.queue_prompts[1], "rerun tests");
    }

    #[test]
    fn collect_cli_tasks_keeps_prompt_then_queue_order() {
        let tasks = collect_cli_tasks(
            Some("fix failing tests".to_string()),
            vec!["inspect build".to_string(), "rerun tests".to_string()],
        );
        assert_eq!(
            tasks,
            vec![
                "fix failing tests".to_string(),
                "inspect build".to_string(),
                "rerun tests".to_string()
            ]
        );
    }
}
