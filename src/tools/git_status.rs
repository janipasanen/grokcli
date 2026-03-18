use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct GitStatusResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(repo_root: &Path) -> Result<GitStatusResult> {
    let output = Command::new("git")
        .arg("status")
        .arg("--short")
        .current_dir(repo_root)
        .output()
        .context("failed to run git status --short")?;
    Ok(GitStatusResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
