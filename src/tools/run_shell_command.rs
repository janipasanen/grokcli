use crate::agent::policy::{CommandPolicyDecision, evaluate_command};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Clone, Deserialize)]
pub struct RunShellCommandArgs {
    pub command: String,
    pub working_directory: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunShellCommandResult {
    pub command: String,
    pub working_directory: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub approval_required: bool,
    pub blocked: bool,
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
                decision_reason: Some(reason),
                timeout_seconds,
            });
        }
        _ => {}
    }

    let started = Instant::now();
    let output = Command::new("zsh")
        .arg("-lc")
        .arg(&args.command)
        .current_dir(&cwd)
        .output()
        .with_context(|| format!("failed to run command '{}'", args.command))?;
    let duration_ms = started.elapsed().as_millis();

    Ok(RunShellCommandResult {
        command: args.command,
        working_directory: cwd_rel,
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        duration_ms,
        approval_required: false,
        blocked: false,
        decision_reason: None,
        timeout_seconds,
    })
}
