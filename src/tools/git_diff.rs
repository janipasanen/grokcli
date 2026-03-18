use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Deserialize)]
pub struct GitDiffArgs {
    pub staged: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitDiffResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub was_truncated: bool,
}

pub fn run(repo_root: &Path, args: GitDiffArgs) -> Result<GitDiffResult> {
    let staged = args.staged.unwrap_or(false);
    let mut cmd = Command::new("git");
    cmd.arg("diff");
    if staged {
        cmd.arg("--staged");
    }
    cmd.arg("--unified=3").current_dir(repo_root);

    let output = cmd.output().context("failed to run git diff")?;
    let command = if staged {
        "git diff --staged --unified=3".to_string()
    } else {
        "git diff --unified=3".to_string()
    };

    let (stdout, stdout_truncated) = truncate_output(&String::from_utf8_lossy(&output.stdout));
    let (stderr, stderr_truncated) = truncate_output(&String::from_utf8_lossy(&output.stderr));
    Ok(GitDiffResult {
        command,
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        was_truncated: stdout_truncated || stderr_truncated,
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
