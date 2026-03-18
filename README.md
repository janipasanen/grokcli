# grokcli

Terminal-native Grok agent scaffold built around xAI's API.

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

Show help:

```bash
grokcli --help
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
