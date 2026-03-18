# AGENTS.md

This file contains repository-specific guidance for coding agents and maintainers working in `grokcli`.

## Purpose

`grokcli` is a Rust-based terminal agent that uses xAI models as the planner and executes tools locally. The main goals of the repo are:

- keep the CLI usable as a Codex-style terminal agent
- keep local tool execution explicit and auditable
- preserve approval-gated behavior for risky actions
- keep xAI model and endpoint behavior configurable

## Read first

Before changing architecture-level behavior, read:

- `grok-agentic-dev-tool-architecture.md`
- `README.md`
- `IMPLEMENTATION_STATUS.md`

If you add or materially change user-visible behavior, update `README.md`. If you add a meaningful implementation milestone, update `IMPLEMENTATION_STATUS.md`.

## Build and test

Primary commands:

```bash
cargo test -q
cargo build
cargo build --release
```

Reinstall the CLI after changing runtime or shell behavior:

```bash
sudo install -m 0755 target/release/grokcli /usr/local/bin/grokcli
hash -r
```

## Important files

- `src/main.rs`: CLI flags, interactive shell, slash commands, queueing, history
- `src/agent/runtime.rs`: agent loop, approvals, tool tracing, verification handling
- `src/agent/instructions.rs`: system/runtime instructions sent to the model
- `src/provider/xai_client.rs`: xAI transport, Responses/chat-completions handling, streaming
- `src/config/config.rs`: app config defaults and config-file loading
- `src/tools/registry.rs`: tool registration and published tool schemas
- `tests/runtime_loop.rs`: integration coverage for provider/runtime/tool-call flows

## Current runtime expectations

- Preferred model: `grok-code-fast-1`
- Supported client-side tool models also include:
  - `grok-4-1-fast-reasoning`
  - `grok-4-1-fast-non-reasoning`
  - `grok-4.20-beta-0309-reasoning`
  - `grok-4.20-beta-0309-non-reasoning`
- Do not target `grok-4.20-multi-agent-beta-0309` for this CLI architecture. It is rejected intentionally.
- Multi-step Responses mode must use stored responses so `previous_response_id` continues correctly.

## Interactive shell expectations

No-argument `grokcli` starts the shell.

Current shell behavior includes:

- Up/Down history in TTY mode
- Tab or Ctrl+Q to queue the current line
- `/queue`, `/queue-show`, `/queue-run`, `/queue-clear`
- `/verbose-tools on|off`
- dashed and underscore aliases for several slash commands

If you change shell behavior, update:

- `README.md`
- help text in `src/main.rs`
- parser tests in `src/main.rs`

## Tooling and editing rules

- Prefer first-class tools over generic shell commands when implementing agent behavior.
- Keep shell-based in-place edits blocked; the runtime should prefer `apply_patch`.
- Keep tool traces and tool output rendering behind `verbose_tools` so the shell can be quiet when requested.
- Do not remove approval gates for risky commands without a clear reason and matching documentation/tests.

## State locations

- Config: `~/.config/grok-agent/config.toml`
- Sessions: `~/.local/share/grok-agent/sessions`
- History: `~/.local/share/grok-agent/history.txt`
- Patch history: `~/.local/share/grok-agent/patches.jsonl`

## When changing behavior

- Add or update tests for parser, runtime, or provider changes.
- Keep user-facing output concise and readable in terminal mode.
- Rebuild and reinstall the binary before manually validating installed CLI behavior.
