# Grok 4.1 Fast Terminal Agent

Architecture and implementation instructions for a terminal-based agentic development tool in the spirit of Codex CLI, but using xAI's Grok 4.1 Fast through the xAI API.

---

## 1. Purpose

Build a local-first terminal tool that helps with real software development tasks:

- inspect and explain codebases
- propose and apply patches
- run build, test, lint, and format commands
- search project files and git history
- execute safe shell commands with explicit guardrails
- stream reasoning-friendly progress to the terminal
- maintain task state across multiple steps
- optionally operate in interactive or autonomous mode

The tool should feel like a development copilot that lives in the terminal, not a generic chatbot.

---

## 2. Product goals

### Primary goals

1. **Terminal native**
   - fast startup
   - keyboard-first UX
   - readable streaming output
   - works well over SSH and tmux

2. **Agentic by default**
   - model can inspect files, propose edits, run commands, evaluate outputs, and continue until a stopping condition is reached

3. **Safe local execution**
   - all destructive actions require approval unless explicitly whitelisted
   - default sandbox mode for shell commands and file writes

4. **Repository aware**
   - understands git diff, status, branches, staged files, and test results

5. **Deterministic orchestration**
   - agent loop is implemented in the client, not hidden in prompts
   - all tool calls are logged, replayable, and auditable

### Non-goals

- replacing the user's editor or IDE
- fully autonomous long-running background coding without visibility
- remote code execution outside the local machine unless explicitly configured

---

## 3. Key external dependency

The tool uses the xAI API as the model backend.

### Recommended API approach

Use the **Responses API** as the primary integration path.

Why:

- it is the recommended xAI API path for new applications
- it supports stateful multi-turn conversations
- it supports agentic tools better than the legacy chat-completions flow
- it avoids resending full history every turn when continuing with previous response identifiers

### Authentication model

The terminal application should authenticate with an **xAI API key** provided by the user.

Practical flow:

1. user creates an xAI account
2. user loads credits in xAI console
3. user creates an API key
4. user exports the key locally, for example:

```bash
export XAI_API_KEY="..."
```

5. terminal client reads it from environment variables or the OS keychain

Do **not** implement browser-style login to Grok.com in the CLI. For a developer tool, the stable and supportable approach is API-key based authentication.

---

## 4. High-level architecture

```text
+---------------------------+
| Terminal UI / TUI Layer   |
| - prompt input            |
| - streaming output        |
| - approval dialogs        |
| - diff renderer           |
+------------+--------------+
             |
             v
+---------------------------+
| Agent Runtime             |
| - task state              |
| - planner/executor loop   |
| - turn management         |
| - stop conditions         |
| - policy enforcement      |
+------+--------------------+
       |
       +-------------------------------+
       |                               |
       v                               v
+--------------------+         +----------------------+
| Tool Registry      |         | xAI Client           |
| - shell            |         | - auth               |
| - file read/write  |         | - streaming          |
| - grep/search      |         | - responses API      |
| - git              |         | - retry/backoff      |
| - patch apply      |         | - model selection    |
| - test/build/lint  |         +----------------------+
+----------+---------+
           |
           v
+---------------------------+
| Local System Adapters     |
| - filesystem              |
| - process runner          |
| - git binary              |
| - ripgrep / fd            |
| - sandbox / approval      |
+---------------------------+
```

---

## 5. Core modules

### 5.1 Terminal UI layer

Responsibilities:

- accept user prompt and mode flags
- render assistant output as streamed chunks
- show tool invocations in real time
- show command output incrementally
- render diffs before applying edits
- request approval for risky actions
- support commands such as:
  - `/model`
  - `/approve`
  - `/plan`
  - `/context`
  - `/diff`
  - `/undo`
  - `/resume`

Recommended implementation options:

- **Rust** for the CLI binary and TUI if you want performance, portability, and easier process control
- good library candidates: `clap`, `ratatui`, `crossterm`, `serde`, `tokio`, `reqwest`

### 5.2 Agent runtime

Responsibilities:

- maintain active task state
- build model input from user intent + repo context + tool results
- dispatch tool calls requested by the model
- store turn transcript and tool outputs
- enforce max-steps, max-cost, max-runtime, and safety policies
- decide when to ask the user for approval or stop

The runtime should be implemented as a deterministic loop, for example:

```text
collect context
-> call model
-> inspect response
-> if tool call: execute tool
-> append tool result
-> continue
-> if final answer or stop condition: finish
```

### 5.3 xAI client

Responsibilities:

- read `XAI_API_KEY`
- connect to `https://api.x.ai`
- send `POST /v1/responses`
- support streaming mode
- support conversation continuation using previous response identifiers
- handle retries for rate limits and transient network failures
- expose model configuration

Minimum client features:

- timeout control
- structured request/response types
- request ID logging
- exponential backoff with jitter
- optional HTTP proxy support

### 5.4 Tool registry

The model should never access the local machine directly. It should only interact through an explicit tool contract.

Recommended built-in client-side tools:

1. `read_file`
2. `read_files_batch`
3. `write_file`
4. `apply_patch`
5. `list_directory`
6. `search_text`
7. `run_shell_command`
8. `git_status`
9. `git_diff`
10. `git_show`
11. `run_tests`
12. `run_linter`
13. `run_formatter`
14. `get_project_metadata`
15. `ask_user_confirmation`

Optional higher-level tools:

- `build_project`
- `run_targeted_command`
- `search_symbol_index`
- `read_diagnostics_cache`
- `open_recent_failures`

### 5.5 Local system adapters

Each tool should have a thin adapter layer that interacts with the actual system.

Examples:

- `run_shell_command` -> process spawning with stdout/stderr streaming
- `apply_patch` -> unified diff validation + safe apply
- `search_text` -> wraps `rg`
- `list_directory` -> wraps filesystem traversal with ignore rules
- `git_diff` -> wraps `git diff --unified=3`

Keep this layer intentionally dumb. All planning should stay in the runtime, not in the adapters.

---

## 6. Recommended language split

Because you develop in Rust and Swift, use this split:

### Option A: Rust-first implementation

Best overall choice for a Codex-CLI-style tool.

Use Rust for:

- CLI and TUI
- agent runtime
- xAI client
- shell/process orchestration
- patching and file ops
- config and persistence

Why Rust is the better primary implementation language here:

- excellent terminal ecosystem
- strong subprocess control
- easy static binaries
- predictable concurrency with `tokio`
- very good JSON and HTTP tooling
- easier cross-platform CLI distribution than Swift

### Option B: Rust core + Swift companion integration

Use Swift only for optional macOS niceties such as:

- Keychain integration
- Finder / Xcode bridges
- macOS notifications
- sourcekit-lsp or Xcode project helpers

This keeps the core portable while still giving you platform-specific advantages on macOS.

---

## 7. Interaction modes

The tool should support at least three modes.

### 7.1 Ask mode

Single-turn or short multi-turn help.

Examples:

- explain this error
- summarize architecture
- suggest a refactor

No file changes unless the user explicitly asks.

### 7.2 Edit mode

The model can propose and apply edits after approval.

Examples:

- fix the failing test
- migrate this module to async
- refactor duplicated code

### 7.3 Agent mode

The model can iterate through tools until the task is solved or it reaches a limit.

Examples:

- run tests, inspect failures, patch code, rerun tests
- inspect cargo build errors and fix them
- diagnose why a Swift package no longer builds

Agent mode should expose clear stop conditions:

- max steps
- max tool calls
- max shell command count
- max file writes
- max token budget
- user interruption

---

## 8. Prompting and planning model

Do not rely on one giant system prompt. Use a layered structure.

### System instruction layer

Defines:

- tool usage policy
- safety limits
- formatting rules
- requirement to think stepwise through tool results
- requirement to prefer minimal edits
- requirement to avoid inventing file contents

### Session context layer

Includes:

- current working directory
- repo root
- platform and shell info
- active branch
- changed files
- selected files or path filters
- build system metadata

### Task layer

Includes the user's explicit request.

### Tool result layer

Includes structured results from file reads, shell commands, git diffs, and test output.

### Planning policy

The runtime should encourage a pattern like this:

1. inspect before editing
2. prefer targeted reads over loading the whole repo
3. create a short plan for non-trivial tasks
4. execute one step at a time
5. verify with tests or builds when code changed
6. summarize exactly what changed

---

## 9. Tool contract design

Every tool should use strict JSON schemas.

Example schema for `run_shell_command`:

```json
{
  "type": "object",
  "properties": {
    "command": { "type": "string" },
    "working_directory": { "type": "string" },
    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600 },
    "require_approval": { "type": "boolean" }
  },
  "required": ["command", "working_directory"]
}
```

Example tool result shape:

```json
{
  "exit_code": 0,
  "stdout": "...",
  "stderr": "...",
  "duration_ms": 812,
  "was_truncated": false
}
```

Guidelines:

- all tools return machine-readable JSON
- large outputs are truncated with explicit markers
- all file paths are normalized to repo-relative paths when possible
- all writes include before/after hashes for auditability

---

## 10. xAI API integration details

### 10.1 Base configuration

Store this in one provider module.

```text
Base URL: https://api.x.ai
Auth Header: Authorization: Bearer <XAI_API_KEY>
Primary Endpoint: /v1/responses
Model: grok-4.1-fast-reasoning
```

Make the model configurable because xAI model naming can evolve.

Suggested config fields:

```toml
provider = "xai"
model = "grok-4.1-fast-reasoning"
base_url = "https://api.x.ai"
stream = true
max_output_tokens = 4000
temperature = 0.1
parallel_tool_calls = false
store = false
```

Notes:

- `parallel_tool_calls = false` is often easier to reason about in a local CLI executor
- `store = false` is a sensible default for privacy-sensitive local development work unless you explicitly want xAI-side response storage

### 10.2 Example request flow

The runtime should send a request like:

```json
{
  "model": "grok-4.1-fast-reasoning",
  "input": [
    {
      "role": "system",
      "content": [
        { "type": "input_text", "text": "You are a terminal-based software engineering agent..." }
      ]
    },
    {
      "role": "user",
      "content": [
        { "type": "input_text", "text": "Fix the failing Rust tests in this repository." }
      ]
    }
  ],
  "tools": [
    {
      "type": "function",
      "name": "run_shell_command",
      "description": "Run a shell command in the local repo after policy checks.",
      "parameters": {
        "type": "object",
        "properties": {
          "command": { "type": "string" },
          "working_directory": { "type": "string" }
        },
        "required": ["command", "working_directory"]
      }
    }
  ],
  "stream": true,
  "parallel_tool_calls": false,
  "store": false
}
```

### 10.3 Streaming event handling

Implement a stream processor that can:

- render plain output tokens to the terminal
- detect tool call events
- buffer structured tool arguments until complete
- execute the tool
- submit the tool result as the next turn

Keep the stream parser isolated from the UI. The parser should emit internal events like:

- `OutputTextDelta`
- `ToolCallStarted`
- `ToolCallArgumentsDelta`
- `ToolCallCompleted`
- `ResponseCompleted`
- `ResponseFailed`

### 10.4 Conversation continuation

Persist enough state to resume a session.

Store locally:

- session ID
- response IDs
- user prompts
- tool calls and results
- file write history
- approval decisions

This allows:

- `/resume`
- `/undo`
- postmortem debugging
- cost analysis

---

## 11. Safety architecture

A terminal coding agent is useful only if it is safe.

### 11.1 Approval tiers

Define explicit policy levels.

#### Tier 0: always allowed

- read files
- list files
- grep/search
- git status
- git diff
- run harmless metadata commands

#### Tier 1: ask once per session

- run tests
- run formatter
- run linter
- build project

#### Tier 2: ask every time

- write file
- apply patch
- run package manager install
- run commands that modify the worktree

#### Tier 3: blocked unless explicitly enabled

- `rm -rf`
- `git reset --hard`
- `git clean -fd`
- networked deployment commands
- credential-changing commands
- commands outside repo root

### 11.2 Sandbox strategy

Start with policy-based sandboxing first.

Minimum protections:

- working directory restricted to repo root by default
- block shell metacharacter patterns you consider unsafe unless approved
- deny writes outside allowlisted directories
- cap process runtime and output size
- redact secrets from logs where possible

### 11.3 Secret handling

- read `XAI_API_KEY` from environment or secure OS storage
- never print it
- scrub likely secrets from persisted transcripts
- allow `.agentignore` and `.agentsecrets` patterns

---

## 12. Context acquisition strategy

A good coding agent should not load the whole repo blindly.

Use this retrieval order:

1. current directory metadata
2. git status and changed files
3. user-selected files
4. targeted search with `rg`
5. focused file reads
6. build/test output
7. additional supporting files only when needed

Add a context budget manager that tracks:

- file bytes loaded
- number of files loaded
- model token estimate
- shell output size

When the budget is exceeded, summarize earlier tool results and continue with summaries instead of raw content.

---

## 13. File editing model

Prefer patch-based editing over full file rewrites.

Recommended order:

1. read target file
2. generate unified diff or structured patch
3. validate patch against current file hash
4. preview diff in terminal
5. apply after approval
6. rerun validation command

Support three write strategies:

### Strategy A: exact patch

Best for deterministic small edits.

### Strategy B: section rewrite

Replace a known function or block using anchors.

### Strategy C: full rewrite

Use only when the file is small or the edit is large and approval is explicit.

---

## 14. Git integration

Git should be a first-class part of the architecture.

Required capabilities:

- show repo root
- show current branch
- show changed files
- diff unstaged and staged changes
- capture pre-edit snapshot
- optional temporary checkpoint commit

Recommended commands to wrap:

```bash
git status --short
git diff --unified=3
git diff --staged --unified=3
git rev-parse --show-toplevel
git branch --show-current
```

Nice-to-have:

- `/checkpoint` command that creates a local safety commit or patch bundle
- `/undo` based on stored patch history rather than git reset

---

## 15. Rust and Swift developer workflows

Design the tool around your actual workflows.

### 15.1 Rust workflow pack

Implement first-class helpers for:

- `cargo check`
- `cargo test`
- `cargo fmt --check`
- `cargo clippy`
- `cargo build`

Common agent pattern:

```text
run cargo test
-> inspect failures
-> read relevant modules
-> patch code
-> rerun cargo test
-> summarize changes
```

### 15.2 Swift workflow pack

Implement first-class helpers for:

- `swift build`
- `swift test`
- `swift package resolve`
- `swift format`
- project-specific commands if you use Vapor or grpc-swift

If you build server-side Swift, add optional helpers for:

- Vapor project detection
- protobuf generation commands
- package dependency diagnostics

### 15.3 Language-aware convenience commands

Examples:

- `agent fix --rust-tests`
- `agent fix --swift-build`
- `agent review --changed`
- `agent explain --file Sources/App/RedisListQueueListenerService.swift`

These are just presets that shape context collection and initial tool availability.

---

## 16. Persistence layout

Use a local state directory, for example:

```text
~/.config/grok-agent/
~/.local/share/grok-agent/
```

Suggested files:

```text
config.toml
sessions/<session-id>.jsonl
approvals.json
cache/
logs/
```

### Session record format

Store append-only JSON lines for:

- user message
- assistant partial output
- tool call
- tool result
- approval request
- approval decision
- file patch metadata
- final summary

This gives you easy replay and debugging.

---

## 17. Suggested implementation plan

### Phase 1: minimal interactive CLI

Deliver:

- prompt input
- xAI Responses API client
- streaming output
- `read_file`, `list_directory`, `search_text`
- one-shot ask mode

Success criteria:

- user can ask repo questions from terminal
- model can inspect a few files and answer accurately

### Phase 2: tool-calling runtime

Deliver:

- deterministic agent loop
- function tool dispatch
- `run_shell_command`
- `git_status`
- `git_diff`
- session persistence

Success criteria:

- model can inspect repo and run tests/builds safely

### Phase 3: patching and approvals

Deliver:

- diff preview renderer
- `apply_patch`
- write approvals
- undo support

Success criteria:

- model can propose and apply a small fix after approval

### Phase 4: language packs

Deliver:

- Rust preset commands
- Swift preset commands
- targeted diagnostics parsing

Success criteria:

- agent can fix common Rust and Swift compile/test failures end to end

### Phase 5: advanced ergonomics

Deliver:

- richer TUI
- resumable sessions
- better output truncation and summarization
- optional background indexing
- optional MCP integration

---

## 18. Recommended repository structure

```text
grok-agent/
  Cargo.toml
  src/
    main.rs
    cli/
      commands.rs
      tui.rs
      output.rs
    agent/
      runtime.rs
      session.rs
      policy.rs
      planner.rs
    provider/
      xai_client.rs
      stream_parser.rs
      models.rs
    tools/
      mod.rs
      registry.rs
      read_file.rs
      write_file.rs
      apply_patch.rs
      search_text.rs
      run_shell_command.rs
      git_status.rs
      git_diff.rs
      run_tests.rs
    adapters/
      filesystem.rs
      process_runner.rs
      git.rs
    config/
      config.rs
      secrets.rs
    persistence/
      session_store.rs
      patch_store.rs
      log_store.rs
```

---

## 19. Provider abstraction

Even if you start with xAI only, define a provider interface.

Example conceptual trait:

```rust
pub trait LanguageModelProvider {
    async fn create_response(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError>;
    async fn stream_response(&self, request: ModelRequest) -> Result<ModelStream, ProviderError>;
}
```

Why:

- easier testing with mocks
- easier failover later
- keeps agent runtime independent from xAI transport details

The xAI-specific implementation can live behind this interface.

---

## 20. Error handling and reliability

Required resilience features:

- retry transient HTTP failures
- handle rate limiting cleanly
- recover from partial stream interruption
- persist session state before risky actions
- surface concise failure messages to the terminal
- keep raw provider errors in debug logs

Recommended behavior:

- one automatic retry for idempotent read-only calls
- explicit user-visible error if a write-related action failed after model planning
- automatic fallback from streaming to synchronous request only when safe and clearly logged

---

## 21. Testing strategy

### Unit tests

- tool schema validation
- config parsing
- patch application logic
- policy enforcement
- stream parsing

### Integration tests

- mock xAI provider responses
- agent loop with scripted tool calls
- shell tool execution against a fixture repo
- git diff and patch workflows

### End-to-end tests

Fixture repositories:

- small Rust crate with failing tests
- small Swift package with build errors
- mixed repo with formatting and lint issues

Golden scenarios:

- explain failure
- patch one file
- rerun tests
- stop after success

---

## 22. Observability

The tool should make agent behavior obvious.

Expose:

- current mode
- model name
- request duration
- estimated token usage
- tool calls executed
- files read and written
- approval history
- final command/test status

Provide verbose logging behind a flag:

```bash
grok-agent --debug
```

---

## 23. Example user flows

### Flow A: fix failing Rust tests

```text
user: fix the failing tests
agent: runs cargo test
agent: reads failing modules
agent: proposes patch
user: approves
agent: applies patch
agent: reruns cargo test
agent: summarizes what changed
```

### Flow B: diagnose Swift build issue

```text
user: why does this package fail to build
agent: runs swift build
agent: reads the files referenced by compiler diagnostics
agent: explains root cause
agent: optionally proposes a fix
```

### Flow C: review current changes

```text
user: review my unstaged changes
agent: runs git diff
agent: reads touched files selectively
agent: returns review notes and risks without editing files
```

---

## 24. Initial implementation checklist

### Must-have

- [ ] Rust CLI project scaffold
- [ ] config loader
- [ ] `XAI_API_KEY` loading
- [ ] xAI Responses API client
- [ ] streaming support
- [ ] read-only tools
- [ ] agent loop with max-step limit
- [ ] shell command tool with approvals
- [ ] git status and diff tools
- [ ] patch proposal renderer
- [ ] patch apply tool
- [ ] session persistence

### Should-have

- [ ] Rust and Swift workflow presets
- [ ] undo support
- [ ] debug logging
- [ ] output truncation and summarization
- [ ] repo ignore support

### Nice-to-have

- [ ] ratatui interface
- [ ] MCP integration
- [ ] keychain-backed secret storage
- [ ] project indexing cache

---

## 25. Final recommendations

1. **Implement the CLI core in Rust.**
   It is the best fit for a portable terminal-native agentic tool.

2. **Use the xAI Responses API, not legacy chat completions.**
   It is the better foundation for multi-step agent behavior.

3. **Treat the model as planner, not executor.**
   Execution must happen only through explicit local tools.

4. **Make safety visible.**
   Users should always understand what the agent wants to do.

5. **Optimize for Rust and Swift workflows from day one.**
   Prebuilt commands and diagnostics parsing will make the tool immediately useful.

6. **Prefer patching over rewriting.**
   Small, auditable changes are much safer and easier to review.

7. **Persist everything important locally.**
   Sessions, tool calls, diffs, and approvals should be replayable.

---

## 26. Suggested next build order

If you want the fastest path to a usable prototype, build in this order:

1. Rust CLI scaffold
2. xAI Responses API streaming client
3. read-only repo tools
4. deterministic agent loop
5. shell command runner with approvals
6. git integration
7. patch preview and apply
8. Rust and Swift preset workflows
9. richer TUI

That sequence gets you to a practical terminal coding agent quickly, while keeping the foundation clean enough for later expansion.
