use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct GitStatusResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub was_truncated: bool,
}

pub fn run(repo_root: &Path) -> Result<GitStatusResult> {
    let output = Command::new("git")
        .arg("status")
        .arg("--short")
        .current_dir(repo_root)
        .output()
        .context("failed to run git status --short")?;
    let (stdout, stdout_truncated) = truncate_output(&String::from_utf8_lossy(&output.stdout));
    let (stderr, stderr_truncated) = truncate_output(&String::from_utf8_lossy(&output.stderr));
    Ok(GitStatusResult {
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
