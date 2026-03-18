# grokcli

Terminal-native Grok agent scaffold built around xAI's API.

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

### Interactive shell

Starting `grokcli` with no prompt opens a text-entry shell. Type a normal prompt and press Enter to run it. Type `/` or `/help` to see the available slash commands.

For Rust and Swift repositories, `agent` and `edit` mode now auto-infer a repair workflow when your prompt clearly describes a build or test failure. That means prompts like "swift test fails" or "cargo test fails" will automatically bias the agent toward `run_tests`, `build_project`, focused file reads, `apply_patch`, and verification reruns even if you do not manually set a preset.

Common interactive commands:

- `/show` prints the current mode, model, API mode, step limit, approval mode, preset, and context budget.
- `/mode <ask|edit|agent>` switches between single-turn and multi-step behavior.
- `/model <name>` changes the Grok model for the current shell session.
- `/api-mode <responses|chat-completions>` switches the provider path.
- `/max-steps <n>` changes the agent loop limit for `edit` and `agent` mode.
- `/auto-approve <on|off>` toggles approval bypass.
- `/stream <on|off>` toggles streaming output.
- `/preset <rust-tests|swift-build|review-changed|off>` applies or clears a workflow preset.
- `/resume-latest`, `/resume <id|path>`, and `/new-session` manage session continuity.
- `/exit` leaves the shell.

If the agent wants to run `swift test`, `swift build`, `cargo test`, `cargo build`, or apply a patch, it will ask for approval unless you start the CLI with `--auto-approve` or toggle `/auto-approve on`.

Example shell session:

```text
$ grokcli
grok> /mode agent
grok> /model grok-code-fast-1
grok> inspect this repo and tell me why the build fails
grok> /show
grok> /exit
```

### General usage

Common options:

- `--mode ask|edit|agent` controls how aggressive the loop is.
- `--max-steps N` caps the tool loop steps.
- `--auto-approve` skips interactive approval prompts.
- `--resume-latest` or `--resume-session <id|path>` continues an earlier session.
- `--context-budget-bytes N` limits tool output passed back to the model.

Examples:

```bash
# Ask mode, single-turn
grokcli --mode ask "summarize the architecture"

# Agent mode with more steps
grokcli --mode agent --max-steps 12 "fix failing tests"

# Use a preset workflow
grokcli --preset rust-tests "fix the failing tests"

# Resume the latest session
grokcli --resume-latest "continue"

# Undo the most recent recorded patch
grokcli --undo-last-patch
```

## Notes

- By default, the runtime uses the Responses API with streaming if available and falls back to chat-completions.
- Session logs are stored at `~/.local/share/grok-agent/sessions`.
