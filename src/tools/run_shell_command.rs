use crate::agent::policy::{CommandPolicyDecision, evaluate_command};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Clone, Deserialize)]
pub struct RunShellCommandArgs {
    pub command: String,
    pub working_directory: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunShellCommandResult {
    pub command: String,
    pub working_directory: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub approval_required: bool,
    pub blocked: bool,
    pub timed_out: bool,
    pub was_truncated: bool,
    pub decision_reason: Option<String>,
    pub timeout_seconds: u64,
}

pub fn run(repo_root: &Path, args: RunShellCommandArgs) -> Result<RunShellCommandResult> {
    let cwd_rel = args.working_directory.unwrap_or_else(|| ".".to_string());
    let cwd = repo_root.join(&cwd_rel);
    if !cwd.starts_with(repo_root) {
        bail!("run_shell_command working_directory escapes repo root");
    }

    let decision = evaluate_command(&args.command, &cwd, repo_root);
    let approved = args.approved.unwrap_or(false);
    let timeout_seconds = args.timeout_seconds.unwrap_or(60);

    match decision {
        CommandPolicyDecision::Block { reason } => {
            return Ok(RunShellCommandResult {
                command: args.command,
                working_directory: cwd_rel,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                approval_required: false,
                blocked: true,
                timed_out: false,
                was_truncated: false,
                decision_reason: Some(reason),
                timeout_seconds,
            });
        }
        CommandPolicyDecision::RequireApproval { reason } if !approved => {
            return Ok(RunShellCommandResult {
                command: args.command,
                working_directory: cwd_rel,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                approval_required: true,
                blocked: false,
                timed_out: false,
                was_truncated: false,
                decision_reason: Some(reason),
                timeout_seconds,
            });
        }
        _ => {}
    }

    let started = Instant::now();
    let mut child = Command::new("zsh")
        .arg("-lc")
        .arg(&args.command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run command '{}'", args.command))?;
    let timeout = Duration::from_secs(timeout_seconds);
    let poll_interval = Duration::from_millis(50);
    let mut timed_out = false;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        thread::sleep(poll_interval);
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed waiting for command '{}'", args.command))?;
    let duration_ms = started.elapsed().as_millis();
    let (stdout, stdout_truncated) = truncate_output(&String::from_utf8_lossy(&output.stdout));
    let (stderr, stderr_truncated) = truncate_output(&String::from_utf8_lossy(&output.stderr));
    let was_truncated = stdout_truncated || stderr_truncated;

    Ok(RunShellCommandResult {
        command: args.command,
        working_directory: cwd_rel,
        exit_code: output.status.code(),
        stdout,
        stderr,
        duration_ms,
        approval_required: false,
        blocked: false,
        timed_out,
        was_truncated,
        decision_reason: None,
        timeout_seconds,
    })
}

fn truncate_output(text: &str) -> (String, bool) {
    const MAX_BYTES: usize = 64 * 1024;
    if text.len() <= MAX_BYTES {
        return (text.to_string(), false);
    }
    let mut truncated = text[..MAX_BYTES].to_string();
    truncated.push_str("\n...[output truncated]...");
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::{RunShellCommandArgs, run};
    use std::env;

    #[test]
    fn run_shell_command_times_out() {
        let repo_root = env::current_dir().expect("cwd");
        let result = run(
            &repo_root,
            RunShellCommandArgs {
                command: "sleep 2".to_string(),
                working_directory: Some(".".to_string()),
                timeout_seconds: Some(1),
                approved: Some(true),
            },
        )
        .expect("run_shell_command");
        assert!(result.timed_out);
    }
}
