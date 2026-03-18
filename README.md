# grokcli

Terminal-native Grok agent scaffold built around xAI's API.

For repository-specific maintenance notes and agent workflow guidance, see `AGENTS.md`.

## Recommended model

For this CLI's client-side agentic workflow, use:

```bash
grok-code-fast-1
```

Other verified working choices:

- `grok-4-1-fast-reasoning`
- `grok-4-1-fast-non-reasoning`
- `grok-4.20-beta-0309-reasoning`
- `grok-4.20-beta-0309-non-reasoning`

Do not use `grok-4.20-multi-agent-beta-0309` with this CLI. That model is for xAI-managed multi-agent/server-side workflows and rejects this tool's client-side function-calling architecture.

## Build (macOS)

```bash
cargo build --release
```

The binary is created at:

```
target/release/grokcli
```

## Install as a CLI (macOS)

Option A: Install to `/usr/local/bin`:

```bash
sudo install -m 0755 target/release/grokcli /usr/local/bin/grokcli
```

Option B: Install to `/opt/homebrew/bin` (Apple Silicon Homebrew default):

```bash
sudo install -m 0755 target/release/grokcli /opt/homebrew/bin/grokcli
```

Option C: Install to `~/.local/bin` (no sudo) and add to PATH:

```bash
mkdir -p ~/.local/bin
install -m 0755 target/release/grokcli ~/.local/bin/grokcli
```

Add to your shell config if needed:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

## Usage

Set your API key:

```bash
export XAI_API_KEY="your-key"
```

Run a prompt:

```bash
grokcli "explain this repo"
```

Run the interactive shell:

```bash
grokcli
```

Show help:

```bash
grokcli --help
```

Show the extended help sections:

```bash
grokcli --show-help-sections
```

### Interactive shell

Starting `grokcli` with no prompt opens a text-entry shell. In the TTY shell, pressing Enter queues the prompt and starts it immediately in the background so you can keep typing follow-up prompts. Type `/` or `/help` to see the available slash commands.

In a normal terminal TTY, the shell supports command history with the Up and Down arrow keys. History is stored at `~/.local/share/grok-agent/history.txt`, so previously entered prompts and slash commands are available across shell sessions.

In the same TTY mode, you can queue follow-up tasks without running them immediately. Use `/queue <text>` to add a task to the FIFO queue, or type a prompt and press `Tab` or `Ctrl+Q` to enqueue the current line. Queued tasks run sequentially in the background, and you can start queued-only work explicitly with `/queue-run`.

For Rust and Swift repositories, `agent` and `edit` mode now auto-infer a repair workflow when your prompt clearly describes a build or test failure. That means prompts like "swift test fails" or "cargo test fails" will automatically bias the agent toward `run_tests`, `build_project`, focused file reads, `apply_patch`, and verification reruns even if you do not manually set a preset.

Common interactive commands:

- `/show` prints the current mode, model, API mode, step limit, approval mode, preset, and context budget.
- `/approve <yes|no>` answers an approval request from a queued task.
- `/mode <ask|edit|agent>` switches between single-turn and multi-step behavior.
- `/model <name>` changes the Grok model for the current shell session.
- `/api-mode <responses|chat-completions>` switches the provider path.
- `/max-steps <n>` changes the agent loop limit for `edit` and `agent` mode.
- `/auto-approve <on|off>` toggles approval bypass.
- `/verbose-tools <on|off>` toggles detailed tool previews. Concise tool action/result lines still print either way.
- `/stream <on|off>` toggles streaming output.
- `/preset <rust-tests|swift-build|review-changed|off>` applies or clears a workflow preset.
- `/queue <text>` adds a prompt to the queue without running it.
- `/queue-show` lists queued tasks.
- `/queue-run` runs queued tasks now.
- `/queue-clear` clears queued tasks.
- `/resume-latest`, `/resume <id|path>`, and `/new-session` manage session continuity.
- `/exit` leaves the shell.

Accepted command aliases in the shell:

- `/max_steps <n>` works the same as `/max-steps <n>`.
- `/auto_approve <on|off>` works the same as `/auto-approve <on|off>`.
- `/api_mode <responses|chat-completions>` works the same as `/api-mode ...`.
- `/verbose_tools <on|off>` works the same as `/verbose-tools <on|off>`.
- `/queue_show`, `/queue_run`, and `/queue_clear` work the same as their dashed forms.

If the agent wants to run `swift test`, `swift build`, `cargo test`, `cargo build`, or apply a patch, it will ask for approval unless you start the CLI with `--auto-approve` or toggle `/auto-approve on`.

Example shell session:

```text
$ grokcli
grok> /mode agent
grok> /model grok-code-fast-1
grok> inspect this repo and tell me why the build fails
queued[1]: inspect this repo and tell me why the build fails
grok> /queue inspect migration failures
queued[2]: inspect migration failures
grok> /queue verify all checkout tests
queued[3]: verify all checkout tests
running queued[1] remaining=2 inspect this repo and tell me why the build fails
tool> run_tests language="rust" command="cargo test"
grok> /show
grok> /exit
```

### General usage

Common options:

- `--mode ask|edit|agent` controls how aggressive the loop is.
- `--max-steps N` caps the tool loop steps.
- `--auto-approve` skips interactive approval prompts.
- `--verbose-tools` enables detailed tool previews, while `--no-verbose-tools` keeps concise tool action/result lines only.
- `--queue <prompt>` appends a follow-up prompt to run in the same non-interactive session. Pass it more than once to batch multiple prompts.
- `--resume-latest` or `--resume-session <id|path>` continues an earlier session.
- `--context-budget-bytes N` limits tool output passed back to the model.

The CLI always prints short `tool>` / `tool<` lines for each tool action and result. With `verbose_tools` enabled it also prints detailed previews such as file contents, directory listings, and labeled `stdout` / `stderr` blocks.

Examples:

```bash
# Ask mode, single-turn
grokcli --mode ask "summarize the architecture"

# Agent mode with more steps
grokcli --mode agent --max-steps 12 "fix failing tests"

# Agent mode without live tool chatter
grokcli --mode agent --no-verbose-tools "fix failing tests"

# Batch multiple prompts in one session
grokcli --mode agent --queue "inspect build logs" --queue "rerun checkout tests" "fix failing tests"

# Use a preset workflow
grokcli --preset rust-tests "fix the failing tests"

# Resume the latest session
grokcli --resume-latest "continue"

# Undo the most recent recorded patch
grokcli --undo-last-patch
```

## Development

Run the test suite:

```bash
cargo test -q
```

Build a debug binary:

```bash
cargo build
```

Build and reinstall the release binary after CLI/runtime changes:

```bash
cargo build --release
sudo install -m 0755 target/release/grokcli /usr/local/bin/grokcli
hash -r
```

## Configuration and state

- Config file path: `~/.config/grok-agent/config.toml`
- Session logs: `~/.local/share/grok-agent/sessions/*.jsonl`
- Shell history: `~/.local/share/grok-agent/history.txt`
- Recorded patch history: `~/.local/share/grok-agent/patches.jsonl`

## Notes

- By default, the runtime uses the Responses API with streaming if available and falls back to chat-completions.
- Session logs are stored at `~/.local/share/grok-agent/sessions`.
