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

    Ok(GitDiffResult {
        command,
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}
